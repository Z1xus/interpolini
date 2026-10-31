use ffmpeg_next::{ffi, format, frame};

use crate::Result;
use crate::audio::{BITRATE, Mixer, Sound};
use crate::compose::{Canvas, Setup, Source, Sources};
use crate::config::Audio;
use crate::decode::{Meta, probe};
use crate::encode::{Encoder, GOP, Plan};
use crate::queue::{Job, kept};
use crate::upscale::Upscaler;
use crate::work::Cancel;

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

fn encode(
    meta: Meta,
    fps: (i64, i64),
    job: &Job,
    mut picture: impl FnMut(i32) -> Result<frame::Video>,
    cancel: &Cancel,
) -> Result<f64> {
    let rate = fps.0 as f64 / fps.1 as f64;
    let upscaler = Upscaler::new(&meta, &job.config.upscale);
    let encoded = upscaler.as_ref().map_or(meta, |upscaler| upscaler.meta);
    let plan = Plan::Encode { lanes: 0 };
    let mut encoder = Encoder::open(void()?, &encoded, fps, &job.config.output, None, plan)?;
    for n in 0..FRAMES {
        cancel.check()?;
        let picture = picture(n)?;
        let picture = match &upscaler {
            Some(upscaler) => upscaler.run(&picture)?,
            None => picture,
        };
        encoder.write(picture, n)?;
    }
    encoder.finish(FRAMES)?;
    // the part starts with a key frame, which is much larger than the frames of a long file
    let mean = |(count, bytes): (u32, u64)| bytes as f64 / f64::from(count.max(1));
    let [rest, keys] = encoder.packets;
    Ok(rate * (mean(rest) + mean(keys) / f64::from(GOP)))
}

fn sample(source: &mut Source, job: &Job, time: f64, end: f64, cancel: &Cancel) -> Result<f64> {
    let graph = source.graph().ok_or("the clip has no video")?;
    let (fps, meta) = (graph.info.fps, graph.meta);
    let rate = fps.0 as f64 / fps.1 as f64;
    let time = time.min(end - f64::from(FRAMES) / rate);
    let picture = |n| Ok(source.at(time + f64::from(n) / rate, cancel)?.0);
    Ok(encode(meta, fps, job, picture, cancel)? * job.speed)
}

pub fn estimate(
    jobs: &[Job],
    timeline: Option<&[Sound]>,
    setup: &Setup,
    cancel: &Cancel,
    sources: &mut Sources,
) -> Result<u64> {
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
    let mut spans: Vec<(f64, f64)> = videos
        .iter()
        .map(|(job, length)| (job.at, job.at + length))
        .collect();
    spans.sort_by(|a, b| a.0.total_cmp(&b.0));
    let (mut shown, mut reached) = (0.0, f64::MIN);
    for (start, stop) in spans {
        shown += (stop - start.max(reached)).max(0.0);
        reached = reached.max(stop);
    }
    let first = videos.first().ok_or("the timeline has no video")?.0;
    let (mut rates, mut black) = (Vec::new(), 0.0);
    let mut before = 0.0;
    for (job, length) in videos {
        let spots = (0..SPOTS).map(|spot| total * (spot as f64 + 0.5) / SPOTS as f64 - before);
        let spots: Vec<f64> = spots.filter(|time| (0.0..length).contains(time)).collect();
        before += length;
        if spots.is_empty() {
            continue;
        }
        sources.with(
            &job.clip,
            &job.config.retimed(job.speed),
            cancel,
            |source| {
                let graph = source.graph().ok_or("the clip has no video")?;
                let grow = setup
                    .size
                    .filter(|_| timeline.is_some())
                    .map_or(1.0, |size| {
                        f64::from(size.0) * f64::from(size.1)
                            / (f64::from(graph.meta.width) * f64::from(graph.meta.height)).max(1.0)
                    });
                if timeline.is_some() && black == 0.0 && end > shown {
                    let (meta, fps) = (graph.meta, graph.info.fps);
                    let picture = Canvas::new(meta, [0; 3]).black((meta.width, meta.height));
                    black =
                        grow * encode(meta, fps, job, |_| Ok(picture.clone()), cancel)? * job.speed;
                }
                let start = job.cut.map_or(0.0, |cut| cut.0);
                let end = start + length * job.speed;
                for time in spots {
                    let time = start + time * job.speed;
                    rates.push(grow * sample(source, job, time, end, cancel)?);
                }
                Ok(())
            },
        )?;
    }
    let picture = rates.iter().sum::<f64>() / rates.len() as f64;
    let (seconds, heard) = match timeline {
        Some(sounds) => {
            let mix = first.config.output.audio == Audio::Mix;
            (shown, Mixer::new(sounds, mix).lanes() as f64 * end)
        }
        None => (total, heard),
    };
    let empty = black * (end - shown).max(0.0);
    Ok((picture * seconds + empty + heard * BITRATE as f64 / 8.0) as u64)
}
