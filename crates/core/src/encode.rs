use std::path::Path;

use ffmpeg_next::format::{self, Pixel};
use ffmpeg_next::software::scaling;
use ffmpeg_next::{Dictionary, Packet, Rational, codec, encoder, ffi, frame, media};

use crate::Result;
use crate::audio::{Lane, Mixer, Source};
use crate::config::{self, Codec, Output};
use crate::decode::Meta;

fn candidates(output: &Output) -> impl Iterator<Item = &'static str> {
    let (hardware, software): (&[_], _) = match output.codec {
        Codec::H264 => (
            &["h264_nvenc", "h264_amf", "h264_qsv", "h264_videotoolbox"],
            "libx264",
        ),
        Codec::Hevc => (
            &["hevc_nvenc", "hevc_amf", "hevc_qsv", "hevc_videotoolbox"],
            "libx265",
        ),
        Codec::Av1 => (&["av1_nvenc", "av1_amf", "av1_qsv"], "libsvtav1"),
    };
    let (hardware, software) = match output.encoder {
        config::Encoder::Auto => (hardware, Some(software)),
        config::Encoder::Hardware => (hardware, None),
        config::Encoder::Software => (&[][..], Some(software)),
    };
    hardware.iter().copied().chain(software)
}

// ffmpeg style pairs, like "-preset p7 -cq 18"
fn pairs(text: &str) -> impl Iterator<Item = (&str, &str)> {
    let mut words = text.split_whitespace();
    std::iter::from_fn(move || Some((words.next()?.trim_start_matches('-'), words.next()?)))
}

fn options(name: &str, quality: u32, custom: &str) -> Dictionary<'static> {
    let quality = quality.to_string();
    let mut options = Dictionary::new();
    let pairs: &[(&str, &str)] = match name.rsplit('_').next() {
        Some("nvenc") => &[
            ("preset", "p4"),
            ("tune", "hq"),
            ("rc", "vbr"),
            ("cq", &quality),
            ("b", "0"),
        ],
        Some("amf") => &[
            ("rc", "cqp"),
            ("qp_i", &quality),
            ("qp_p", &quality),
            ("qp_b", &quality),
        ],
        Some("qsv") => &[("global_quality", &quality)],
        Some("videotoolbox") => &[("flags", "+qscale"), ("global_quality", &quality)],
        _ if name == "libsvtav1" => &[("crf", &quality), ("preset", "10")],
        _ => &[("crf", &quality), ("preset", "veryfast")],
    };
    for (key, value) in pairs.iter().copied().chain(self::pairs(custom)) {
        if key != "c:v" {
            options.set(key, value);
        }
    }
    options
}

struct Audio {
    input: format::context::Input,
    streams: Vec<Option<usize>>,
    packet: Packet,
    held: bool,
    offset: f64,
}

impl Audio {
    fn open(
        clip: &Path,
        output: &mut format::context::Output,
        start: f64,
        muted: &[usize],
    ) -> Result<Option<Self>> {
        let mut input = format::input(clip)?;
        let mut streams = Vec::new();
        let mut track = 0;
        for stream in input.streams() {
            let audio = stream.parameters().medium() == media::Type::Audio;
            track += usize::from(audio);
            if !audio || muted.contains(&(track - 1)) {
                streams.push(None);
                continue;
            }
            let mut target = output.add_stream(encoder::find(codec::Id::None))?;
            target.set_parameters(stream.parameters());
            // the tag of one container is not valid in another
            unsafe { (*target.parameters().as_mut_ptr()).codec_tag = 0 };
            streams.push(Some(target.index()));
        }
        if streams.iter().all(Option::is_none) {
            return Ok(None);
        }
        let offset = unsafe {
            let context = input.as_mut_ptr();
            for (index, stream) in streams.iter().enumerate() {
                if stream.is_none() {
                    (**(*context).streams.add(index)).discard = ffi::AVDiscard::AVDISCARD_ALL;
                }
            }
            let offset =
                crate::decode::origin(&input) + (start * f64::from(ffi::AV_TIME_BASE)) as i64;
            ffi::av_seek_frame(context, -1, offset, ffi::AVSEEK_FLAG_BACKWARD);
            offset as f64 / f64::from(ffi::AV_TIME_BASE)
        };
        Ok(Some(Self {
            input,
            streams,
            packet: Packet::empty(),
            held: false,
            offset,
        }))
    }

    fn copy(&mut self, output: &mut format::context::Output, seconds: f64) -> Result<()> {
        loop {
            if !self.held {
                match crate::decode::next(&mut self.packet, &mut self.input) {
                    Ok(()) => {}
                    Err(ffmpeg_next::Error::Eof) => return Ok(()),
                    Err(error) => return Err(error.into()),
                }
            }
            let Some(target) = self.streams[self.packet.stream()] else {
                continue;
            };
            let base = self
                .input
                .stream(self.packet.stream())
                .map(|stream| stream.time_base());
            let base = base.unwrap_or(Rational(1, 1));
            let time = self.packet.dts().unwrap_or(0) as f64 * f64::from(base);
            if time < self.offset {
                continue;
            }
            self.held = time > seconds + self.offset;
            if self.held {
                return Ok(());
            }
            let shift = (self.offset / f64::from(base)) as i64;
            self.packet
                .set_pts(self.packet.pts().map(|pts| pts - shift));
            self.packet
                .set_dts(self.packet.dts().map(|dts| dts - shift));
            let target_base = output.stream(target).map(|stream| stream.time_base());
            self.packet.rescale_ts(base, target_base.unwrap_or(base));
            self.packet.set_stream(target);
            self.packet.set_position(-1);
            self.packet.write_interleaved(output)?;
        }
    }
}

pub enum Plan<'a> {
    // the packets of the clip, as they are
    Copy {
        clip: &'a Path,
        start: f64,
        muted: &'a [usize],
    },
    // the tracks of each clip in its turn, as one track or one for each
    Encode {
        lanes: usize,
    },
    // the sounds of a timeline, added together
    Mix(Mixer),
}

pub struct Encoder {
    output: format::context::Output,
    encoder: encoder::video::Encoder,
    audio: Option<Audio>,
    lanes: Vec<Lane>,
    source: Option<Source>,
    mixer: Option<Mixer>,
    nv12: Option<scaling::Context>,
    packet: Packet,
    base: Rational,
    pub name: String,
}

impl Encoder {
    pub fn open(
        path: &Path,
        meta: &Meta,
        fps: (i64, i64),
        config: &Output,
        plan: Plan,
    ) -> Result<Self> {
        let mut output = format::output(path)?;
        let header = output
            .format()
            .flags()
            .contains(format::Flags::GLOBAL_HEADER);
        let base = Rational(fps.1 as i32, fps.0 as i32);
        let mut failure = String::from("no encoder for this codec");
        let chosen = pairs(&config.options).find(|pair| pair.0 == "c:v");
        let names: Vec<&str> = match chosen {
            Some(pair) => vec![pair.1],
            None => candidates(config).collect(),
        };
        let opened = names.into_iter().find_map(|name| {
            let codec = encoder::find_by_name(name)?;
            let mut video = codec::Context::new_with_codec(codec)
                .encoder()
                .video()
                .ok()?;
            video.set_width(meta.width);
            video.set_height(meta.height);
            video.set_format(if name.ends_with("_qsv") {
                Pixel::NV12
            } else {
                Pixel::YUV420P
            });
            video.set_time_base(base);
            video.set_frame_rate(Some(base.invert()));
            video.set_colorspace(meta.space);
            video.set_color_range(meta.range);
            video.set_color_primaries(meta.primaries);
            video.set_color_transfer_characteristic(meta.transfer);
            if header {
                video.set_flags(codec::Flags::GLOBAL_HEADER);
            }
            match video.open_with(options(name, config.quality, &config.options)) {
                Ok(encoder) => Some((name, encoder)),
                Err(error) => {
                    failure = format!("{name}: {error}");
                    None
                }
            }
        });
        let (name, encoder) = opened.ok_or(failure)?;
        output
            .add_stream(encoder::find_by_name(name))?
            .set_parameters(&encoder);
        let mut lanes = Vec::new();
        let mut mixer = None;
        let audio = match plan {
            Plan::Copy { clip, start, muted } => Audio::open(clip, &mut output, start, muted)?,
            Plan::Encode { lanes: count } => {
                for _ in 0..count {
                    lanes.push(Lane::open(&mut output)?);
                }
                None
            }
            Plan::Mix(sounds) => {
                for _ in 0..sounds.lanes() {
                    lanes.push(Lane::open(&mut output)?);
                }
                mixer = Some(sounds);
                None
            }
        };
        output.write_header()?;
        let nv12 = if encoder.format() == Pixel::NV12 {
            let (width, height) = (meta.width, meta.height);
            let flags = scaling::Flags::BILINEAR;
            Some(scaling::Context::get(
                Pixel::YUV420P,
                width,
                height,
                Pixel::NV12,
                width,
                height,
                flags,
            )?)
        } else {
            None
        };
        Ok(Self {
            output,
            encoder,
            audio,
            lanes,
            source: None,
            mixer,
            nv12,
            packet: Packet::empty(),
            base,
            name: name.to_owned(),
        })
    }

    fn drain(&mut self) -> Result<()> {
        while self.encoder.receive_packet(&mut self.packet).is_ok() {
            let target = self
                .output
                .stream(0)
                .map_or(self.base, |stream| stream.time_base());
            let seconds = self.packet.dts().unwrap_or(0) as f64 * f64::from(self.base);
            if let Some(audio) = &mut self.audio {
                audio.copy(&mut self.output, seconds)?;
            }
            self.packet.rescale_ts(self.base, target);
            self.packet.set_stream(0);
            self.packet.write_interleaved(&mut self.output)?;
        }
        Ok(())
    }

    pub fn begin(&mut self, clip: &Path, start: f64, muted: &[usize]) -> Result<()> {
        if !self.lanes.is_empty() && self.mixer.is_none() {
            self.source = Some(Source::open(clip, start, |track| !muted.contains(&track))?);
        }
        Ok(())
    }

    fn pump(&mut self, seconds: f64) -> Result<()> {
        let mix = self.lanes.len() == 1;
        while self.lanes.iter().any(|lane| lane.seconds() <= seconds) {
            for (index, lane) in self.lanes.iter_mut().enumerate() {
                let track = (!mix).then_some(index);
                let samples = match (&mut self.mixer, &mut self.source) {
                    (Some(mixer), _) => mixer.take(index, lane.position(), lane.size())?,
                    (None, Some(source)) => source.take(track, lane.size())?,
                    (None, None) => [vec![0.0; lane.size()], vec![0.0; lane.size()]],
                };
                lane.write(&mut self.output, samples)?;
            }
        }
        Ok(())
    }

    pub fn write(&mut self, mut picture: frame::Video, n: i32) -> Result<()> {
        if let Some(scaler) = &mut self.nv12 {
            let mut converted = frame::Video::empty();
            scaler.run(&picture, &mut converted)?;
            picture = converted;
        }
        picture.set_pts(Some(i64::from(n)));
        self.encoder.send_frame(&picture)?;
        self.drain()?;
        self.pump(f64::from(n + 1) * f64::from(self.base))
    }

    pub fn finish(mut self, seconds: f64) -> Result<()> {
        self.encoder.send_eof()?;
        self.drain()?;
        if let Some(audio) = &mut self.audio {
            audio.copy(&mut self.output, seconds)?;
        }
        for lane in &mut self.lanes {
            lane.finish(&mut self.output)?;
        }
        self.output.write_trailer()?;
        Ok(())
    }
}
