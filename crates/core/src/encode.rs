use std::ffi::c_void;
use std::path::Path;
use std::sync::{Arc, OnceLock};

use ffmpeg_next::format::{self, Pixel};
use ffmpeg_next::log::Level;
use ffmpeg_next::software::scaling;
use ffmpeg_next::{Dictionary, Packet, Rational, codec, encoder, ffi, frame, media, picture};

use crate::Result;
use crate::audio::{Lane, Mixer, Source};
use crate::budget;
use crate::config::{self, Codec, Output};
use crate::decode::Meta;

// the frames from one key frame to the next, each encoder has another default
pub const GOP: u32 = 250;

fn candidates(output: &Output) -> impl Iterator<Item = &'static str> {
    let (hardware, software): (&[_], _) = match output.codec {
        Codec::H264 => (
            &[
                "h264_nvenc",
                "h264_amf",
                "h264_qsv",
                "h264_vaapi",
                "h264_vulkan",
                "h264_videotoolbox",
            ],
            "libx264",
        ),
        Codec::Hevc => (
            &[
                "hevc_nvenc",
                "hevc_amf",
                "hevc_qsv",
                "hevc_vaapi",
                "hevc_vulkan",
                "hevc_videotoolbox",
            ],
            "libx265",
        ),
        Codec::Av1 => (
            &["av1_nvenc", "av1_amf", "av1_qsv", "av1_vaapi"],
            "libsvtav1",
        ),
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

fn options(name: &str, output: &Output, sized: bool) -> Dictionary<'static> {
    // these av1 encoders count the quantizer to 255, the others to 51 or 63
    let wide = name.starts_with("av1_") && !name.ends_with("_nvenc");
    let quality = (output.quality * if wide { 4 } else { 1 }).to_string();
    let family = name.rsplit('_').next();
    let rate: &[(&str, &str)] = match family {
        Some("nvenc") if sized => &[("tune", "hq"), ("rc", "vbr")],
        _ if sized => &[],
        Some("nvenc") => &[("tune", "hq"), ("rc", "vbr"), ("cq", &quality), ("b", "0")],
        Some("amf") => &[
            ("rc", "cqp"),
            ("qp_i", &quality),
            ("qp_p", &quality),
            ("qp_b", &quality),
        ],
        Some("qsv") => &[("global_quality", &quality)],
        Some("vaapi") => &[("rc_mode", "CQP"), ("global_quality", &quality)],
        Some("vulkan") => &[("qp", &quality)],
        Some("videotoolbox") => &[("flags", "+qscale"), ("global_quality", &quality)],
        _ => &[("crf", &quality)],
    };
    let presets = match family {
        Some("nvenc") => Some(("preset", ["p1", "p4", "p6", "p7"])),
        Some("amf") => Some(("quality", ["speed", "speed", "balanced", "quality"])),
        Some("qsv") => Some(("preset", ["veryfast", "medium", "slow", "veryslow"])),
        // 1 is the best, ffmpeg caps a level above the range of the driver
        Some("vaapi") => Some(("compression_level", ["7", "4", "2", "1"])),
        _ if name == "libsvtav1" => Some(("preset", ["12", "10", "8", "6"])),
        _ if name.starts_with("lib") => {
            Some(("preset", ["superfast", "veryfast", "medium", "slow"]))
        }
        _ => None,
    };
    let preset = presets.map(|(key, presets)| (key, presets[output.effort as usize]));
    // a card with two encoders uses both for one picture, h264 has no such mode
    let split = (family == Some("nvenc") && !name.starts_with("h264"))
        .then_some(("split_encode_mode", "forced"));
    // after a flush the next picture must be an idr
    let fresh = (family == Some("nvenc")).then_some(("forced-idr", "1"));
    let pairs = preset.into_iter().chain(rate.iter().copied());
    let pairs = pairs.chain(split).chain(fresh);
    let mut options = Dictionary::new();
    for (key, value) in pairs.chain(self::pairs(&output.options)) {
        if key != "c:v" {
            options.set(key, value);
        }
    }
    options
}

// vaapi and vulkan encode from surfaces on the gpu, not from memory
fn surfaces(video: &mut encoder::video::Video, meta: &Meta, format: Pixel) -> Result<()> {
    let kind = if format == Pixel::VAAPI {
        ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_VAAPI
    } else {
        ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_VULKAN
    };
    unsafe {
        let mut device = std::ptr::null_mut();
        let code = ffi::av_hwdevice_ctx_create(
            &mut device,
            kind,
            std::ptr::null(),
            std::ptr::null_mut(),
            0,
        );
        if code < 0 {
            return Err(ffmpeg_next::Error::from(code).into());
        }
        let mut frames = ffi::av_hwframe_ctx_alloc(device);
        ffi::av_buffer_unref(&mut device);
        let context = (*frames).data.cast::<ffi::AVHWFramesContext>();
        (*context).format = format.into();
        (*context).sw_format = ffi::AVPixelFormat::AV_PIX_FMT_NV12;
        (*context).width = meta.width as i32;
        (*context).height = meta.height as i32;
        let code = ffi::av_hwframe_ctx_init(frames);
        if code < 0 {
            ffi::av_buffer_unref(&mut frames);
            return Err(ffmpeg_next::Error::from(code).into());
        }
        (*video.as_mut_ptr()).hw_frames_ctx = frames;
    }
    Ok(())
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

// call it after the encoder of the file is open, nvenc fails to open at the same time
fn engines() -> usize {
    #[repr(C)]
    struct Guid(u32, u16, u16, [u8; 8]);
    #[repr(C)]
    struct List {
        version: u32,
        reserved: u32,
        functions: [*const c_void; 318],
    }
    #[repr(C)]
    struct Session {
        version: u32,
        kind: u32,
        device: *mut c_void,
        reserved: *mut c_void,
        api: u32,
        rest: [u32; 253],
        more: [*mut c_void; 64],
    }
    #[repr(C)]
    struct Caps {
        version: u32,
        query: u32,
        reserved: [u32; 62],
    }
    type Open = unsafe extern "C" fn(*mut Session, *mut *mut c_void) -> u32;
    type Ask = unsafe extern "C" fn(*mut c_void, Guid, *mut Caps, *mut i32) -> u32;
    type Close = unsafe extern "C" fn(*mut c_void) -> u32;
    // the first api version that tells the number of chips
    const API: u32 = 12 | 1 << 24;
    const fn version(of: u32) -> u32 {
        API | of << 16 | 0x7 << 28
    }
    // places in the function list, and the id of the question
    const ASK: usize = 7;
    const CLOSE: usize = 27;
    const OPEN: usize = 29;
    const CHIPS: u32 = 49;
    const H264: Guid = Guid(
        0x6bc8_2762,
        0x4e63,
        0x4ca4,
        [0xaa, 0x85, 0x1e, 0x50, 0xf3, 0x21, 0xf6, 0xbf],
    );
    static CHIPS_FOUND: OnceLock<usize> = OnceLock::new();
    let ask = || unsafe {
        let names = match cfg!(windows) {
            true => ("nvcuda.dll", "nvEncodeAPI64.dll"),
            false => ("libcuda.so.1", "libnvidia-encode.so.1"),
        };
        let cuda = libloading::Library::new(names.0).ok()?;
        let nvenc = libloading::Library::new(names.1).ok()?;
        let init = cuda.get::<unsafe extern "C" fn(u32) -> i32>(b"cuInit\0");
        let device = cuda.get::<unsafe extern "C" fn(*mut i32, i32) -> i32>(b"cuDeviceGet\0");
        let create = cuda
            .get::<unsafe extern "C" fn(*mut *mut c_void, u32, i32) -> i32>(b"cuCtxCreate_v2\0");
        let destroy = cuda.get::<unsafe extern "C" fn(*mut c_void) -> i32>(b"cuCtxDestroy_v2\0");
        let (init, device, create, destroy) =
            (init.ok()?, device.ok()?, create.ok()?, destroy.ok()?);
        let instance =
            nvenc.get::<unsafe extern "C" fn(*mut List) -> u32>(b"NvEncodeAPICreateInstance\0");
        let instance = instance.ok()?;
        let (mut card, mut context) = (0, std::ptr::null_mut());
        if init(0) != 0 || device(&raw mut card, 0) != 0 || create(&raw mut context, 0, card) != 0 {
            return None;
        }
        let mut list = List {
            version: version(2),
            reserved: 0,
            functions: [std::ptr::null(); 318],
        };
        let mut session = Session {
            version: version(1),
            kind: 1,
            device: context,
            reserved: std::ptr::null_mut(),
            api: API,
            rest: [0; 253],
            more: [std::ptr::null_mut(); 64],
        };
        let mut caps = Caps {
            version: version(1),
            query: CHIPS,
            reserved: [0; 62],
        };
        let (mut encoder, mut chips) = (std::ptr::null_mut(), 0);
        let mut found = None;
        if instance(&raw mut list) == 0 && !list.functions[OPEN].is_null() {
            let open: Open = std::mem::transmute(list.functions[OPEN]);
            let ask: Ask = std::mem::transmute(list.functions[ASK]);
            let close: Close = std::mem::transmute(list.functions[CLOSE]);
            let opened = open(&raw mut session, &raw mut encoder) == 0;
            if opened && ask(encoder, H264, &raw mut caps, &raw mut chips) == 0 {
                found = Some(chips.max(1) as usize);
            }
            // the api wants a close for a session that did not open, too
            if !encoder.is_null() {
                close(encoder);
            }
        }
        destroy(context);
        found
    };
    *CHIPS_FOUND.get_or_init(|| ask().unwrap_or(1))
}

pub type Spare = Arc<dyn Fn() -> Result<encoder::video::Encoder> + Send + Sync>;

fn head(video: &encoder::video::Encoder) -> Vec<u8> {
    let context = unsafe { &*video.as_ptr() };
    match context.extradata.is_null() {
        true => Vec::new(),
        false => unsafe {
            std::slice::from_raw_parts(context.extradata, context.extradata_size as usize).to_vec()
        },
    }
}

fn context(
    name: &str,
    meta: &Meta,
    base: Rational,
    config: &Output,
    rate: Option<usize>,
    header: bool,
) -> Option<std::result::Result<encoder::video::Encoder, String>> {
    let codec = encoder::find_by_name(name)?;
    let mut video = codec::Context::new_with_codec(codec)
        .encoder()
        .video()
        .ok()?;
    video.set_width(meta.width);
    video.set_height(meta.height);
    let yuv444 = codec
        .video()
        .ok()
        .and_then(|video| video.formats())
        .is_some_and(|mut formats| formats.any(|format| format == Pixel::YUV444P));
    let format = match name.rsplit('_').next() {
        Some("qsv") => Pixel::NV12,
        Some("vaapi") => Pixel::VAAPI,
        Some("vulkan") => Pixel::VULKAN,
        _ if meta.yuv444 && yuv444 => Pixel::YUV444P,
        _ => Pixel::YUV420P,
    };
    video.set_format(format);
    if matches!(format, Pixel::VAAPI | Pixel::VULKAN)
        && let Err(error) = surfaces(&mut video, meta, format)
    {
        return Some(Err(format!("{name}: {error}")));
    }
    video.set_time_base(base);
    video.set_frame_rate(Some(base.invert()));
    video.set_gop(GOP);
    unsafe { (*video.as_mut_ptr()).thread_count = budget::own() as i32 };
    video.set_colorspace(meta.space);
    video.set_color_range(meta.range);
    video.set_color_primaries(meta.primaries);
    video.set_color_transfer_characteristic(meta.transfer);
    if header {
        video.set_flags(codec::Flags::GLOBAL_HEADER);
    }
    if let Some(rate) = rate {
        video.set_bit_rate(rate);
    }
    // svt-av1 takes a top rate only with a constant quality
    if let Some(rate) = rate.filter(|_| name != "libsvtav1") {
        video.set_max_bit_rate(rate);
        // one second of the rate, with more the file can go over its size
        unsafe { (*video.as_mut_ptr()).rc_buffer_size = rate as i32 };
    }
    let opened = video.open_with(options(name, config, rate.is_some()));
    Some(opened.map_err(|error| format!("{name}: {error}")))
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
    convert: Option<scaling::Context>,
    packet: Packet,
    base: Rational,
    spare: Option<Spare>,
    busy: bool,
    pub name: String,
    pub yuv444: bool,
    pub packets: [(u32, u64); 2],
}

impl Encoder {
    pub fn open(
        mut output: format::context::Output,
        meta: &Meta,
        fps: (i64, i64),
        config: &Output,
        rate: Option<usize>,
        plan: Plan,
    ) -> Result<Self> {
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
        // the encoders that do not fit this computer fail without a word
        // nvenc lists 4:4:4 for av1 as well, but the gpu has none, so each encoder tries 4:2:0 after it
        let plain = Meta {
            yuv444: false,
            ..*meta
        };
        let mut open = |level| {
            ffmpeg_next::log::set_level(level);
            names.iter().find_map(|&name| {
                let mut tries = [meta, &plain]
                    .into_iter()
                    .take(1 + usize::from(meta.yuv444));
                tries.find_map(
                    |meta| match context(name, meta, base, config, rate, header)? {
                        Ok(encoder) => Some((name, encoder)),
                        Err(error) => {
                            failure = error;
                            None
                        }
                    },
                )
            })
        };
        let opened = open(Level::Quiet).or_else(|| open(Level::Error));
        ffmpeg_next::log::set_level(Level::Error);
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
        // the gpu surfaces take nv12
        let target = match encoder.format() {
            Pixel::NV12 | Pixel::VAAPI | Pixel::VULKAN => Pixel::NV12,
            format => format,
        };
        let convert = match target == meta.pixel() {
            true => None,
            false => {
                let (width, height) = (meta.width, meta.height);
                let flags = scaling::Flags::BILINEAR;
                Some(scaling::Context::get(
                    meta.pixel(),
                    width,
                    height,
                    target,
                    width,
                    height,
                    flags,
                )?)
            }
        };
        // nvenc starts again after a flush, and a size target needs one encoder for the whole file
        // the lanes give their pictures to it as they are, so it must take them without a change
        let spare =
            (name.ends_with("_nvenc") && rate.is_none() && engines() > 1 && convert.is_none())
                .then(|| {
                    let (name, meta, config, first) =
                        (name.to_owned(), *meta, config.clone(), head(&encoder));
                    let spare: Spare = Arc::new(move || {
                        let video = context(&name, &meta, base, &config, None, header);
                        let video = video.ok_or("no encoder")??;
                        match head(&video) == first {
                            true => Ok(video),
                            false => Err("the encoders do not match".into()),
                        }
                    });
                    spare
                });
        Ok(Self {
            output,
            encoder,
            audio,
            lanes,
            source: None,
            mixer,
            convert,
            packet: Packet::empty(),
            base,
            spare,
            busy: false,
            name: name.to_owned(),
            yuv444: target == Pixel::YUV444P,
            packets: [(0, 0); 2],
        })
    }

    fn mux(&mut self) -> Result<()> {
        let target = self
            .output
            .stream(0)
            .map_or(self.base, |stream| stream.time_base());
        let seconds = self.packet.dts().unwrap_or(0) as f64 * f64::from(self.base);
        if let Some(audio) = &mut self.audio {
            audio.copy(&mut self.output, seconds)?;
        }
        let kind = &mut self.packets[usize::from(self.packet.is_key())];
        *kind = (kind.0 + 1, kind.1 + self.packet.size() as u64);
        self.packet.rescale_ts(self.base, target);
        self.packet.set_stream(0);
        self.packet.write_interleaved(&mut self.output)?;
        Ok(())
    }

    fn drain(&mut self) -> Result<()> {
        while self.encoder.receive_packet(&mut self.packet).is_ok() {
            self.mux()?;
        }
        Ok(())
    }

    pub fn spare(&self) -> Option<Spare> {
        self.spare.clone()
    }

    pub fn pass(&mut self, packets: Vec<Packet>, n: i32) -> Result<()> {
        if self.busy {
            self.encoder.send_eof()?;
            self.drain()?;
            unsafe { ffi::avcodec_flush_buffers(self.encoder.as_mut_ptr()) };
            self.busy = false;
        }
        for packet in packets {
            self.packet = packet;
            self.mux()?;
        }
        self.pump(f64::from(n + 1) * f64::from(self.base))
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
        if let Some(scaler) = &mut self.convert {
            let mut converted = frame::Video::empty();
            scaler.run(&picture, &mut converted)?;
            picture = converted;
        }
        let frames = unsafe { (*self.encoder.as_ptr()).hw_frames_ctx };
        if !frames.is_null() {
            let mut surface = frame::Video::empty();
            let code = unsafe {
                match ffi::av_hwframe_get_buffer(frames, surface.as_mut_ptr(), 0) {
                    0 => ffi::av_hwframe_transfer_data(surface.as_mut_ptr(), picture.as_ptr(), 0),
                    code => code,
                }
            };
            if code < 0 {
                return Err(ffmpeg_next::Error::from(code).into());
            }
            picture = surface;
        }
        picture.set_pts(Some(i64::from(n)));
        if !self.busy && self.spare.is_some() {
            picture.set_kind(picture::Type::I);
        }
        self.busy = true;
        self.encoder.send_frame(&picture)?;
        self.drain()?;
        self.pump(f64::from(n + 1) * f64::from(self.base))
    }

    pub fn finish(&mut self, seconds: f64) -> Result<()> {
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
