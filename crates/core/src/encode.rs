use std::ffi::c_void;
use std::path::Path;
use std::sync::{Arc, OnceLock};

use ffmpeg_next::format::{self, Pixel};
use ffmpeg_next::log::Level;
use ffmpeg_next::{Dictionary, Packet, Rational, codec, encoder, ffi, frame, media, picture};
use rayon::prelude::*;

use crate::Result;
use crate::audio::{Lane, Mixer, RATE, Source, name};
use crate::budget;
use crate::compose::Scaler;
use crate::config::{self, Codec, Effort, Output};
use crate::decode::{Meta, absolute, title};
use crate::munch::{Munch, crush};

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

// the setting of each encoder for the four efforts, fastest first
fn presets(name: &str) -> Option<(&'static str, [&'static str; 4])> {
    match name.rsplit('_').next() {
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
    }
}

// the hardware encoders that open on this system, found once
pub(crate) static OPENS: OnceLock<Vec<&'static str>> = OnceLock::new();

pub fn find_encoders() {
    OPENS.get_or_init(|| {
        let meta = Meta {
            width: 1280,
            height: 720,
            ..Meta::standard()
        };
        let codecs = [Codec::H264, Codec::Hevc, Codec::Av1];
        let names = codecs.into_iter().flat_map(|codec| {
            let output = Output {
                codec,
                encoder: config::Encoder::Hardware,
                ..Output::default()
            };
            candidates(&output).collect::<Vec<_>>()
        });
        ffmpeg_next::log::set_level(Level::Quiet);
        let base = Rational(1, 60);
        let found: Vec<&str> = names
            .filter(|name| {
                context(name, &meta, base, &Output::default(), None, false, false)
                    .is_some_and(|c| c.is_ok())
            })
            .collect();
        ffmpeg_next::log::set_level(Level::Error);
        if found.iter().any(|name| name.ends_with("_nvenc")) {
            engines();
        }
        found
    });
}

// what the effort sets on the encoder that a render picks, or what the options set in its place
pub fn effort(output: &Output, effort: Effort) -> String {
    let keys = ["preset", "quality", "compression_level"];
    if let Some((key, value)) = pairs(&output.options).find(|pair| keys.contains(&pair.0)) {
        return format!("Options set {key} {value}");
    }
    let Some(opens) = OPENS.get() else {
        return String::new();
    };
    let chosen = pairs(&output.options)
        .find(|pair| pair.0 == "c:v")
        .map(|pair| pair.1);
    let picked = chosen.or_else(|| {
        candidates(output).find(|name| name.starts_with("lib") || opens.contains(name))
    });
    let Some(name) = picked else {
        return "No hardware encoder for this codec".into();
    };
    let short = match name.rsplit('_').next() {
        Some("nvenc") => "NVENC",
        Some("amf") => "AMF",
        Some("qsv") => "QSV",
        Some("vaapi") => "VAAPI",
        Some("vulkan") => "Vulkan",
        Some("videotoolbox") => "VideoToolbox",
        _ if name == "libsvtav1" => "SVT-AV1",
        _ => name.trim_start_matches("lib"),
    };
    let Some((key, values)) = presets(name) else {
        return format!("{short} has no effort levels");
    };
    let value = values[effort as usize];
    match value.parse::<u32>() {
        Ok(_) if key == "preset" => format!("{short} preset {value}"),
        Ok(_) => format!("{short} level {value}"),
        Err(_) => format!("{short} {value}"),
    }
}

// videotoolbox reads 0 to 100 with 100 the best, in lambda units
pub(crate) fn toolbox(quality: u32) -> u32 {
    100u32.saturating_sub(2 * quality) * ffi::FF_QP2LAMBDA as u32
}

fn options(name: &str, output: &Output, sized: bool, packed: bool) -> Dictionary<'static> {
    // these av1 encoders count the quantizer to 255, the others to 51 or 63
    let wide = name.starts_with("av1_") && !name.ends_with("_nvenc");
    let quality = (output.quality * if wide { 4 } else { 1 }).to_string();
    let scaled = toolbox(output.quality).to_string();
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
        Some("videotoolbox") => &[("flags", "+qscale"), ("global_quality", &scaled)],
        _ => &[("crf", &quality)],
    };
    let preset = presets(name).map(|(key, presets)| (key, presets[output.effort as usize]));
    // a card with two encoders uses both for one picture, h264 has no such mode
    let split = (family == Some("nvenc") && !name.starts_with("h264"))
        .then_some(("split_encode_mode", "forced"));
    // after a flush the next picture must be an idr
    let fresh = (family == Some("nvenc")).then_some(("forced-idr", "1"));
    // b-frames reorder packets across engines
    let ordered = (family == Some("nvenc") && packed).then_some(("bf", "0"));
    let pairs = preset.into_iter().chain(rate.iter().copied());
    let pairs = pairs.chain(split);
    let mut options = Dictionary::new();
    // a part of a reused encoder must start with an idr, and packed parts must stay in order, whatever the options say
    let forced = ordered.into_iter().chain(fresh);
    for (key, value) in pairs.chain(self::pairs(&output.options)).chain(forced) {
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
        if frames.is_null() {
            return Err("not enough memory".into());
        }
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
        let mut input = format::input(&absolute(clip))?;
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
            name(&mut target, &title(&stream));
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
            if start > 0.0 {
                ffi::av_seek_frame(context, -1, offset, ffi::AVSEEK_FLAG_BACKWARD);
            }
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
            let Some(target) = self.streams.get(self.packet.stream()).copied().flatten() else {
                continue;
            };
            let base = self
                .input
                .stream(self.packet.stream())
                .map(|stream| stream.time_base());
            let base = base.unwrap_or(Rational(1, 1));
            let time =
                self.packet.pts().or(self.packet.dts()).unwrap_or(0) as f64 * f64::from(base);
            let length = self.packet.duration() as f64 * f64::from(base);
            if time + length <= self.offset {
                continue;
            }
            self.held = time >= seconds + self.offset;
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

static ENGINES: OnceLock<usize> = OnceLock::new();

fn two_engines() -> bool {
    engines() > 1
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
        if instance(&raw mut list) == 0
            && [OPEN, ASK, CLOSE]
                .iter()
                .all(|&place| !list.functions[place].is_null())
        {
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
    *ENGINES.get_or_init(|| ask().unwrap_or(1))
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

fn threads(name: &str) -> usize {
    let own = budget::own();
    match name {
        "libx264" | "libx265" => own.min(16),
        _ => own,
    }
}

pub(crate) fn context(
    name: &str,
    meta: &Meta,
    base: Rational,
    config: &Output,
    rate: Option<usize>,
    header: bool,
    packed: bool,
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
    unsafe { (*video.as_mut_ptr()).thread_count = threads(name) as i32 };
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
        unsafe { (*video.as_mut_ptr()).rc_buffer_size = rate.min(i32::MAX as usize) as i32 };
    }
    let opened = video.open_with(options(name, config, rate.is_some(), packed));
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
        titles: Vec<String>,
    },
    // the sounds of a timeline, added together
    Mix(Mixer),
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn pack_nv12_row_avx2(u: &[u8], v: &[u8], uv: &mut [u8]) {
    use std::arch::x86_64::{
        _mm256_loadu_si256, _mm256_permute2x128_si256, _mm256_storeu_si256, _mm256_unpackhi_epi8,
        _mm256_unpacklo_epi8,
    };
    let width = u.len();
    let mut x = 0;
    while x + 32 <= width {
        unsafe {
            let vu = _mm256_loadu_si256(u.as_ptr().add(x).cast());
            let vv = _mm256_loadu_si256(v.as_ptr().add(x).cast());
            let lo = _mm256_unpacklo_epi8(vu, vv);
            let hi = _mm256_unpackhi_epi8(vu, vv);
            let out0 = _mm256_permute2x128_si256::<0x20>(lo, hi);
            let out1 = _mm256_permute2x128_si256::<0x31>(lo, hi);
            _mm256_storeu_si256(uv.as_mut_ptr().add(x * 2).cast(), out0);
            _mm256_storeu_si256(uv.as_mut_ptr().add(x * 2 + 32).cast(), out1);
        }
        x += 32;
    }
    for i in x..width {
        uv[i * 2] = u[i];
        uv[i * 2 + 1] = v[i];
    }
}

#[cfg(target_arch = "aarch64")]
fn pack_nv12_row_neon(u: &[u8], v: &[u8], uv: &mut [u8]) {
    use std::arch::aarch64::{uint8x16x2_t, vld1q_u8, vst2q_u8};
    let width = u.len();
    let mut x = 0;
    while x + 16 <= width {
        unsafe {
            let vu = vld1q_u8(u.as_ptr().add(x));
            let vv = vld1q_u8(v.as_ptr().add(x));
            vst2q_u8(uv.as_mut_ptr().add(x * 2), uint8x16x2_t(vu, vv));
        }
        x += 16;
    }
    for i in x..width {
        uv[i * 2] = u[i];
        uv[i * 2 + 1] = v[i];
    }
}

fn pack_nv12_row(u: &[u8], v: &[u8], uv: &mut [u8], #[cfg(target_arch = "x86_64")] avx2: bool) {
    #[cfg(target_arch = "x86_64")]
    if avx2 {
        unsafe { pack_nv12_row_avx2(u, v, uv) };
        return;
    }
    #[cfg(target_arch = "aarch64")]
    {
        pack_nv12_row_neon(u, v, uv);
        return;
    }
    #[allow(unreachable_code)]
    for ((&u, &v), dst) in u.iter().zip(v).zip(uv.as_chunks_mut::<2>().0) {
        *dst = [u, v];
    }
}

fn pack_nv12(src: &frame::Video, dst: &mut frame::Video) {
    let (width, height) = ((src.width() as usize) & !1, (src.height() as usize) & !1);
    let (cw, ch) = (width / 2, height / 2);
    let (sy, su, sv) = (src.stride(0), src.stride(1), src.stride(2));
    let (dy, duv) = (dst.stride(0), dst.stride(1));
    let (src_y, src_u, src_v) = (src.data(0), src.data(1), src.data(2));
    let raw = unsafe { &*dst.as_ptr() };
    let (dst_y, dst_uv) = unsafe {
        (
            std::slice::from_raw_parts_mut(raw.data[0], dy * height),
            std::slice::from_raw_parts_mut(raw.data[1], duv * ch),
        )
    };
    #[cfg(target_arch = "x86_64")]
    let avx2 = std::arch::is_x86_feature_detected!("avx2");
    if width * height >= 1920 * 1080 {
        let piece = budget::piece(ch);
        budget::install(|| {
            dst_y
                .par_chunks_mut(dy * 2)
                .take(ch)
                .zip(dst_uv.par_chunks_mut(duv).take(ch))
                .enumerate()
                .with_min_len(piece)
                .for_each(|(cy, (y_pair, uv_row))| {
                    let (y0, y1) = y_pair.split_at_mut(dy);
                    let row = cy * 2;
                    y0[..width].copy_from_slice(&src_y[row * sy..][..width]);
                    y1[..width].copy_from_slice(&src_y[(row + 1) * sy..][..width]);
                    pack_nv12_row(
                        &src_u[cy * su..][..cw],
                        &src_v[cy * sv..][..cw],
                        &mut uv_row[..cw * 2],
                        #[cfg(target_arch = "x86_64")]
                        avx2,
                    );
                });
        });
    } else {
        if sy == width && dy == width {
            dst_y[..width * height].copy_from_slice(&src_y[..width * height]);
        } else {
            for row in 0..height {
                dst_y[row * dy..][..width].copy_from_slice(&src_y[row * sy..][..width]);
            }
        }
        for cy in 0..ch {
            pack_nv12_row(
                &src_u[cy * su..][..cw],
                &src_v[cy * sv..][..cw],
                &mut dst_uv[cy * duv..][..cw * 2],
                #[cfg(target_arch = "x86_64")]
                avx2,
            );
        }
    }
}

struct FramePool {
    format: Pixel,
    width: u32,
    height: u32,
    frames: Vec<frame::Video>,
}

impl FramePool {
    fn new(format: Pixel, width: u32, height: u32) -> Self {
        Self {
            format,
            width,
            height,
            frames: vec![frame::Video::new(format, width, height)],
        }
    }

    fn acquire(&mut self) -> frame::Video {
        if let Some(pos) = self
            .frames
            .iter()
            .position(|f| unsafe { ffi::av_frame_is_writable(f.as_ptr().cast_mut()) != 0 })
        {
            self.frames.swap_remove(pos)
        } else {
            frame::Video::new(self.format, self.width, self.height)
        }
    }

    fn release(&mut self, frame: frame::Video) {
        if self.frames.len() < 8 {
            self.frames.push(frame);
        }
    }
}

enum Convert {
    Nv12(FramePool),
    Scale(Scaler, FramePool),
}

pub struct Encoder {
    output: format::context::Output,
    encoder: encoder::video::Encoder,
    audio: Option<Audio>,
    lanes: Vec<Lane>,
    source: Option<Source>,
    mixer: Option<Mixer>,
    convert: Option<Convert>,
    meta: Meta,
    packet: Packet,
    base: Rational,
    spare: Option<Spare>,
    busy: bool,
    pub name: String,
    pub yuv444: bool,
    pub munch: Option<Munch>,
    held: Vec<[f32; 2]>,
    pub packets: [(u32, u64); 2],
    shown: i64,
    _software: Option<budget::SoftwareEncode>,
}

unsafe impl Send for Encoder {}

// true for a packet, false when the encoder has nothing more for now
pub(crate) fn received(result: std::result::Result<(), ffmpeg_next::Error>) -> Result<bool> {
    match result {
        Ok(()) => Ok(true),
        Err(ffmpeg_next::Error::Eof)
        | Err(ffmpeg_next::Error::Other {
            errno: ffmpeg_next::error::EAGAIN,
        }) => Ok(false),
        Err(error) => Err(error.into()),
    }
}

impl Encoder {
    pub fn open(
        mut output: format::context::Output,
        meta: &Meta,
        fps: (i64, i64),
        config: &Output,
        rate: Option<usize>,
        plan: Plan,
        packed: bool,
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
            None => match OPENS.get() {
                Some(opens) => candidates(config)
                    .filter(|name| name.starts_with("lib") || opens.contains(name))
                    .collect(),
                None => candidates(config).collect(),
            },
        };
        let pack = |name: &str| {
            packed && rate.is_none() && name.ends_with("_nvenc") && two_engines()
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
                let packed = pack(name);
                let mut tries = [meta, &plain]
                    .into_iter()
                    .take(1 + usize::from(meta.yuv444));
                tries.find_map(|meta| {
                    match context(name, meta, base, config, rate, header, packed)? {
                        Ok(encoder) => Some((name, encoder)),
                        Err(error) => {
                            failure = error;
                            None
                        }
                    }
                })
            })
        };
        let opened = open(Level::Quiet).or_else(|| open(Level::Error));
        ffmpeg_next::log::set_level(Level::Error);
        let (name, encoder) = opened.ok_or(failure)?;
        let _software = (name.starts_with("lib") && budget::current() == budget::Class::Render)
            .then(|| budget::SoftwareEncode::new(threads(name)));
        output
            .add_stream(encoder::find_by_name(name))?
            .set_parameters(&encoder);
        if let Some(matrix) = meta.matrix {
            let stream = output.stream_mut(0).ok_or("no stream")?;
            let parameters = unsafe { &mut *stream.parameters().as_mut_ptr() };
            let side = unsafe {
                ffi::av_packet_side_data_new(
                    &raw mut parameters.coded_side_data,
                    &raw mut parameters.nb_coded_side_data,
                    ffi::AVPacketSideDataType::AV_PKT_DATA_DISPLAYMATRIX,
                    matrix.len(),
                    0,
                )
            };
            if !side.is_null() {
                unsafe {
                    std::ptr::copy_nonoverlapping(matrix.as_ptr(), (*side).data, matrix.len())
                };
            }
        }
        let mut lanes = Vec::new();
        let mut mixer = None;
        let audio = match plan {
            Plan::Copy { clip, start, muted } => Audio::open(clip, &mut output, start, muted)?,
            Plan::Encode { titles } => {
                for title in titles {
                    lanes.push(Lane::open(&mut output, &title)?);
                }
                None
            }
            Plan::Mix(sounds) => {
                for lane in 0..sounds.lanes() {
                    lanes.push(Lane::open(&mut output, sounds.title(lane))?);
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
        let convert = if target == meta.pixel() {
            None
        } else if target == Pixel::NV12 && meta.pixel() == Pixel::YUV420P {
            Some(Convert::Nv12(FramePool::new(
                Pixel::NV12,
                meta.width,
                meta.height,
            )))
        } else {
            Some(Convert::Scale(
                Scaler::new(),
                FramePool::new(target, meta.width, meta.height),
            ))
        };
        // nvenc starts again after a flush, and a size target needs one encoder for the whole file
        // the lanes give their pictures to it as they are, so it must take them without a change
        let spare: Option<Spare> =
            (pack(name) && convert.is_none()).then(|| {
                let (name, meta, config, first) =
                    (name.to_owned(), *meta, config.clone(), head(&encoder));
                Arc::new(move || {
                    let video = context(&name, &meta, base, &config, None, header, true);
                    let video = video.ok_or("no encoder")??;
                    match head(&video) == first {
                        true => Ok(video),
                        false => Err("the encoders do not match".into()),
                    }
                }) as Spare
            });
        Ok(Self {
            output,
            encoder,
            audio,
            held: vec![[0.0; 2]; lanes.len()],
            lanes,
            source: None,
            mixer,
            convert,
            meta: *meta,
            packet: Packet::empty(),
            base,
            spare,
            busy: false,
            name: name.to_owned(),
            yuv444: target == Pixel::YUV444P,
            munch: None,
            packets: [(0, 0); 2],
            shown: 0,
            _software,
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
        self.shown = self.shown.max(self.packet.pts().unwrap_or(0) + 1);
        let kind = &mut self.packets[usize::from(self.packet.is_key())];
        *kind = (kind.0 + 1, kind.1 + self.packet.size() as u64);
        self.packet.set_duration(1);
        self.packet.rescale_ts(self.base, target);
        self.packet.set_stream(0);
        self.packet.write_interleaved(&mut self.output)?;
        Ok(())
    }

    fn drain(&mut self) -> Result<()> {
        while received(self.encoder.receive_packet(&mut self.packet))? {
            self.mux()?;
        }
        Ok(())
    }

    pub fn spare(&self) -> Option<Spare> {
        self.spare.clone()
    }

    pub fn pass(&mut self, packets: Vec<Packet>) -> Result<()> {
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
        // a worker can still hold the packets of these frames, so the sound waits for them
        self.pump(self.shown as f64 * f64::from(self.base))
    }

    pub fn begin(&mut self, clip: &Path, start: f64, muted: &[usize]) -> Result<()> {
        if !self.lanes.is_empty() && self.mixer.is_none() {
            self.source = Some(Source::open(clip, start, 1.0, |track| {
                !muted.contains(&track)
            })?);
        }
        Ok(())
    }

    fn feed(&mut self, samples: i64) -> Result<()> {
        let mix = self.lanes.len() == 1;
        for (index, lane) in self.lanes.iter_mut().enumerate() {
            let track = (!mix).then_some(index);
            let count = lane.size().min((samples - lane.position()).max(0) as usize);
            let samples = match (&mut self.mixer, &mut self.source) {
                (Some(mixer), _) => {
                    let mut samples = mixer.take(index, lane.position(), count)?;
                    if let Some(munch) = &self.munch {
                        crush(&mut samples, lane.position(), munch, &mut self.held[index]);
                    }
                    samples
                }
                (None, Some(source)) => source.take(track, count)?,
                (None, None) => [vec![0.0; count], vec![0.0; count]],
            };
            if count > 0 {
                lane.write(&mut self.output, samples)?;
            }
        }
        Ok(())
    }

    fn pump(&mut self, seconds: f64) -> Result<()> {
        while self.lanes.iter().any(|lane| lane.seconds() <= seconds) {
            self.feed(i64::MAX)?;
        }
        Ok(())
    }

    fn release(&mut self, frame: frame::Video) {
        match &mut self.convert {
            Some(Convert::Nv12(pool) | Convert::Scale(_, pool)) => pool.release(frame),
            None => {}
        }
    }

    pub fn write(&mut self, mut picture: frame::Video, n: i32) -> Result<()> {
        let mut pooled = false;
        match &mut self.convert {
            Some(Convert::Nv12(pool)) if picture.format() != Pixel::NV12 => {
                let mut dst = pool.acquire();
                pack_nv12(&picture, &mut dst);
                self.meta.tag(&mut dst);
                picture = dst;
                pooled = true;
            }
            Some(Convert::Scale(scaler, pool)) => {
                let mut dst = pool.acquire();
                self.meta.tag(&mut dst);
                scaler.run(&mut dst, &picture)?;
                picture = dst;
                pooled = true;
            }
            _ => {}
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
            if pooled {
                self.release(picture);
                pooled = false;
            }
            if code < 0 {
                return Err(ffmpeg_next::Error::from(code).into());
            }
            picture = surface;
        }
        picture.set_pts(Some(i64::from(n)));
        picture.set_kind(if !self.busy && self.spare.is_some() {
            picture::Type::I
        } else {
            picture::Type::None
        });
        self.busy = true;
        let sent = self.encoder.send_frame(&picture);
        if pooled {
            self.release(picture);
        }
        sent?;
        self.drain()?;
        self.pump(f64::from(n + 1) * f64::from(self.base))
    }

    pub fn finish(&mut self, frames: i32) -> Result<()> {
        self.encoder.send_eof()?;
        self.drain()?;
        // the lanes of a stopped render can still hold every picture that was sent
        if self.packets.iter().all(|kind| kind.0 == 0) {
            return Err("stopped".into());
        }
        // the sound ends with the last picture in the file
        let seconds = self.shown.min(i64::from(frames)) as f64 * f64::from(self.base);
        if let Some(audio) = &mut self.audio {
            audio.copy(&mut self.output, seconds)?;
        }
        let samples = (seconds * f64::from(RATE)).round() as i64;
        while self.lanes.iter().any(|lane| lane.position() < samples) {
            self.feed(samples)?;
        }
        for lane in &mut self.lanes {
            lane.finish(&mut self.output)?;
        }
        self.output.write_trailer()?;
        Ok(())
    }
}
