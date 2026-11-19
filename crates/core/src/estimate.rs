use ffmpeg_next::{ffi, format, frame};

use crate::Result;
use crate::audio::{BITRATE, Mixer, Sound};
use crate::compose::{Canvas, Setup, Sources};
use crate::config::Audio;
use crate::decode::{Meta, probe};
use crate::encode::{Encoder, GOP, Plan};
use crate::graph::Plan as Shape;
use crate::munch::Muncher;
use crate::queue::{Job, kept, packs, rational};
use crate::upscale::Upscaler;
use crate::work::{self, Cancel};

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
    setup: &Setup,
    packed: bool,
    mut picture: impl FnMut(i32) -> Result<frame::Video>,
    cancel: &Cancel,
) -> Result<f64> {
    let rate = fps.0 as f64 / fps.1 as f64;
    let upscaler = Upscaler::new(&meta, &job.config.upscale);
    let mut encoded = upscaler.as_ref().map_or(meta, |upscaler| upscaler.meta);
    // the canvas gives the size of a munch
    let mut muncher = setup
        .munch
        .map(|munch| {
            let size = setup.size.unwrap_or((encoded.width, encoded.height));
            Muncher::new(
                &Meta {
                    width: size.0,
                    height: size.1,
                    ..encoded
                },
                &munch,
            )
        })
        .transpose()?;
    if let Some(muncher) = &muncher {
        encoded = muncher.meta;
    }
    let plan = Plan::Encode { titles: Vec::new() };
    work::after_showing(cancel)?;
    let mut encoder = Encoder::open(
        void()?,
        &encoded,
        fps,
        &job.config.output,
        None,
        plan,
        packed,
    )?;
    for n in 0..FRAMES {
        work::after_showing(cancel)?;
        let picture = picture(n)?;
        let picture = match &upscaler {
            Some(upscaler) => upscaler.run(&picture)?,
            None => picture,
        };
        let picture = match &mut muncher {
            Some(muncher) => muncher.run(&picture)?,
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

// the frame rate of the file as the render picks it
fn output(graph: (i64, i64), source: f64, job: &Job, setup: &Setup) -> (i64, i64) {
    let mut fps = match job.speed {
        1.0 => graph,
        _ => Shape::new(source, &job.config).output(rational(source), &job.config),
    };
    if let Some(rate) = setup.fps {
        fps = rational(rate);
    }
    if let Some(rate) = setup.munch.and_then(|munch| munch.fps) {
        fps = (i64::from(rate), 1);
    }
    fps
}

pub fn estimate(
    jobs: &[Job],
    timeline: Option<&[Sound]>,
    setup: &Setup,
    cancel: &Cancel,
    sources: &mut Sources,
) -> Result<u64> {
    // single clips render at their own size
    let setup = &match timeline {
        Some(_) => *setup,
        None => Setup {
            munch: setup.munch,
            ..Setup::default()
        },
    };
    let mut videos = Vec::new();
    let (mut end, mut heard) = (0.0, 0.0);
    for job in jobs {
        let probe = probe(&job.clip)?;
        let length = kept(job, &probe);
        if length > 0.0 {
            end = f64::max(end, job.at + length);
        }
        let tracks = (0..probe.tracks.len()).filter(|track| !job.muted.contains(track));
        let lanes = match job.config.output.audio {
            Audio::Mix => tracks.count().min(1),
            Audio::Separate => tracks.count(),
        };
        heard += lanes as f64 * length;
        if probe.fps > 0.0 && length > 0.0 {
            videos.push((job, length, probe.fps));
        }
    }
    let total: f64 = videos.iter().map(|video| video.1).sum();
    let mut spans: Vec<(f64, f64)> = videos
        .iter()
        .map(|(job, length, _)| (job.at, job.at + length))
        .collect();
    spans.sort_by(|a, b| a.0.total_cmp(&b.0));
    let (mut shown, mut reached) = (0.0, f64::MIN);
    for (start, stop) in spans {
        shown += (stop - start.max(reached)).max(0.0);
        reached = reached.max(stop);
    }
    let first = videos.first().ok_or("the timeline has no video")?.0;
    // a joined file has the frame rate of its earliest video
    let order = |video: &&(&Job, f64, f64)| (video.0.at, video.0.track);
    let base = videos.iter().min_by(|a, b| {
        order(a)
            .0
            .total_cmp(&order(b).0)
            .then(order(a).1.cmp(&order(b).1))
    });
    // a joined file is encoded with the settings of its earliest video
    let coder = timeline.and(base).map(|video| video.0);
    let joined = timeline.and(base).map(|(job, _, fps)| {
        let rate = Shape::new(*fps, &job.config).output(rational(*fps), &job.config);
        output(rate, *fps, job, setup)
    });
    let (mut rates, mut black) = (Vec::new(), 0.0);
    let mut before = 0.0;
    for (job, length, fps) in videos {
        let spots = (0..SPOTS).map(|spot| total * (spot as f64 + 0.5) / SPOTS as f64 - before);
        let spots: Vec<f64> = spots.filter(|time| (0.0..length).contains(time)).collect();
        before += length;
        if spots.is_empty() {
            continue;
        }
        work::after_showing(cancel)?;
        sources.with(
            &job.clip,
            &job.config.retimed(job.speed),
            cancel,
            |source| {
                let graph = source.graph().ok_or("the clip has no video")?;
                let meta = graph.meta;
                let out = joined.unwrap_or_else(|| output(graph.info.fps, fps, job, setup));
                let coder = coder.unwrap_or(job);
                let upscaled = Upscaler::new(&meta, &job.config.upscale).is_some();
                let packed = packs(
                    job,
                    graph,
                    timeline.is_some(),
                    upscaled || setup.munch.is_some(),
                );
                let grow = setup
                    .size
                    .filter(|_| setup.munch.is_none())
                    .map_or(1.0, |size| {
                        f64::from(size.0) * f64::from(size.1)
                            / (f64::from(meta.width) * f64::from(meta.height)).max(1.0)
                    });
                if timeline.is_some() && black == 0.0 && end > shown {
                    let picture = Canvas::new(meta, [0; 3]).black((meta.width, meta.height));
                    let picture = |_| Ok(picture.clone());
                    black = grow * encode(meta, out, coder, setup, packed, picture, cancel)?;
                }
                let start = job.cut.map_or(0.0, |cut| cut.0);
                let end = start + length * job.speed;
                let step = job.speed * out.1 as f64 / out.0 as f64;
                for time in spots {
                    let time = (start + time * job.speed)
                        .min(end - f64::from(FRAMES) * step)
                        .max(start);
                    let picture = |n| Ok(source.at(time + f64::from(n) * step, cancel)?.0);
                    rates.push(grow * encode(meta, out, coder, setup, packed, picture, cancel)?);
                }
                Ok(())
            },
        )?;
    }
    if rates.is_empty() {
        return Err("the cut has no picture".into());
    }
    let picture = rates.iter().sum::<f64>() / rates.len() as f64;
    let (seconds, heard) = match timeline {
        Some(sounds) => {
            let mix = coder.unwrap_or(first).config.output.audio == Audio::Mix;
            (shown, Mixer::new(sounds, mix).lanes() as f64 * end)
        }
        None => (total, heard),
    };
    let empty = black * (end - shown).max(0.0);
    Ok((picture * seconds + empty + heard * BITRATE as f64 / 8.0) as u64)
}
