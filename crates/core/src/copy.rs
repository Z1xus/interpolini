use std::collections::VecDeque;
use std::path::Path;
use std::time::{Duration, Instant};

use ffmpeg_next::{Discard, Packet, Rational, codec, encoder, ffi, format, media};

use crate::queue::Job;
use crate::work::Cancel;
use crate::{Event, Result, Sink};

pub fn keyframes(clip: &Path, cancel: &Cancel) -> Result<Vec<f64>> {
    ffmpeg_next::init()?;
    let mut input = format::input(clip)?;
    let video = crate::decode::video(&input).ok_or("the clip has no video")?;
    let (video, base) = (video.index(), f64::from(video.time_base()));
    let begin = crate::decode::origin(&input) as f64 / 1e6;
    for mut stream in input.streams_mut() {
        let discard = match stream.index() == video {
            true => Discard::NonKey,
            false => Discard::All,
        };
        unsafe { (*stream.as_mut_ptr()).discard = discard.into() };
    }
    let mut keys = Vec::new();
    let mut packet = Packet::empty();
    while crate::decode::next(&mut packet, &mut input).is_ok() {
        if packet.stream() == video && packet.is_key() {
            cancel.check()?;
            let time = packet.pts().or(packet.dts()).unwrap_or(0);
            keys.push(time as f64 * base - begin);
        }
    }
    Ok(keys)
}

// copies the packets of the cut without a new encode, so the cut starts on a keyframe
pub fn copy(job: &Job, output: &Path, sink: Sink, cancel: &Cancel) -> Result<()> {
    ffmpeg_next::init()?;
    let mut input = format::input(&job.clip)?;
    let mut target = format::output(output)?;
    let video = crate::decode::video(&input).ok_or("the clip has no video")?;
    let (video, rate) = (video.index(), f64::from(video.avg_frame_rate()));

    let mut streams = Vec::new();
    let mut track = 0;
    for stream in input.streams() {
        let audio = stream.parameters().medium() == media::Type::Audio;
        track += usize::from(audio);
        if stream.index() != video && (!audio || job.muted.contains(&(track - 1))) {
            streams.push(None);
            continue;
        }
        let mut added = target.add_stream(encoder::find(codec::Id::None))?;
        added.set_parameters(stream.parameters());
        // the tag of one container is not valid in another
        unsafe { (*added.parameters().as_mut_ptr()).codec_tag = 0 };
        streams.push(Some(added.index()));
    }

    let seconds = input
        .stream(video)
        .map_or(0.0, |stream| match stream.duration() {
            ffi::AV_NOPTS_VALUE => input.duration() as f64 / f64::from(ffi::AV_TIME_BASE),
            ticks => (ticks + stream.start_time().max(0)) as f64 * f64::from(stream.time_base()),
        });
    let (start, end) = job.cut.unwrap_or((0.0, f64::INFINITY));
    let begin = unsafe {
        let context = input.as_mut_ptr();
        let begin = (*context).start_time.max(0);
        // a start on a keyframe must not fall before it
        let position = begin + ((start + 0.001) * f64::from(ffi::AV_TIME_BASE)) as i64;
        ffi::av_seek_frame(context, -1, position, ffi::AVSEEK_FLAG_BACKWARD);
        begin as f64 / f64::from(ffi::AV_TIME_BASE)
    };
    let end = end.min(seconds - begin);
    target.write_header()?;
    if !job.copy {
        sink(Event::Info("nothing to render, copying the streams".into()));
    }

    let frames = ((end - start) * rate).max(1.0) as u32;
    let started = Instant::now();
    let mut reported = started;
    let mut origin = None;
    let mut written = 0;
    let mut finished = vec![false; streams.len()];
    let mut waiting = VecDeque::new();
    let mut held = Vec::new();
    let mut packet = Packet::empty();
    while !cancel.stopped() {
        let stream = match waiting.pop_front() {
            Some(held) => {
                packet = held;
                packet.stream()
            }
            None => {
                if streams
                    .iter()
                    .zip(&finished)
                    .all(|(stream, done)| stream.is_none() || *done)
                    || crate::decode::next(&mut packet, &mut input).is_err()
                {
                    break;
                }
                packet.stream()
            }
        };
        let Some(index) = streams[stream] else {
            continue;
        };
        let base = input
            .stream(stream)
            .map_or(Rational(1, 1), |stream| stream.time_base());
        let moment = match stream == video {
            true => packet.dts(),
            false => packet.pts().or(packet.dts()),
        };
        let time = moment.unwrap_or(0) as f64 * f64::from(base) - begin;
        if time > end {
            finished[stream] = true;
            continue;
        }
        if stream == video {
            if origin.is_none() && packet.is_key() {
                origin = Some(time);
                waiting.extend(held.drain(..));
            }
            written += 1;
        }
        let Some(origin) = origin else {
            if stream != video {
                held.push(packet.clone());
            }
            continue;
        };
        let length = packet.duration() as f64 * f64::from(base);
        if stream == video && time < origin || stream != video && time + length <= origin {
            continue;
        }
        let shift = ((origin + begin) / f64::from(base)) as i64;
        packet.set_pts(packet.pts().map(|pts| pts - shift));
        packet.set_dts(packet.dts().map(|dts| dts - shift));
        let target_base = target
            .stream(index)
            .map_or(base, |stream| stream.time_base());
        packet.rescale_ts(base, target_base);
        packet.set_stream(index);
        packet.set_position(-1);
        packet.write_interleaved(&mut target)?;
        if reported.elapsed() >= Duration::from_millis(100) {
            reported = Instant::now();
            let fps = written as f32 / started.elapsed().as_secs_f32();
            sink(Event::Progress {
                frame: written.min(frames),
                frames,
                fps,
            });
        }
    }
    target.write_trailer()?;
    Ok(())
}
