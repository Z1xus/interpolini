use std::sync::atomic::{AtomicBool, Ordering};

use ffmpeg_next::{ffi, format};

use crate::Result;
use crate::audio::{BITRATE, Mixer, Sound};
use crate::compose::Source;
use crate::config::Audio;
use crate::decode::probe;
use crate::encode::{Encoder, GOP, Plan};
use crate::queue::{Job, kept};
use crate::upscale::Upscaler;

const SPOTS: usize = 3;
const FRAMES: i32 = 24;

fn void() -> Result<format::context::Output> {
    let mut output = std::ptr::null_mut();
    let code = unsafe {
        ffi::avformat_alloc_output_context2(
            &mut output,
            std::ptr::null(),
            c"null".as_ptr(),
            std::ptr::null(),
        )
    };
    match code {
        0 => Ok(unsafe { format::context::Output::wrap(output) }),
        code => Err(ffmpeg_next::Error::from(code).into()),
    }
}

fn sample(source: &mut Source, job: &Job, time: f64, end: f64, cancel: &AtomicBool) -> Result<f64> {
    let graph = source.graph().ok_or("the clip has no video")?;
    let (fps, meta) = (graph.info.fps, graph.meta);
    let rate = fps.0 as f64 / fps.1 as f64;
    let time = time.min(end - f64::from(FRAMES) / rate);
    let upscaler = Upscaler::new(&meta, &job.config.upscale);
    let encoded = upscaler.as_ref().map_or(meta, |upscaler| upscaler.meta);
    let plan = Plan::Encode { lanes: 0 };
    let mut encoder = Encoder::open(void()?, &encoded, fps, &job.config.output, None, plan)?;
    for n in 0..FRAMES {
        if cancel.load(Ordering::Relaxed) {
            return Err("stopped".into());
        }
        let picture = source.at(time + f64::from(n) / rate)?.0;
        let picture = match &upscaler {
            Some(upscaler) => upscaler.run(&picture)?,
            None => picture,
        };
        encoder.write(picture, n)?;
    }
    encoder.finish(0.0)?;
    // the part starts with a key frame, which is much larger than the frames of a long file
    let mean = |(count, bytes): (u32, u64)| bytes as f64 / f64::from(count.max(1));
    let [rest, keys] = encoder.packets;
    Ok(rate * (mean(rest) + mean(keys) / f64::from(GOP)))
}

pub fn estimate(jobs: &[Job], timeline: Option<&[Sound]>, cancel: &AtomicBool) -> Result<u64> {
    let mut videos = Vec::new();
    let (mut end, mut heard) = (0.0, 0.0);
    for job in jobs {
        let probe = probe(&job.clip)?;
        let length = kept(job, &probe);
        end = f64::max(end, job.at + length);
        let tracks = (0..probe.tracks.len()).filter(|track| !job.muted.contains(track));
        let lanes = match job.config.output.audio {
            Audio::Mix => tracks.count().min(1),
            Audio::Separate => tracks.count(),
        };
        heard += lanes as f64 * length;
        if !probe.still {
            videos.push((job, length));
        }
    }
    let total: f64 = videos.iter().map(|video| video.1).sum();
    let first = videos.first().ok_or("the timeline has no video")?.0;
    let mut rates = Vec::new();
    let mut before = 0.0;
    for (job, length) in videos {
        let spots = (0..SPOTS).map(|spot| total * (spot as f64 + 0.5) / SPOTS as f64 - before);
        let spots: Vec<f64> = spots.filter(|time| (0.0..length).contains(time)).collect();
        before += length;
        if spots.is_empty() {
            continue;
        }
        let mut source = Source::open(&job.clip, &job.config, false, &|_| {})?;
        let start = job.cut.map_or(0.0, |cut| cut.0);
        for time in spots {
            rates.push(sample(
                &mut source,
                job,
                start + time,
                start + length,
                cancel,
            )?);
        }
    }
    let picture = rates.iter().sum::<f64>() / rates.len() as f64;
    let (seconds, heard) = match timeline {
        Some(sounds) => {
            let mix = first.config.output.audio == Audio::Mix;
            (end, Mixer::new(sounds, mix).lanes() as f64 * end)
        }
        None => (total, heard),
    };
    Ok((picture * seconds + heard * BITRATE as f64 / 8.0) as u64)
}
