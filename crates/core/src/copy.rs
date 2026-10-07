use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use ffmpeg_next::{Packet, Rational, codec, encoder, ffi, format, media};

use crate::queue::Job;
use crate::{Event, Result, Sink};

// copies the packets of the cut without a new encode, so the cut starts on a keyframe
pub fn copy(job: &Job, output: &Path, sink: Sink, cancel: &AtomicBool) -> Result<()> {
    ffmpeg_next::init()?;
    let mut input = format::input(&job.clip)?;
    let mut target = format::output(output)?;
    let video = input
        .streams()
        .best(media::Type::Video)
        .ok_or("the clip has no video")?;
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

    let seconds = input.duration() as f64 / f64::from(ffi::AV_TIME_BASE);
    let (start, end) = job.cut.unwrap_or((0.0, f64::INFINITY));
    let end = end.min(seconds);
    let begin = unsafe {
        let context = input.as_mut_ptr();
        let begin = (*context).start_time.max(0);
        let position = begin + (start * f64::from(ffi::AV_TIME_BASE)) as i64;
        ffi::av_seek_frame(context, -1, position, ffi::AVSEEK_FLAG_BACKWARD);
        begin as f64 / f64::from(ffi::AV_TIME_BASE)
    };
    target.write_header()?;
    sink(Event::Info("nothing to render, copying the streams".into()));

    let frames = ((end - start) * rate).max(1.0) as u32;
    let started = Instant::now();
    let mut reported = started;
    let mut origin = None;
    let mut written = 0;
    let mut packet = Packet::empty();
    while !cancel.load(Ordering::Relaxed) && crate::decode::next(&mut packet, &mut input).is_ok() {
        let Some(index) = streams[packet.stream()] else {
            continue;
        };
        let base = input
            .stream(packet.stream())
            .map_or(Rational(1, 1), |stream| stream.time_base());
        let time = packet.dts().unwrap_or(0) as f64 * f64::from(base) - begin;
        if packet.stream() == video {
            if origin.is_none() && packet.is_key() {
                origin = Some(time);
            }
            if time > end {
                break;
            }
            written += 1;
        }
        let Some(origin) = origin.filter(|origin| time >= *origin) else {
            continue;
        };
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
