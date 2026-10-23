use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use ffmpeg_next::format;

use crate::audio::{BITRATE, Mixer, Sound, keep};
use crate::compose::{Canvas, Place, Setup, Source};
use crate::config::{self, Audio, Config, Target};
use crate::copy::copy;
use crate::decode::{Meta, Probe, damage, probe};
use crate::dedup::{differences, threshold};
use crate::encode::{Encoder, Plan};
use crate::graph::lossless;
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
    length + job.hold
}

pub fn sounds(jobs: &[Job]) -> Result<Vec<Sound>> {
    let mut sounds = Vec::new();
    for job in jobs {
        let probe = named(job)?;
        for stream in (0..probe.tracks.len()).filter(|track| !job.muted.contains(track)) {
            sounds.push(Sound {
                clip: job.clip.clone(),
                stream,
                start: job.cut.map_or(0.0, |cut| cut.0),
                length: kept(job, &probe),
                at: job.at,
                track: stream,
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
        if probe(&job.clip)?.fps > 0.0 {
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
fn rational(fps: f64) -> (i64, i64) {
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
        let mut config = job.config.clone();
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
    if let Some(size) = setup.size {
        (meta.width, meta.height) = size;
    }
    if let Some(rate) = setup.fps {
        fps = rational(rate);
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

    let job = jobs[base].1;
    let config = &job.config.output;
    let start = job.cut.map_or(0.0, |cut| cut.0);
    let (mix, sized) = (config.audio == Audio::Mix, config.target == Target::Size);
    let plan = match timeline {
        Some(sounds) => Plan::Mix(Mixer::new(sounds, mix)),
        // sound that is copied has no known size
        None if sized || (mix && most > 1) => Plan::Encode {
            lanes: if mix { most.min(1) } else { most },
        },
        None => Plan::Copy {
            clip: &job.clip,
            start,
            muted: &silenced[base],
        },
    };
    let lanes = match &plan {
        Plan::Mix(mixer) => mixer.lanes(),
        Plan::Encode { lanes } => *lanes,
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
    let mut encoder = Encoder::open(format::output(output)?, &encoded, fps, config, bits, plan)?;
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
    let alone = timeline.is_none() && upscaler.is_none() && job.fade.none();
    let alone = alone && job.place == Place::default();
    let fused = match first.graph() {
        Some(graph) if alone && graph.packs() => encoder
            .spare()
            .is_some_and(|spare| graph.encode(spare, from, from + frames)),
        _ => false,
    };
    let frames = match (fused, first.graph()) {
        (true, Some(graph)) => frames.min(graph.info.frames - from),
        _ => frames,
    };
    if timeline.is_none() {
        encoder.begin(&job.clip, start, &silenced[base])?;
    }
    let mut canvas = Canvas::new(meta, setup.background);
    let size = canvas.size();
    let mut sources = BTreeMap::from([(base, first)]);

    let started = Instant::now();
    let mut reported = started;
    let mut written = 0;
    for (index, part) in parts.iter().enumerate() {
        if cancel.stopped() {
            break;
        }
        // a clip stays open until its end, and the gpu is free before the next one opens
        sources.retain(|clip, _| placed[*clip].at + placed[*clip].length > part.start + 0.001);
        // a video that fills the canvas hides the clips under it
        let fills = |clip: &usize| {
            let size = (size, (probes[*clip].width, probes[*clip].height));
            !probes[*clip].still && jobs[*clip].1.place.covers(size.0, size.1)
        };
        let solid = |clip: &usize| fills(clip) && jobs[*clip].1.fade.none();
        let shown = &part.clips[part.clips.iter().rposition(solid).unwrap_or(0)..];
        for clip in shown {
            if !sources.contains_key(clip) {
                sources.insert(*clip, open(*clip)?);
            }
        }
        let last = match index + 1 == parts.len() {
            true => frames,
            false => ((part.end * rate).round() as i32).min(frames),
        };
        while written < last && !cancel.stopped() {
            let made = match sources.get_mut(&base).filter(|_| fused) {
                Some(source) => Some(source.made(from + written, cancel)?),
                None => None,
            };
            match made {
                Some(Made::Packets(packets)) => encoder.pass(packets, written)?,
                Some(Made::Picture(picture)) => encoder.write(picture, written)?,
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
                        let inside = job.cut.map_or(0.0, |cut| cut.0) + time - job.at;
                        if let Some(source) = sources.get_mut(clip) {
                            layers.push((
                                source.at(inside, cancel)?,
                                job.place,
                                level(clip) as f32,
                            ));
                        }
                    }
                    let picture = canvas.compose(layers)?;
                    let picture = match &upscaler {
                        Some(upscaler) => upscaler.run(&picture)?,
                        None => picture,
                    };
                    encoder.write(picture, written)?;
                }
            }
            written += 1;
            if reported.elapsed() >= Duration::from_millis(100) || written == frames {
                reported = Instant::now();
                sink(Event::Progress {
                    frame: written as u32,
                    frames: frames as u32,
                    fps: written as f32 / started.elapsed().as_secs_f32(),
                });
            }
        }
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
    if written < frames {
        sink(Event::Warning(format!(
            "stopped at frame {written} of {frames}"
        )));
    }
    // the file is closed in the normal way, so a stopped render still plays
    match written {
        0 => Err("stopped".into()),
        _ => encoder.finish(written),
    }
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

fn render(job: &Job, clip: usize, output: &Path, sink: Sink, cancel: &Cancel) -> Result<()> {
    // a file of its own starts with the clip, not at its place on the timeline
    let job = &Job {
        at: 0.0,
        ..job.clone()
    };
    let untouched = job.place == Place::default();
    if job.copy || (untouched && lossless(probe(&job.clip)?.fps, &job.config)) {
        return copy(job, output, sink, cancel);
    }
    fit(
        &[(clip, job)],
        None,
        &Setup::default(),
        output,
        sink,
        cancel,
    )
}

// with join, all clips go into one file in their order
pub fn run(jobs: &[Job], timeline: Option<&[Sound]>, setup: &Setup, sink: Sink, cancel: &Cancel) {
    if let Some(sounds) = timeline.filter(|_| !jobs.is_empty()) {
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
        return;
    }
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
        match render(job, clip, &output, sink, cancel) {
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
