use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::time::{Duration, Instant};

use ffmpeg_next::{Packet, format, frame};

use crate::audio::{BITRATE, Mixer, Sound, keep};
use crate::budget as resources;
use crate::compose::{Canvas, Place, Setup, Source};
use crate::config::{self, Audio, Config, Target};
use crate::copy::copy;
use crate::decode::{Meta, Probe, absolute, damage, probe};
use crate::dedup::{differences, threshold};
use crate::encode::{Encoder, Plan};
use crate::graph::{Graph, Plan as Shape, lossless};
use crate::munch::Muncher;
use crate::segments::Made;
use crate::timeline::{Fade, Part, Placed, parts};
use crate::upscale::Upscaler;
use crate::work::{self, Cancel};
use crate::{Event, Result, Sink};

const MEGABYTE: f64 = 1e6;
// the encoders miss a bit rate by a few percent, and the container takes some bytes too
const MARGIN: f64 = 0.95;

#[derive(Clone, PartialEq)]
pub struct Job {
    pub clip: PathBuf,
    pub config: Config,
    pub cut: Option<(f64, f64)>,
    pub muted: Vec<usize>,
    // the streams are copied, whatever the config says
    pub copy: bool,
    // the place on the timeline, for a render into one file
    pub at: f64,
    pub track: usize,
    pub place: Place,
    pub fade: Fade,
    // the seconds that the last picture stays after the end
    pub hold: f64,
    pub speed: f64,
    pub folder: Option<PathBuf>,
}

fn target(job: &Job, extension: &str) -> PathBuf {
    let stem = job.clip.file_stem().unwrap_or_default().to_string_lossy();
    let named = |name: String| match &job.folder {
        Some(folder) => folder.join(name),
        None => job.clip.with_file_name(name),
    };
    if let Some(folder) = &job.folder {
        let _ = std::fs::create_dir_all(folder);
    }
    (1..)
        .map(|attempt| match attempt {
            1 => named(format!("{stem} - interpolini.{extension}")),
            _ => named(format!("{stem} - interpolini ({attempt}).{extension}")),
        })
        .find(|path| !path.exists())
        .unwrap_or_default()
}

fn named(job: &Job) -> Result<Probe> {
    let name = job.clip.file_name().unwrap_or_default().to_string_lossy();
    probe(&job.clip).map_err(|error| format!("{name}: {error}").into())
}

pub fn sequence(jobs: &mut [Job]) -> Result<()> {
    let mut at = 0.0;
    for job in jobs {
        (job.at, job.track) = (at, 0);
        at += kept(job, &named(job)?);
    }
    Ok(())
}

pub(crate) fn kept(job: &Job, probe: &Probe) -> f64 {
    let length = match job.cut {
        // an image is as long as its clip
        Some((start, end)) if probe.still => end - start,
        Some((start, end)) => end.min(probe.seconds) - start,
        None => probe.seconds,
    };
    // a cut that starts after the end of the clip keeps nothing, and holds nothing
    match length > 0.0 {
        true => length / job.speed + job.hold,
        false => 0.0,
    }
}

pub fn sounds(jobs: &[Job]) -> Result<Vec<Sound>> {
    let mut sounds = Vec::new();
    for job in jobs {
        let probe = named(job)?;
        if kept(job, &probe) <= 0.0 {
            continue;
        }
        for stream in (0..probe.tracks.len()).filter(|track| !job.muted.contains(track)) {
            sounds.push(Sound {
                clip: job.clip.clone(),
                stream,
                title: probe.tracks[stream].clone(),
                start: job.cut.map_or(0.0, |cut| cut.0),
                length: kept(job, &probe),
                at: job.at,
                track: stream,
                speed: job.speed,
                gain: 1.0,
                fade: Fade::default(),
            });
        }
    }
    Ok(sounds)
}

fn base(jobs: &[(usize, &Job)]) -> Result<usize> {
    let mut videos = Vec::new();
    for (index, (_, job)) in jobs.iter().enumerate() {
        let probe = probe(&job.clip)?;
        if probe.fps > 0.0 && kept(job, &probe) > 0.0 {
            videos.push(index);
        }
    }
    let order = |index: &usize| (jobs[*index].1.at, jobs[*index].1.track);
    let first = videos.into_iter().min_by(|a, b| {
        let (a, b) = (order(a), order(b));
        a.0.total_cmp(&b.0).then(a.1.cmp(&b.1))
    });
    first.ok_or_else(|| "the timeline has no video".into())
}

fn budget(megabytes: f64, seconds: f64, lanes: usize) -> Result<usize> {
    let sound = (lanes * BITRATE) as f64 * seconds;
    let rate = (megabytes * MEGABYTE * 8.0 - sound) / seconds;
    match rate >= 50_000.0 {
        true => Ok(rate as usize),
        false => Err("the size is too small for this clip".into()),
    }
}

fn cover(seconds: f64, rate: f64) -> i32 {
    let frames = seconds * rate;
    let near = frames.round();
    match (frames - near).abs() < 1e-6 {
        true => near as i32,
        false => frames.ceil() as i32,
    }
}

// ntsc rates divide by 1001
pub(crate) fn rational(fps: f64) -> (i64, i64) {
    let ntsc = [24, 30, 60, 120]
        .into_iter()
        .find(|rate| (fps - f64::from(*rate) * 1000.0 / 1001.0).abs() < 0.01);
    match ntsc {
        Some(rate) => (i64::from(rate) * 1000, 1001),
        None => ((fps * 1000.0).round() as i64, 1000),
    }
}

fn encode(
    jobs: &[(usize, &Job)],
    timeline: Option<&[Sound]>,
    setup: &Setup,
    output: &Path,
    share: f64,
    sink: Sink,
    cancel: &Cancel,
) -> Result<()> {
    let mut most = 0;
    let mut silenced = Vec::new();
    let mut placed = Vec::new();
    let mut probes = Vec::new();
    for (_, job) in jobs {
        let probe = probe(&job.clip)?;
        let heard: Vec<usize> = (0..probe.tracks.len())
            .filter(|track| !job.muted.contains(track))
            .collect();
        // a track that repeats the others does not play when mixing
        let mixed = timeline.is_none() && job.config.output.audio == Audio::Mix;
        let heard = if mixed {
            keep(&job.clip, &heard)
        } else {
            heard
        };
        most = most.max(heard.len());
        silenced.push(match mixed {
            true => (0..probe.tracks.len())
                .filter(|track| !heard.contains(track))
                .collect(),
            false => job.muted.clone(),
        });
        placed.push(Placed {
            at: job.at,
            length: kept(job, &probe),
            track: job.track,
        });
        probes.push(probe);
    }
    let parts = match timeline {
        None => vec![Part {
            start: 0.0,
            end: placed[0].length,
            clips: vec![0],
        }],
        Some(_) => parts(&placed),
    };
    let open = |clip: usize| -> Result<Source> {
        let (index, job) = jobs[clip];
        if timeline.is_some() {
            sink(Event::Start { clip: index });
        }
        let mut config = job.config.retimed(job.speed);
        if config.dedup.enabled && config.dedup.threshold == 0.0 && !probes[clip].still {
            let (from, to) = job.cut.unwrap_or((0.0, f64::MAX));
            let share = config.dedup.repeats / 100.0;
            config.dedup.threshold = threshold(&differences(&job.clip, from, to, cancel)?, share);
        }
        let source = Source::open(&job.clip, &config, probes[clip].still, sink, cancel)?;
        if let (Some((_, end)), Some(graph)) = (job.cut, source.graph()) {
            graph.stop(end);
        }
        Ok(source)
    };
    let base = base(jobs)?;
    let first = open(base)?;
    let graph = first.graph().ok_or("the timeline has no video")?;
    let (mut fps, mut meta) = (graph.info.fps, graph.meta);
    let (job, fps_in) = (jobs[base].1, probes[base].fps);
    if job.speed != 1.0 {
        fps = Shape::new(fps_in, &job.config).output(rational(fps_in), &job.config);
    }
    if let Some(size) = setup.size {
        (meta.width, meta.height) = size;
    }
    if let Some(rate) = setup.fps {
        fps = rational(rate);
    }
    if let Some(rate) = setup.munch.and_then(|munch| munch.fps) {
        fps = (i64::from(rate), 1);
    }
    // one video in 4:4:4 makes the whole file 4:4:4, an image follows the videos
    meta.matrix = meta.matrix.filter(|_| timeline.is_none());
    meta.yuv444 = probes.iter().any(|probe| probe.yuv444 && !probe.still);
    let rate = fps.0 as f64 / fps.1 as f64;
    let end = parts.last().map_or(0.0, |part| part.end);
    let frames = cover(end, rate);
    if frames <= 0 {
        return Err("the cut has no picture".into());
    }

    let config = &job.config.output;
    let start = job.cut.map_or(0.0, |cut| cut.0);
    let (mix, sized) = (config.audio == Audio::Mix, config.target == Target::Size);
    let plan = match timeline {
        Some(sounds) => Plan::Mix(Mixer::new(sounds, mix)),
        None if job.speed != 1.0 || setup.munch.is_some() => {
            Plan::Mix(Mixer::new(&sounds(std::slice::from_ref(job))?, mix))
        }
        // sound that is copied has no known size
        None if sized || (mix && most > 1) => Plan::Encode {
            titles: match mix {
                true => vec![String::new(); most.min(1)],
                false => (probes[base].tracks.iter().enumerate())
                    .filter(|(track, _)| !silenced[base].contains(track))
                    .map(|(_, title)| title.clone())
                    .collect(),
            },
        },
        None => Plan::Copy {
            clip: &job.clip,
            start,
            muted: &silenced[base],
        },
    };
    let lanes = match &plan {
        Plan::Mix(mixer) => mixer.lanes(),
        Plan::Encode { titles } => titles.len(),
        Plan::Copy { .. } => 0,
    };
    let megabytes = f64::from(config.size) * MARGIN * share;
    let bits = match sized {
        true => Some(budget(megabytes, f64::from(frames) / rate, lanes)?),
        false => None,
    };
    let upscaler = Upscaler::new(&meta, &job.config.upscale);
    if let Some(upscaler) = &upscaler {
        let Meta { width, height, .. } = upscaler.meta;
        let method = job.config.upscale.method.name();
        sink(Event::Info(format!(
            "upscaling to {width}x{height}, {method}"
        )));
    }
    let encoded = upscaler.as_ref().map_or(meta, |upscaler| upscaler.meta);
    let mut muncher = setup
        .munch
        .map(|munch| Muncher::new(&encoded, &munch))
        .transpose()?;
    let encoded = muncher.as_ref().map_or(encoded, |muncher| muncher.meta);
    let changed = upscaler.is_some() || muncher.is_some();
    let packed = packs(job, graph, timeline.is_some(), changed);
    let mut encoder = Encoder::open(
        format::output(&absolute(output))?,
        &encoded,
        fps,
        config,
        bits,
        plan,
        packed,
    )?;
    encoder.munch = setup.munch;
    let fallback = if meta.yuv444 && !encoder.yuv444 {
        " in 4:2:0, it has no 4:4:4"
    } else {
        ""
    };
    sink(Event::Info(format!(
        "encoding with {}{fallback}",
        encoder.name
    )));
    let from = ((start - job.at) * rate).round().max(0.0) as i32;
    let fused = packed
        && encoder
            .spare()
            .is_some_and(|spare| graph.encode(spare, from, from + frames));
    let frames = match fused {
        true => frames.min(graph.info.frames - from),
        false => frames,
    };
    if timeline.is_none() {
        encoder.begin(&job.clip, start, &silenced[base])?;
    }
    let mut canvas = Canvas::new(meta, setup.background);
    let size = canvas.size();
    let mut sources = BTreeMap::from([(base, first)]);

    enum Encoded {
        Picture(frame::Video, i32),
        Packets(Vec<Packet>),
    }

    let luma = encoded.width as usize * encoded.height as usize;
    let frame_bytes = luma + 2 * (luma >> (encoded.shift() * 2));
    let room = (resources::bytes(resources::current()) / 16 / frame_bytes.max(1)).clamp(4, 8);
    let (tx, rx) = work::channel::<Encoded>(room);
    let abort = Arc::new(AtomicBool::new(false));
    let done = Arc::new(AtomicI32::new(0));
    let enc_abort = Arc::clone(&abort);
    let enc_done = Arc::clone(&done);
    let enc_cancel = cancel.clone();
    let writer = work::spawn("encoder", move || -> Result<()> {
        let mut count = 0;
        let hold = Cancel::default();
        while let Some(item) = rx.recv(&hold)? {
            if enc_abort.load(Ordering::Acquire) {
                return Ok(());
            }
            if enc_cancel.stopped() {
                break;
            }
            match item {
                Encoded::Packets(packets) => encoder.pass(packets)?,
                Encoded::Picture(picture, n) => encoder.write(picture, n)?,
            }
            count += 1;
            enc_done.store(count, Ordering::Release);
        }
        if enc_abort.load(Ordering::Acquire) {
            return Ok(());
        }
        match count {
            0 => Err("stopped".into()),
            _ => encoder.finish(count),
        }
    });

    let started = Instant::now();
    let mut reported = started;
    let mut written = 0;
    let produced: Result<()> = (|| {
        'parts: for (index, part) in parts.iter().enumerate() {
            if cancel.stopped() {
                break;
            }
            sources.retain(|clip, _| placed[*clip].at + placed[*clip].length > part.start + 0.001);
            let fills = |clip: &usize| {
                let size = (size, (probes[*clip].width, probes[*clip].height));
                !probes[*clip].still && jobs[*clip].1.place.covers(size.0, size.1)
            };
            let solid = |clip: &usize| fills(clip) && jobs[*clip].1.fade.none();
            let shown = &part.clips[part.clips.iter().rposition(solid).unwrap_or(0)..];
            for clip in shown {
                if !sources.contains_key(clip) {
                    let source = match open(*clip) {
                        Err(_) if cancel.stopped() => break 'parts,
                        source => source?,
                    };
                    sources.insert(*clip, source);
                }
            }
            let last = match index + 1 == parts.len() {
                true => frames,
                false => ((part.end * rate).round() as i32).min(frames),
            };
            'frames: while written < last && !cancel.stopped() {
                let made = match sources.get_mut(&base).filter(|_| fused) {
                    Some(source) => match source.made(from + written, cancel) {
                        Err(_) if cancel.stopped() => break 'frames,
                        made => Some(made?),
                    },
                    None => None,
                };
                let item = match made {
                    Some(Made::Packets(packets)) => Encoded::Packets(packets),
                    Some(Made::Picture(picture)) => Encoded::Picture(picture, written),
                    None => {
                        let time = f64::from(written) / rate;
                        let mut layers = Vec::new();
                        let level = |clip: &usize| {
                            let Placed { at, length, .. } = placed[*clip];
                            jobs[*clip].1.fade.level(time - at, length)
                        };
                        let top = shown
                            .iter()
                            .rposition(|clip| fills(clip) && level(clip) >= 1.0);
                        for clip in &shown[top.unwrap_or(0)..] {
                            let job = jobs[*clip].1;
                            let inside =
                                job.cut.map_or(0.0, |cut| cut.0) + (time - job.at) * job.speed;
                            if let Some(source) = sources.get_mut(clip) {
                                let picture = match source.at(inside, cancel) {
                                    Err(_) if cancel.stopped() => break 'frames,
                                    picture => picture?,
                                };
                                layers.push((picture, job.place, level(clip) as f32));
                            }
                        }
                        let picture = canvas.compose(layers)?;
                        let picture = match &upscaler {
                            Some(upscaler) => upscaler.run(&picture)?,
                            None => picture,
                        };
                        let picture = match &mut muncher {
                            Some(muncher) => muncher.run(&picture)?,
                            None => picture,
                        };
                        Encoded::Picture(picture, written)
                    }
                };
                if tx.send_cancel(item, cancel).is_err() {
                    break 'parts;
                }
                written += 1;
                if reported.elapsed() >= Duration::from_millis(100) {
                    reported = Instant::now();
                    let shown = done.load(Ordering::Relaxed).max(1);
                    sink(Event::Progress {
                        frame: shown as u32,
                        frames: frames as u32,
                        fps: shown as f32 / started.elapsed().as_secs_f32(),
                    });
                }
            }
        }
        Ok(())
    })();
    if produced.is_err() {
        abort.store(true, Ordering::Release);
    }
    drop(tx);
    let finished = writer
        .join()
        .unwrap_or_else(|_| Err("the encoder stopped".into()));
    produced?;
    let written = done.load(Ordering::Acquire);
    if written == frames {
        sink(Event::Progress {
            frame: written as u32,
            frames: frames as u32,
            fps: written as f32 / started.elapsed().as_secs_f32(),
        });
    }
    for source in sources.values() {
        let lanes = source.graph().and_then(|graph| graph.lanes());
        if let Some(lanes) = lanes.filter(|lanes| *lanes > 1) {
            sink(Event::Info(format!("decoders: {lanes}")));
        }
    }
    for (_, job) in jobs {
        if let Some((from, to)) = damage(&job.clip) {
            sink(Event::Warning(format!(
                "damaged video from {} to {}",
                clock(from),
                clock(to)
            )));
        }
    }
    if written < frames && finished.is_ok() {
        sink(Event::Warning(format!(
            "stopped at frame {written} of {frames}"
        )));
    }
    finished
}

fn fit(
    jobs: &[(usize, &Job)],
    timeline: Option<&[Sound]>,
    setup: &Setup,
    output: &Path,
    sink: Sink,
    cancel: &Cancel,
) -> Result<()> {
    let config = &jobs[base(jobs)?].1.config.output;
    let limit = f64::from(config.size) * MEGABYTE;
    let software = jobs.iter().map(|(_, job)| {
        let mut job = (*job).clone();
        job.config.output.encoder = config::Encoder::Software;
        job
    });
    let software: Vec<Job> = software.collect();
    let software: Vec<(usize, &Job)> = jobs.iter().map(|job| job.0).zip(&software).collect();
    let mut share = 1.0;
    for attempt in 0.. {
        let jobs = if attempt < 2 { jobs } else { &software };
        encode(jobs, timeline, setup, output, share, sink, cancel)?;
        let size = std::fs::metadata(output)?.len() as f64;
        if config.target == Target::Quality || size <= limit || cancel.stopped() {
            break;
        }
        if attempt == 2 {
            sink(Event::Warning(format!("still over {} MB", config.size)));
            break;
        }
        share = if attempt == 0 {
            limit / size * MARGIN
        } else {
            1.0
        };
        sink(Event::Warning(format!(
            "over {} MB, rendering again",
            config.size
        )));
    }
    Ok(())
}

fn clock(seconds: f64) -> String {
    let seconds = seconds as u64;
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

fn render(
    job: &Job,
    clip: usize,
    setup: &Setup,
    output: &Path,
    sink: Sink,
    cancel: &Cancel,
) -> Result<()> {
    // a file of its own starts with the clip, not at its place on the timeline
    let job = &Job {
        at: 0.0,
        ..job.clone()
    };
    let untouched = job.place == Place::default();
    let plain = untouched && job.speed == 1.0 && setup.munch.is_none();
    if job.copy || (plain && lossless(probe(&job.clip)?.fps, &job.config)) {
        return copy(job, output, sink, cancel);
    }
    fit(
        &[(clip, job)],
        None,
        &Setup {
            munch: setup.munch,
            ..Setup::default()
        },
        output,
        sink,
        cancel,
    )
}

// a clip on its own with blended frames can go to both encoders of the card at once
pub(crate) fn packs(job: &Job, graph: &Graph, joined: bool, changed: bool) -> bool {
    let alone = !joined && !changed && job.fade.none() && job.speed == 1.0;
    alone && job.place == Place::default() && graph.packs()
}

pub fn run(jobs: &[Job], timeline: Option<&[Sound]>, setup: &Setup, sink: Sink, cancel: &Cancel) {
    let _svp = jobs
        .iter()
        .any(|job| {
            !job.copy
                && (job.config.interpolation.enabled
                    || job.config.pre.enabled
                    || job.config.dedup.enabled)
        })
        .then(|| crate::svp::Svp::load(&config::app_dir()).ok())
        .flatten();
    match timeline.filter(|_| !jobs.is_empty()) {
        Some(sounds) => joined(jobs, sounds, setup, sink, cancel),
        None => separate(jobs, setup, sink, cancel),
    }
    crate::rife::rest();
}

// with join, all clips go into one file in their order
fn joined(jobs: &[Job], sounds: &[Sound], setup: &Setup, sink: Sink, cancel: &Cancel) {
    let started = Instant::now();
    let all: Vec<(usize, &Job)> = jobs.iter().enumerate().collect();
    let named = &jobs[base(&all).unwrap_or(0)];
    let output = target(named, named.config.output.container.name());
    let _partial = work::partial(&output);
    match fit(&all, Some(sounds), setup, &output, sink, cancel) {
        Ok(()) => {
            let seconds = started.elapsed().as_secs_f32();
            for clip in 0..jobs.len() {
                let output = output.clone();
                sink(Event::Done {
                    clip,
                    output,
                    seconds,
                });
            }
        }
        Err(error) => {
            let _ = std::fs::remove_file(&output);
            match cancel.stopped() {
                true => sink(Event::Warning("stopped".into())),
                false => sink(Event::Failed {
                    clip: 0,
                    error: error.to_string(),
                }),
            }
        }
    }
}

fn separate(jobs: &[Job], setup: &Setup, sink: Sink, cancel: &Cancel) {
    for (clip, job) in jobs.iter().enumerate() {
        if cancel.stopped() {
            break;
        }
        sink(Event::Start { clip });
        let started = Instant::now();
        // a copy stays in the container of its source, which can hold its streams
        let source = job.clip.extension().unwrap_or_default().to_string_lossy();
        let output = match job.copy {
            true => target(job, &source),
            false => target(job, job.config.output.container.name()),
        };
        let _partial = work::partial(&output);
        match render(job, clip, setup, &output, sink, cancel) {
            Ok(()) => {
                let seconds = started.elapsed().as_secs_f32();
                sink(Event::Done {
                    clip,
                    output,
                    seconds,
                });
            }
            Err(error) => {
                let _ = std::fs::remove_file(&output);
                match cancel.stopped() {
                    true => sink(Event::Warning("stopped".into())),
                    false => sink(Event::Failed {
                        clip,
                        error: error.to_string(),
                    }),
                }
            }
        }
    }
}
