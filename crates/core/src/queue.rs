use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::audio::{Mixer, Sound};
use crate::compose::{Canvas, Place, Source};
use crate::config::{Audio, Config};
use crate::copy::copy;
use crate::decode::{Meta, Probe, probe};
use crate::encode::{Encoder, Plan};
use crate::graph::lossless;
use crate::timeline::{Part, Placed, parts};
use crate::upscale::Upscaler;
use crate::{Event, Result, Sink};

pub struct Job {
    pub clip: PathBuf,
    pub config: Config,
    pub cut: Option<(f64, f64)>,
    pub muted: Vec<usize>,
    // the place on the timeline, for a render into one file
    pub at: f64,
    pub track: usize,
    pub place: Place,
}

fn target(clip: &Path, extension: &str) -> PathBuf {
    let stem = clip.file_stem().unwrap_or_default().to_string_lossy();
    (1..)
        .map(|attempt| match attempt {
            1 => clip.with_file_name(format!("{stem} - interpolini.{extension}")),
            _ => clip.with_file_name(format!("{stem} - interpolini ({attempt}).{extension}")),
        })
        .find(|path| !path.exists())
        .unwrap_or_default()
}

pub fn sequence(jobs: &mut [Job]) -> Result<()> {
    let mut at = 0.0;
    for job in jobs {
        (job.at, job.track) = (at, 0);
        at += kept(job, &probe(&job.clip)?);
    }
    Ok(())
}

fn kept(job: &Job, probe: &Probe) -> f64 {
    match job.cut {
        // an image is as long as its clip
        Some((start, end)) if probe.still => end - start,
        Some((start, end)) => end.min(probe.seconds) - start,
        None => probe.seconds,
    }
}

pub fn sounds(jobs: &[Job]) -> Result<Vec<Sound>> {
    let mut sounds = Vec::new();
    for job in jobs {
        let probe = probe(&job.clip)?;
        for stream in (0..probe.tracks.len()).filter(|track| !job.muted.contains(track)) {
            sounds.push(Sound {
                clip: job.clip.clone(),
                stream,
                start: job.cut.map_or(0.0, |cut| cut.0),
                length: kept(job, &probe),
                at: job.at,
                track: stream,
            });
        }
    }
    Ok(sounds)
}

// the first video clip gives the picture size, the frame rate and the output settings,
// and the lowest one when two start together
fn base(jobs: &[(usize, &Job)]) -> Result<usize> {
    let mut videos = Vec::new();
    for (index, (_, job)) in jobs.iter().enumerate() {
        if !probe(&job.clip)?.still {
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

fn encode(
    jobs: &[(usize, &Job)],
    timeline: Option<&[Sound]>,
    output: &Path,
    sink: Sink,
    cancel: &AtomicBool,
) -> Result<()> {
    let mut most = 0;
    let mut placed = Vec::new();
    let mut probes = Vec::new();
    for (_, job) in jobs {
        let probe = probe(&job.clip)?;
        let heard = |track: &usize| !job.muted.contains(track);
        most = most.max((0..probe.tracks.len()).filter(heard).count());
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
    let open = |clip: usize| {
        let (index, job) = jobs[clip];
        if timeline.is_some() {
            sink(Event::Start { clip: index });
        }
        Source::open(&job.clip, &job.config, probes[clip].still, sink)
    };
    let base = base(jobs)?;
    let first = open(base)?;
    let graph = first.graph().ok_or("the timeline has no video")?;
    let (fps, meta) = (graph.info.fps, graph.meta);
    let rate = fps.0 as f64 / fps.1 as f64;
    let end = parts.last().map_or(0.0, |part| part.end);
    // a whole clip on its own gives each of its frames one time
    let whole = timeline.is_none() && jobs[0].1.cut.is_none();
    let frames = match whole {
        true => graph.info.frames,
        false => (end * rate).round() as i32,
    };

    let job = jobs[base].1;
    let config = &job.config.output;
    let start = job.cut.map_or(0.0, |cut| cut.0);
    let plan = match timeline {
        Some(sounds) => Plan::Mix(Mixer::new(sounds, config.audio == Audio::Mix)),
        None if config.audio == Audio::Mix && most > 1 => Plan::Encode { lanes: 1 },
        None => Plan::Copy {
            clip: &job.clip,
            start,
            muted: &job.muted,
        },
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
    let mut encoder = Encoder::open(output, &encoded, fps, config, plan)?;
    sink(Event::Info(format!("encoding with {}", encoder.name)));
    if timeline.is_none() {
        encoder.begin(&job.clip, start, &job.muted)?;
    }
    let mut canvas = Canvas::new(meta);
    let size = canvas.size();
    let mut sources = BTreeMap::from([(base, first)]);

    let started = Instant::now();
    let mut reported = started;
    let mut written = 0;
    for part in &parts {
        // a clip stays open until its end, and the gpu is free before the next one opens
        sources.retain(|clip, _| placed[*clip].at + placed[*clip].length > part.start + 0.001);
        // a video that fills the canvas hides the clips under it
        let fills = |clip: &usize| {
            let size = (size, (probes[*clip].width, probes[*clip].height));
            !probes[*clip].still && jobs[*clip].1.place.covers(size.0, size.1)
        };
        let shown = &part.clips[part.clips.iter().rposition(fills).unwrap_or(0)..];
        for clip in shown {
            if !sources.contains_key(clip) {
                sources.insert(*clip, open(*clip)?);
            }
        }
        let last = match whole {
            true => frames,
            false => ((part.end * rate).round() as i32).min(frames),
        };
        while written < last && !cancel.load(Ordering::Relaxed) {
            let time = f64::from(written) / rate;
            let mut layers = Vec::new();
            for clip in shown {
                let job = jobs[*clip].1;
                let inside = job.cut.map_or(0.0, |cut| cut.0) + time - job.at;
                if let Some(source) = sources.get_mut(clip) {
                    layers.push((source.at(inside)?, job.place));
                }
            }
            let picture = canvas.compose(layers)?;
            let picture = match &upscaler {
                Some(upscaler) => upscaler.run(&picture)?,
                None => picture,
            };
            encoder.write(picture, written)?;
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
    if written < frames {
        sink(Event::Warning(format!(
            "stopped at frame {written} of {frames}"
        )));
    }
    // the file is closed in the normal way, so a stopped render still plays
    match written {
        0 => Err("stopped".into()),
        _ => encoder.finish(f64::from(written) / rate),
    }
}

fn render(job: &Job, clip: usize, output: &Path, sink: Sink, cancel: &AtomicBool) -> Result<()> {
    let untouched = job.place == Place::default();
    if untouched && lossless(probe(&job.clip)?.fps, &job.config) {
        return copy(job, output, sink, cancel);
    }
    encode(&[(clip, job)], None, output, sink, cancel)
}

// with join, all clips go into one file in their order
pub fn run(jobs: &[Job], timeline: Option<&[Sound]>, sink: Sink, cancel: &AtomicBool) {
    if let Some(sounds) = timeline.filter(|_| !jobs.is_empty()) {
        let started = Instant::now();
        let all: Vec<(usize, &Job)> = jobs.iter().enumerate().collect();
        let named = &jobs[base(&all).unwrap_or(0)];
        let output = target(&named.clip, named.config.output.container.name());
        match encode(&all, Some(sounds), &output, sink, cancel) {
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
                let error = error.to_string();
                sink(Event::Failed { clip: 0, error });
            }
        }
        return;
    }
    for (clip, job) in jobs.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        sink(Event::Start { clip });
        let started = Instant::now();
        let output = target(&job.clip, job.config.output.container.name());
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
                sink(Event::Failed {
                    clip,
                    error: error.to_string(),
                });
            }
        }
    }
}
