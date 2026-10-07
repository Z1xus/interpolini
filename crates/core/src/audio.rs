use std::collections::VecDeque;
use std::path::{Path, PathBuf};

use ffmpeg_next::format::sample::Type;
use ffmpeg_next::format::{self, Sample};
use ffmpeg_next::software::resampling;
use ffmpeg_next::{ChannelLayout, Packet, Rational, codec, encoder, ffi, frame, media};

use crate::Result;

pub const RATE: u32 = 48_000;
const FORMAT: Sample = Sample::F32(Type::Planar);

struct Track {
    stream: usize,
    decoder: codec::decoder::Audio,
    resampler: Option<resampling::Context>,
    samples: [VecDeque<f32>; 2],
    // the samples before the start of the cut that are still to drop
    skip: Option<usize>,
}

pub struct Source {
    input: format::context::Input,
    tracks: Vec<Track>,
    packet: Packet,
    start: f64,
    ended: bool,
}

unsafe impl Send for Source {}

impl Track {
    fn push(&mut self, decoded: &mut frame::Audio, start: f64, base: Rational) -> Result<()> {
        // a track without a channel layout gets the usual one for its channels
        let layout = match decoded.channel_layout() {
            layout if layout.channels() > 0 && !layout.is_empty() => layout,
            _ => ChannelLayout::default(i32::from(decoded.channels())),
        };
        decoded.set_channel_layout(layout);
        if self.resampler.is_none() {
            let resampler = resampling::Context::get(
                decoded.format(),
                layout,
                decoded.rate(),
                FORMAT,
                ChannelLayout::STEREO,
                RATE,
            )?;
            self.resampler = Some(resampler);
        }
        let time = decoded.timestamp().unwrap_or(0) as f64 * f64::from(base);
        if self.skip.is_none() {
            // sound that starts after the picture gets silence before it
            let late = ((time - start).max(0.0) * f64::from(RATE)) as usize;
            for samples in &mut self.samples {
                samples.extend(std::iter::repeat_n(0.0, late));
            }
        }
        let skip = *self
            .skip
            .get_or_insert(((start - time).max(0.0) * f64::from(RATE)) as usize);
        let mut output = frame::Audio::empty();
        if let Some(resampler) = &mut self.resampler {
            resampler.run(decoded, &mut output)?;
        }
        let dropped = skip.min(output.samples());
        self.skip = Some(skip - dropped);
        for (channel, samples) in self.samples.iter_mut().enumerate() {
            samples.extend(&output.plane::<f32>(channel)[dropped..output.samples()]);
        }
        Ok(())
    }
}

impl Source {
    // keep says which audio tracks of the clip to decode, by their number
    pub fn open(clip: &Path, start: f64, keep: impl Fn(usize) -> bool) -> Result<Self> {
        let mut input = format::input(clip)?;
        let mut tracks = Vec::new();
        let mut number = 0;
        for stream in input.streams() {
            if stream.parameters().medium() != media::Type::Audio {
                continue;
            }
            number += 1;
            if !keep(number - 1) {
                continue;
            }
            let decoder = codec::Context::from_parameters(stream.parameters())?
                .decoder()
                .audio()?;
            tracks.push(Track {
                stream: stream.index(),
                decoder,
                resampler: None,
                samples: Default::default(),
                skip: None,
            });
        }
        let start = unsafe {
            let context = input.as_mut_ptr();
            let offset =
                crate::decode::origin(&input) + (start * f64::from(ffi::AV_TIME_BASE)) as i64;
            ffi::av_seek_frame(context, -1, offset, ffi::AVSEEK_FLAG_BACKWARD);
            offset as f64 / f64::from(ffi::AV_TIME_BASE)
        };
        Ok(Self {
            input,
            tracks,
            packet: Packet::empty(),
            start,
            ended: false,
        })
    }

    fn fill(&mut self, count: usize) -> Result<()> {
        let short = |tracks: &[Track]| tracks.iter().any(|track| track.samples[0].len() < count);
        let mut decoded = frame::Audio::empty();
        while !self.ended && short(&self.tracks) {
            if crate::decode::next(&mut self.packet, &mut self.input).is_err() {
                self.ended = true;
                break;
            }
            let stream = self.packet.stream();
            let Some(track) = self.tracks.iter_mut().find(|track| track.stream == stream) else {
                continue;
            };
            let base = self
                .input
                .stream(stream)
                .map_or(Rational(1, 1), |stream| stream.time_base());
            // a broken packet must not stop the clip
            let _ = track.decoder.send_packet(&self.packet);
            while track.decoder.receive_frame(&mut decoded).is_ok() {
                track.push(&mut decoded, self.start, base)?;
            }
        }
        Ok(())
    }

    pub fn take(&mut self, track: Option<usize>, count: usize) -> Result<[Vec<f32>; 2]> {
        self.fill(count)?;
        let mut output = [vec![0.0; count], vec![0.0; count]];
        for (index, source) in self.tracks.iter_mut().enumerate() {
            if track.is_some_and(|track| track != index) {
                continue;
            }
            for (channel, samples) in source.samples.iter_mut().enumerate() {
                let taken = samples.drain(..count.min(samples.len()));
                for (sum, sample) in output[channel].iter_mut().zip(taken) {
                    *sum = (*sum + sample).clamp(-1.0, 1.0);
                }
            }
        }
        Ok(output)
    }
}

pub fn peaks(clip: &Path, seconds: f64, count: usize) -> Result<Vec<Vec<f32>>> {
    let mut source = Source::open(clip, 0.0, |_| true)?;
    let slice = ((seconds * f64::from(RATE)) as usize / count).max(1);
    let mut tracks = vec![Vec::with_capacity(count); source.tracks.len()];
    for _ in 0..count {
        for (track, peaks) in tracks.iter_mut().enumerate() {
            let [left, _] = source.take(Some(track), slice)?;
            peaks.push(
                left.iter()
                    .fold(0.0, |peak: f32, sample| peak.max(sample.abs())),
            );
        }
    }
    Ok(tracks)
}

#[derive(Clone, Debug)]
pub struct Sound {
    pub clip: PathBuf,
    pub stream: usize,
    // the place in the source and how much of it plays, in seconds
    pub start: f64,
    pub length: f64,
    pub at: f64,
    pub track: usize,
}

struct Playing {
    sound: Sound,
    source: Option<Source>,
}

pub struct Mixer {
    lanes: Vec<Vec<Playing>>,
}

impl Mixer {
    pub fn new(sounds: &[Sound], mix: bool) -> Self {
        let count = match mix {
            true => usize::from(!sounds.is_empty()),
            false => sounds
                .iter()
                .map(|sound| sound.track + 1)
                .max()
                .unwrap_or(0),
        };
        let mut lanes: Vec<Vec<Playing>> = (0..count).map(|_| Vec::new()).collect();
        for sound in sounds {
            let lane = if mix { 0 } else { sound.track };
            lanes[lane].push(Playing {
                sound: sound.clone(),
                source: None,
            });
        }
        Self { lanes }
    }

    pub fn lanes(&self) -> usize {
        self.lanes.len()
    }

    // the samples of one lane from a place on the timeline, counted in samples
    pub fn take(&mut self, lane: usize, position: i64, count: usize) -> Result<[Vec<f32>; 2]> {
        let rate = f64::from(RATE);
        let mut output = [vec![0.0; count], vec![0.0; count]];
        for playing in &mut self.lanes[lane] {
            let sound = &playing.sound;
            let first = (sound.at * rate) as i64;
            let last = first + (sound.length * rate) as i64;
            let (from, to) = (first.max(position), last.min(position + count as i64));
            if to <= from {
                if last <= position {
                    playing.source = None;
                }
                continue;
            }
            let stream = sound.stream;
            let source = match &mut playing.source {
                Some(source) => source,
                None => {
                    let start = sound.start + (from - first) as f64 / rate;
                    playing
                        .source
                        .insert(Source::open(&sound.clip, start, |track| track == stream)?)
                }
            };
            let samples = source.take(None, (to - from) as usize)?;
            for (sums, samples) in output.iter_mut().zip(&samples) {
                let sums = &mut sums[(from - position) as usize..];
                for (sum, sample) in sums.iter_mut().zip(samples) {
                    *sum = (*sum + sample).clamp(-1.0, 1.0);
                }
            }
        }
        Ok(output)
    }
}

pub struct Lane {
    encoder: encoder::audio::Encoder,
    stream: usize,
    samples: i64,
}

impl Lane {
    pub fn open(output: &mut format::context::Output) -> Result<Self> {
        let codec = encoder::find(codec::Id::AAC).ok_or("there is no aac encoder")?;
        let header = output
            .format()
            .flags()
            .contains(format::Flags::GLOBAL_HEADER);
        let mut audio = codec::Context::new_with_codec(codec).encoder().audio()?;
        audio.set_rate(RATE as i32);
        audio.set_channel_layout(ChannelLayout::STEREO);
        audio.set_format(FORMAT);
        audio.set_bit_rate(192_000);
        audio.set_time_base(Rational(1, RATE as i32));
        if header {
            audio.set_flags(codec::Flags::GLOBAL_HEADER);
        }
        let encoder = audio.open_as(codec)?;
        let mut stream = output.add_stream(codec)?;
        stream.set_parameters(&encoder);
        Ok(Self {
            encoder,
            stream: stream.index(),
            samples: 0,
        })
    }

    pub fn position(&self) -> i64 {
        self.samples
    }

    pub fn seconds(&self) -> f64 {
        (self.samples + i64::from(self.encoder.frame_size())) as f64 / f64::from(RATE)
    }

    fn drain(&mut self, output: &mut format::context::Output) -> Result<()> {
        let mut packet = Packet::empty();
        while self.encoder.receive_packet(&mut packet).is_ok() {
            let base = output.stream(self.stream).map(|stream| stream.time_base());
            packet.rescale_ts(
                Rational(1, RATE as i32),
                base.unwrap_or(Rational(1, RATE as i32)),
            );
            packet.set_stream(self.stream);
            packet.write_interleaved(output)?;
        }
        Ok(())
    }

    pub fn write(
        &mut self,
        output: &mut format::context::Output,
        samples: [Vec<f32>; 2],
    ) -> Result<()> {
        let count = self.encoder.frame_size() as usize;
        let mut sound = frame::Audio::new(FORMAT, count, ChannelLayout::STEREO);
        sound.set_rate(RATE);
        sound.set_pts(Some(self.samples));
        for (channel, samples) in samples.iter().enumerate() {
            sound.plane_mut::<f32>(channel)[..count].copy_from_slice(&samples[..count]);
        }
        self.samples += count as i64;
        self.encoder.send_frame(&sound)?;
        self.drain(output)
    }

    pub fn size(&self) -> usize {
        self.encoder.frame_size() as usize
    }

    pub fn finish(&mut self, output: &mut format::context::Output) -> Result<()> {
        self.encoder.send_eof()?;
        self.drain(output)
    }
}
