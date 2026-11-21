use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, TryRecvError};
use std::sync::{Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::SystemTime;

use ffmpeg_next::codec::{self, threading};
use ffmpeg_next::format::{self, Pixel};
use ffmpeg_next::log::Level;
use ffmpeg_next::packet::Mut;
use ffmpeg_next::{Packet, color, ffi, frame, media};

use crate::budget::{self, Class};
use crate::compose::share;
use crate::svp::Reader;
use crate::work::{self, Follow};
use crate::{Error, Result};

#[derive(Clone, Copy)]
pub struct Meta {
    pub width: u32,
    pub height: u32,
    pub fps: (i64, i64),
    pub frames: i32,
    pub space: color::Space,
    pub range: color::Range,
    pub primaries: color::Primaries,
    pub transfer: color::TransferCharacteristic,
    pub still: bool,
    pub codec: &'static str,
    // the color planes are as large as the picture
    pub yuv444: bool,
    // the display matrix turns or flips the picture
    pub matrix: Option<[u8; 36]>,
    pub cropped: bool,
}

fn matrix(stream: &format::stream::Stream) -> Option<[u8; 36]> {
    let parameters = unsafe { &*stream.parameters().as_ptr() };
    let data = unsafe {
        ffi::av_packet_side_data_get(
            parameters.coded_side_data,
            parameters.nb_coded_side_data,
            ffi::AVPacketSideDataType::AV_PKT_DATA_DISPLAYMATRIX,
        )
        .as_ref()?
    };
    let bytes = unsafe { std::slice::from_raw_parts(data.data, data.size) };
    bytes.try_into().ok()
}

pub struct Scale {
    raw: *mut ffi::SwsContext,
    input: (Pixel, u32, u32),
    output: (Pixel, u32, u32),
}

unsafe impl Send for Scale {}

impl Scale {
    pub fn as_mut_ptr(&mut self) -> *mut ffi::SwsContext {
        self.raw
    }
}

impl Drop for Scale {
    fn drop(&mut self) {
        unsafe { ffi::sws_free_context(&mut self.raw) };
    }
}

impl Meta {
    pub(crate) fn standard() -> Self {
        Self {
            width: 0,
            height: 0,
            fps: (30, 1),
            frames: 0,
            space: color::Space::BT709,
            range: color::Range::MPEG,
            primaries: color::Primaries::BT709,
            transfer: color::TransferCharacteristic::BT709,
            still: false,
            codec: "",
            yuv444: false,
            matrix: None,
            cropped: false,
        }
    }

    pub fn pixel(&self) -> Pixel {
        match self.yuv444 {
            true => Pixel::YUV444P,
            false => Pixel::YUV420P,
        }
    }

    // the step from a luma size to a color plane size
    pub fn shift(&self) -> usize {
        usize::from(!self.yuv444)
    }

    pub fn bt601(&self) -> bool {
        match self.space {
            color::Space::BT470BG | color::Space::SMPTE170M => true,
            color::Space::Unspecified => self.height < 720,
            _ => false,
        }
    }

    pub fn full(&self) -> bool {
        self.range == color::Range::JPEG
    }

    pub fn tags(&self) -> (ffi::AVColorSpace, ffi::AVColorRange) {
        let space = match self.bt601() {
            true => ffi::AVColorSpace::AVCOL_SPC_SMPTE170M,
            false => ffi::AVColorSpace::AVCOL_SPC_BT709,
        };
        let range = match self.full() {
            true => ffi::AVColorRange::AVCOL_RANGE_JPEG,
            false => ffi::AVColorRange::AVCOL_RANGE_MPEG,
        };
        (space, range)
    }

    pub fn yuv(&self, color: [u8; 3]) -> [u8; 3] {
        let (kr, kb) = if self.bt601() {
            (0.299, 0.114)
        } else {
            (0.2126, 0.0722)
        };
        let [r, g, b] = color.map(|value| f32::from(value) / 255.0);
        let y = kr * r + (1.0 - kr - kb) * g + kb * b;
        let (u, v) = ((b - y) / (2.0 * (1.0 - kb)), (r - y) / (2.0 * (1.0 - kr)));
        // the limited range starts at 16, and its spans are 219 and 224
        let (offset, luma, chroma) = if self.full() {
            (0.0, 255.0, 255.0)
        } else {
            (16.0, 219.0, 224.0)
        };
        [offset + y * luma, 128.0 + u * chroma, 128.0 + v * chroma].map(|value| value.round() as u8)
    }

    // marks a picture with the color of this clip, so a scaler can bring it to another clip
    pub fn tag(&self, picture: &mut frame::Video) {
        let raw = unsafe { &mut *picture.as_mut_ptr() };
        (raw.colorspace, raw.color_range) = self.tags();
    }

    pub fn scaler(
        &self,
        from: Pixel,
        to: Pixel,
        width: u32,
        height: u32,
    ) -> Result<Scale> {
        let raw = unsafe { ffi::sws_alloc_context() };
        if raw.is_null() {
            return Err("not enough memory".into());
        }
        let standard = if self.bt601() {
            ffi::SWS_CS_ITU601
        } else {
            ffi::SWS_CS_ITU709
        };
        let range = |format| i32::from(rgb(format) || self.full());
        let code = unsafe {
            (*raw).threads = budget::own() as i32;
            (*raw).flags = ffi::SwsFlags::SWS_BILINEAR as u32;
            let mut code = 0;
            for (name, value) in [
                (c"srcw", i64::from(self.width)),
                (c"srch", i64::from(self.height)),
                (
                    c"src_format",
                    i64::from(ffi::AVPixelFormat::from(from) as i32),
                ),
                (c"dstw", i64::from(width)),
                (c"dsth", i64::from(height)),
                (
                    c"dst_format",
                    i64::from(ffi::AVPixelFormat::from(to) as i32),
                ),
            ] {
                code = ffi::av_opt_set_int(raw.cast(), name.as_ptr(), value, 0);
                if code < 0 {
                    break;
                }
            }
            if code >= 0 {
                code = ffi::sws_init_context(raw, std::ptr::null_mut(), std::ptr::null_mut());
                if code >= 0 {
                    let table = ffi::sws_getCoefficients(standard);
                    ffi::sws_setColorspaceDetails(
                        raw,
                        table,
                        range(from),
                        table,
                        range(to),
                        0,
                        1 << 16,
                        1 << 16,
                    );
                }
            }
            code
        };
        if code < 0 {
            let mut ptr = raw;
            unsafe { ffi::sws_free_context(&mut ptr) };
            return Err(ffmpeg_next::Error::from(code).into());
        }
        Ok(Scale {
            raw,
            input: (from, self.width, self.height),
            output: (to, width, height),
        })
    }
}

// ffmpeg reads a name with a colon as a protocol
pub fn absolute(path: &Path) -> PathBuf {
    match path.is_relative() {
        true => Path::new(".").join(path),
        false => path.to_owned(),
    }
}

// a file that cannot seek, such as a raw stream, has its packets counted from its start
fn length(path: &Path, index: usize, start: Option<i64>, ticks: f64) -> Option<(i64, i64)> {
    let open = || format::input(&absolute(path)).ok();
    let mut packet = Packet::empty();
    let mut scan = |input: &mut format::context::Input, once: bool| {
        let (mut first, mut last, mut count) = (None, None, 0);
        while next(&mut packet, input).is_ok() {
            if packet.stream() == index {
                let time = packet.pts().or(packet.dts());
                first = first.or(time);
                last = last.max(time);
                count += 1;
                if once {
                    break;
                }
            }
        }
        (first, last, count)
    };
    let mut input = open()?;
    let first = start.or_else(|| scan(&mut input, true).0);
    // a time that wraps around has its packets counted
    if let Some(first) = first
        && input.seek(i64::MAX, ..).is_ok()
        && let (_, Some(last), _) = scan(&mut input, false)
        && last > first
    {
        return Some((((last - first) as f64 * ticks).round() as i64 + 1, first));
    }
    let (first, _, count) = scan(&mut open()?, false);
    Some((count, start.or(first).unwrap_or(0))).filter(|(count, _)| *count > 0)
}

// newer ffmpeg puts an mp4 track name in name
pub fn title(stream: &format::stream::Stream) -> String {
    let metadata = stream.metadata();
    let found = metadata.get("title").or_else(|| metadata.get("name"));
    found.unwrap_or_default().to_owned()
}

#[derive(Clone)]
pub struct Probe {
    pub seconds: f64,
    pub fps: f64,
    pub tracks: Vec<String>,
    pub width: u32,
    pub height: u32,
    // an image has no length and no frame rate
    pub still: bool,
    pub yuv444: bool,
}

type ProbeKey = (PathBuf, Option<SystemTime>, u64);

fn probe_cache() -> &'static Mutex<BTreeMap<ProbeKey, Probe>> {
    static CACHE: OnceLock<Mutex<BTreeMap<ProbeKey, Probe>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(BTreeMap::new()))
}

fn stream_probe(path: &Path, input: &format::context::Input) -> Result<Option<Probe>> {
    let tracks: Vec<String> = input
        .streams()
        .filter_map(|stream| {
            (stream.parameters().medium() == media::Type::Audio).then(|| title(&stream))
        })
        .collect();
    let total = input.duration().max(0) as f64 / f64::from(ffi::AV_TIME_BASE);
    let Some(stream) = video(input) else {
        if tracks.is_empty() {
            return Err("the clip has no video".into());
        }
        if total <= 0.0 {
            return Err("the clip has no length".into());
        }
        return Ok(Some(Probe {
            seconds: total,
            fps: 0.0,
            tracks,
            width: 0,
            height: 0,
            still: false,
            yuv444: false,
        }));
    };
    let params = unsafe { &*stream.parameters().as_ptr() };
    let width = u32::try_from(params.width).unwrap_or(0) & !1;
    let height = u32::try_from(params.height).unwrap_or(0) & !1;
    let pix = params.format;
    if width == 0
        || height == 0
        || pix < 0
        || pix >= ffi::AVPixelFormat::AV_PIX_FMT_NB as i32
        || codec::decoder::find(stream.parameters().id()).is_none()
    {
        return Ok(None);
    }
    let format = unsafe { ffi::av_pix_fmt_desc_get(std::mem::transmute::<i32, ffi::AVPixelFormat>(pix)).as_ref() };
    let Some(desc) = format else {
        return Ok(None);
    };
    let mut rate = stream.avg_frame_rate();
    if rate.numerator() <= 0 {
        rate = stream.rate();
    }
    if rate.numerator() <= 0 || rate.denominator() <= 0 {
        return Ok(None);
    }
    let real = stream.rate();
    let lossy = real.denominator() > 0
        && f64::from(real) > f64::from(rate) * 1.001
        && f64::from(real) < f64::from(rate) * 1.5;
    if lossy {
        rate = real;
    }
    let rate = rate.reduce();
    let fps = f64::from(rate);
    let time_base = f64::from(stream.time_base());
    let seconds = if stream.duration() > 0 {
        stream.duration() as f64 * time_base
    } else {
        total
    };
    let known = Some(stream.start_time()).filter(|start| *start != ffi::AV_NOPTS_VALUE);
    let container = input.format();
    let still = container.name() == "image2" || container.name().ends_with("_pipe");
    let frames = match stream.frames() {
        frames if frames > 0 && !lossy => frames,
        _ if seconds > 0.0 => (seconds * fps).round() as i64,
        _ if still => 1,
        _ => match length(path, stream.index(), known, time_base * fps) {
            Some((count, _)) => count,
            None => return Ok(None),
        },
    };
    let frames = i32::try_from(frames).unwrap_or(i32::MAX).max(1);
    let yuv444 = desc.log2_chroma_w == 0 && desc.log2_chroma_h == 0;
    Ok(Some(Probe {
        seconds: if still { 0.0 } else { f64::from(frames) / fps },
        fps: if still { 0.0 } else { fps },
        tracks,
        width,
        height,
        still,
        yuv444,
    }))
}

// a file with sound only has a frame rate of 0
pub fn probe(path: &Path) -> Result<Probe> {
    path.to_str()
        .ok_or("the clip name has unsupported characters")?;
    let key = std::fs::metadata(path)
        .ok()
        .map(|meta| (path.to_path_buf(), meta.modified().ok(), meta.len()));
    if let Some(key) = &key
        && let Some(cached) = probe_cache()
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .get(key)
    {
        return Ok(cached.clone());
    }
    ffmpeg_next::init()?;
    let input = format::input(&absolute(path))?;
    let probed = match stream_probe(path, &input)? {
        Some(probed) => probed,
        None => {
            let audio = |stream: &format::stream::Stream| {
                (stream.parameters().medium() == media::Type::Audio).then(|| title(stream))
            };
            let read = |input: &format::context::Input| {
                let tracks: Vec<String> = input
                    .streams()
                    .filter_map(|stream| audio(&stream))
                    .collect();
                (
                    tracks,
                    input.duration().max(0) as f64 / f64::from(ffi::AV_TIME_BASE),
                )
            };
            let (meta, (tracks, length)) = match Decoder::open(path) {
                Ok(decoder) => (Some(decoder.meta), read(&decoder.input)),
                Err(error) => match read(&input) {
                    (tracks, _) if tracks.is_empty() => return Err(error),
                    sound => (None, sound),
                },
            };
            let video = meta.filter(|meta| !meta.still);
            let fps = video.map_or(0.0, |meta| meta.fps.0 as f64 / meta.fps.1 as f64);
            Probe {
                seconds: match (meta, video) {
                    (_, Some(meta)) => f64::from(meta.frames) / fps,
                    (Some(_), None) => 0.0,
                    (None, _) if length > 0.0 => length,
                    (None, _) => return Err("the clip has no length".into()),
                },
                fps,
                tracks,
                width: meta.map_or(0, |meta| meta.width),
                height: meta.map_or(0, |meta| meta.height),
                still: meta.is_some_and(|meta| meta.still),
                yuv444: meta.is_some_and(|meta| meta.yuv444),
            }
        }
    };
    if let Some(key) = key {
        let mut cache = probe_cache()
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if cache.len() >= 256 {
            cache.clear();
        }
        cache.insert(key, probed.clone());
    }
    Ok(probed)
}

// reads the next packet of a file, av_read_frame does not free what the packet held before
pub fn next(
    packet: &mut Packet,
    input: &mut format::context::Input,
) -> std::result::Result<(), ffmpeg_next::Error> {
    unsafe { ffi::av_packet_unref(packet.as_mut_ptr()) };
    packet.read(input)
}

pub fn video(input: &format::context::Input) -> Option<format::stream::Stream<'_>> {
    let cover = |stream: &format::stream::Stream| {
        stream
            .disposition()
            .contains(format::stream::Disposition::ATTACHED_PIC)
    };
    let best = input.streams().best(media::Type::Video);
    best.filter(|stream| !cover(stream)).or_else(|| {
        input
            .streams()
            .find(|stream| stream.parameters().medium() == media::Type::Video && !cover(stream))
    })
}

// the time in a file where its picture starts, in the time base of ffmpeg
// the sound is cut from the same time, so the two stay together
pub fn origin(input: &format::context::Input) -> i64 {
    let video = video(input);
    let start = video.and_then(|stream| match stream.start_time() {
        ffi::AV_NOPTS_VALUE => None,
        start => Some((start as f64 * f64::from(stream.time_base()) * 1e6) as i64),
    });
    start.unwrap_or(unsafe { (*input.as_ptr()).start_time }.max(0))
}

pub(crate) const PAST: usize = 16;
const HISTORY: usize = 192 << 20;
pub(crate) const REFERENCES: usize = 16;
// a shorter step forward is decoded through, a longer one seeks
const NEAR: i32 = 64;
// the frames the gpu has in flight
const FLIGHT: usize = 4;

static DAMAGE: Mutex<BTreeMap<PathBuf, (f64, BTreeSet<i32>)>> = Mutex::new(BTreeMap::new());

pub fn damage(path: &Path) -> Option<(f64, f64)> {
    let mut all = DAMAGE.lock().unwrap_or_else(|poison| poison.into_inner());
    let (rate, frames) = all.remove(path)?;
    let (first, last) = (*frames.first()?, *frames.last()?);
    Some((f64::from(first) / rate, f64::from(last + 1) / rate))
}

pub struct Decoder {
    path: PathBuf,
    input: format::context::Input,
    decoder: codec::decoder::Video,
    scaler: Option<Scale>,
    packet: Packet,
    // the picture that shows now and its number, and the one after it when it is decoded
    frame: frame::Video,
    at: i32,
    past: VecDeque<(i32, frame::Video)>,
    staging: Vec<frame::Video>,
    keep: usize,
    picture: usize,
    // the number that the decoder goes to, the gpu gives only the pictures near it
    wanted: i32,
    ahead: frame::Video,
    after: Option<i32>,
    meta: Meta,
    full: (u32, u32),
    by: u32,
    stream: usize,
    gpu: bool,
    drained: bool,
    // the decoder was full, so the packet goes again
    pending: bool,
    // a seek that gave no picture yet, a stop can leave the file anywhere after it
    lost: bool,
    raw: bool,
    start: i64,
    frames_per_tick: f64,
    cancel: Follow,
}

// the scaler has no thread affinity and the host reads one frame at a time
unsafe impl Send for Decoder {}

// rgb counts too, it has a full color sample for each pixel
fn whole(format: Pixel) -> bool {
    let descriptor = unsafe { ffi::av_pix_fmt_desc_get(format.into()).as_ref() };
    descriptor
        .is_some_and(|descriptor| descriptor.log2_chroma_w == 0 && descriptor.log2_chroma_h == 0)
}

pub fn yuv444(format: Pixel) -> bool {
    matches!(format, Pixel::YUV444P | Pixel::YUVJ444P)
}

// the pictures that go through as they are, in the planes of the pipeline
fn native(format: Pixel, full: bool) -> bool {
    match full {
        true => yuv444(format),
        false => matches!(
            format,
            Pixel::YUV420P | Pixel::YUVJ420P | Pixel::NV12 | Pixel::P010LE | Pixel::YUV420P10LE
        ),
    }
}

pub type Gpu = ffi::AVHWDeviceType;

fn gpus() -> [Gpu; 2] {
    let own = if cfg!(windows) {
        Gpu::AV_HWDEVICE_TYPE_D3D11VA
    } else if cfg!(target_os = "macos") {
        Gpu::AV_HWDEVICE_TYPE_VIDEOTOOLBOX
    } else {
        Gpu::AV_HWDEVICE_TYPE_VAAPI
    };
    [Gpu::AV_HWDEVICE_TYPE_CUDA, own]
}

fn available_gpus() -> &'static [Gpu] {
    static GPUS: OnceLock<Vec<Gpu>> = OnceLock::new();
    GPUS.get_or_init(|| {
        ffmpeg_next::log::set_level(Level::Quiet);
        let found = gpus()
            .into_iter()
            .filter(|&kind| unsafe {
                let mut device = std::ptr::null_mut();
                let ok = ffi::av_hwdevice_ctx_create(
                    &mut device,
                    kind,
                    std::ptr::null(),
                    std::ptr::null_mut(),
                    0,
                ) >= 0;
                if !device.is_null() {
                    ffi::av_buffer_unref(&mut device);
                }
                ok
            })
            .collect();
        ffmpeg_next::log::set_level(Level::Error);
        found
    })
}

fn row(picture: &frame::Video, plane: usize, y: usize) -> &[u8] {
    &picture.data(plane)[y * picture.stride(plane)..]
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn unpack_nv12_row_avx2(src: &[u8], u: &mut [u8], v: &mut [u8]) {
    use std::arch::x86_64::{
        _mm_storeu_si128, _mm256_castsi256_si128, _mm256_extracti128_si256, _mm256_loadu_si256,
        _mm256_permute4x64_epi64, _mm256_setr_epi8, _mm256_shuffle_epi8,
    };
    let width = u.len();
    let shuf = _mm256_setr_epi8(
        0, 2, 4, 6, 8, 10, 12, 14, 1, 3, 5, 7, 9, 11, 13, 15, 0, 2, 4, 6, 8, 10, 12, 14, 1, 3, 5,
        7, 9, 11, 13, 15,
    );
    let mut x = 0;
    while x + 16 <= width {
        unsafe {
            let raw = _mm256_loadu_si256(src.as_ptr().add(x * 2).cast());
            let sep = _mm256_shuffle_epi8(raw, shuf);
            let ord = _mm256_permute4x64_epi64(sep, 0xD8);
            _mm_storeu_si128(u.as_mut_ptr().add(x).cast(), _mm256_castsi256_si128(ord));
            _mm_storeu_si128(
                v.as_mut_ptr().add(x).cast(),
                _mm256_extracti128_si256::<1>(ord),
            );
        }
        x += 16;
    }
    let (pairs, _) = src[x * 2..width * 2].as_chunks::<2>();
    for ((pair, u), v) in pairs.iter().zip(&mut u[x..]).zip(&mut v[x..]) {
        [*u, *v] = *pair;
    }
}

#[cfg(target_arch = "aarch64")]
fn unpack_nv12_row_neon(src: &[u8], u: &mut [u8], v: &mut [u8]) {
    use std::arch::aarch64::{vld2q_u8, vst1q_u8};
    let width = u.len();
    let mut x = 0;
    while x + 16 <= width {
        unsafe {
            let pair = vld2q_u8(src.as_ptr().add(x * 2));
            vst1q_u8(u.as_mut_ptr().add(x), pair.0);
            vst1q_u8(v.as_mut_ptr().add(x), pair.1);
        }
        x += 16;
    }
    let (pairs, _) = src[x * 2..width * 2].as_chunks::<2>();
    for ((pair, u), v) in pairs.iter().zip(&mut u[x..]).zip(&mut v[x..]) {
        [*u, *v] = *pair;
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn unpack_p010_y_row_avx2(src: &[u8], dst: &mut [u8]) {
    use std::arch::x86_64::{
        _mm256_loadu_si256, _mm256_packus_epi16, _mm256_permute4x64_epi64, _mm256_srli_epi16,
        _mm256_storeu_si256,
    };
    let width = dst.len();
    let mut x = 0;
    while x + 32 <= width {
        unsafe {
            let a = _mm256_srli_epi16::<8>(_mm256_loadu_si256(src.as_ptr().add(x * 2).cast()));
            let b = _mm256_srli_epi16::<8>(_mm256_loadu_si256(src.as_ptr().add(x * 2 + 32).cast()));
            let packed = _mm256_permute4x64_epi64::<0xD8>(_mm256_packus_epi16(a, b));
            _mm256_storeu_si256(dst.as_mut_ptr().add(x).cast(), packed);
        }
        x += 32;
    }
    for (i, dst) in dst[x..].iter_mut().enumerate() {
        *dst = src[(x + i) * 2 + 1];
    }
}

#[cfg(target_arch = "aarch64")]
fn unpack_p010_y_row_neon(src: &[u8], dst: &mut [u8]) {
    use std::arch::aarch64::{vld2q_u8, vst1q_u8};
    let width = dst.len();
    let mut x = 0;
    while x + 16 <= width {
        unsafe {
            let pair = vld2q_u8(src.as_ptr().add(x * 2));
            vst1q_u8(dst.as_mut_ptr().add(x), pair.1);
        }
        x += 16;
    }
    for (i, dst) in dst[x..].iter_mut().enumerate() {
        *dst = src[(x + i) * 2 + 1];
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn unpack_p010_uv_row_avx2(src: &[u8], u: &mut [u8], v: &mut [u8]) {
    use std::arch::x86_64::{
        _mm_storeu_si128, _mm256_castsi256_si128, _mm256_extracti128_si256, _mm256_loadu_si256,
        _mm256_packus_epi16, _mm256_permute4x64_epi64, _mm256_setr_epi8, _mm256_shuffle_epi8,
        _mm256_srli_epi16,
    };
    let width = u.len();
    let shuf = _mm256_setr_epi8(
        0, 2, 4, 6, 8, 10, 12, 14, 1, 3, 5, 7, 9, 11, 13, 15, 0, 2, 4, 6, 8, 10, 12, 14, 1, 3, 5,
        7, 9, 11, 13, 15,
    );
    let mut x = 0;
    while x + 16 <= width {
        unsafe {
            let a = _mm256_srli_epi16::<8>(_mm256_loadu_si256(src.as_ptr().add(x * 4).cast()));
            let b = _mm256_srli_epi16::<8>(_mm256_loadu_si256(src.as_ptr().add(x * 4 + 32).cast()));
            let packed = _mm256_permute4x64_epi64::<0xD8>(_mm256_packus_epi16(a, b));
            let sep = _mm256_shuffle_epi8(packed, shuf);
            let ord = _mm256_permute4x64_epi64::<0xD8>(sep);
            _mm_storeu_si128(u.as_mut_ptr().add(x).cast(), _mm256_castsi256_si128(ord));
            _mm_storeu_si128(
                v.as_mut_ptr().add(x).cast(),
                _mm256_extracti128_si256::<1>(ord),
            );
        }
        x += 16;
    }
    for i in x..width {
        u[i] = src[i * 4 + 1];
        v[i] = src[i * 4 + 3];
    }
}

#[cfg(target_arch = "aarch64")]
fn unpack_p010_uv_row_neon(src: &[u8], u: &mut [u8], v: &mut [u8]) {
    use std::arch::aarch64::{vld4q_u8, vst1q_u8};
    let width = u.len();
    let mut x = 0;
    while x + 16 <= width {
        unsafe {
            let quad = vld4q_u8(src.as_ptr().add(x * 4));
            vst1q_u8(u.as_mut_ptr().add(x), quad.1);
            vst1q_u8(v.as_mut_ptr().add(x), quad.3);
        }
        x += 16;
    }
    for i in x..width {
        u[i] = src[i * 4 + 1];
        v[i] = src[i * 4 + 3];
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn unpack_p10_plane_row_avx2(src: &[u8], dst: &mut [u8]) {
    use std::arch::x86_64::{
        _mm256_loadu_si256, _mm256_packus_epi16, _mm256_permute4x64_epi64, _mm256_srli_epi16,
        _mm256_storeu_si256,
    };
    let width = dst.len();
    let mut x = 0;
    while x + 32 <= width {
        unsafe {
            let a = _mm256_srli_epi16::<2>(_mm256_loadu_si256(src.as_ptr().add(x * 2).cast()));
            let b = _mm256_srli_epi16::<2>(_mm256_loadu_si256(src.as_ptr().add(x * 2 + 32).cast()));
            let packed = _mm256_permute4x64_epi64::<0xD8>(_mm256_packus_epi16(a, b));
            _mm256_storeu_si256(dst.as_mut_ptr().add(x).cast(), packed);
        }
        x += 32;
    }
    for (i, dst) in dst[x..].iter_mut().enumerate() {
        let pos = (x + i) * 2;
        *dst = (u16::from_le_bytes([src[pos], src[pos + 1]]) >> 2).min(255) as u8;
    }
}

#[cfg(target_arch = "aarch64")]
fn unpack_p10_plane_row_neon(src: &[u8], dst: &mut [u8]) {
    use std::arch::aarch64::{
        vcombine_u8, vld1q_u8, vqshrn_n_u16, vreinterpretq_u16_u8, vst1q_u8,
    };
    let width = dst.len();
    let mut x = 0;
    while x + 16 <= width {
        unsafe {
            let a = vreinterpretq_u16_u8(vld1q_u8(src.as_ptr().add(x * 2)));
            let b = vreinterpretq_u16_u8(vld1q_u8(src.as_ptr().add(x * 2 + 16)));
            let packed = vcombine_u8(vqshrn_n_u16::<2>(a), vqshrn_n_u16::<2>(b));
            vst1q_u8(dst.as_mut_ptr().add(x), packed);
        }
        x += 16;
    }
    for (i, dst) in dst[x..].iter_mut().enumerate() {
        let pos = (x + i) * 2;
        *dst = (u16::from_le_bytes([src[pos], src[pos + 1]]) >> 2).min(255) as u8;
    }
}

fn unpack(picture: &frame::Video, meta: &Meta, planes: [*mut u8; 3], strides: [isize; 3]) {
    let target = |plane: usize, y: usize, width| unsafe {
        std::slice::from_raw_parts_mut(planes[plane].offset(y as isize * strides[plane]), width)
    };
    let (width, height) = (meta.width as usize, meta.height as usize);
    let (cw, ch) = (width >> meta.shift(), height >> meta.shift());
    #[cfg(target_arch = "x86_64")]
    let avx2 = std::arch::is_x86_feature_detected!("avx2");
    if picture.format() == Pixel::P010LE {
        for y in 0..height {
            let src = &row(picture, 0, y)[..width * 2];
            let dst = target(0, y, width);
            #[cfg(target_arch = "x86_64")]
            if avx2 {
                unsafe { unpack_p010_y_row_avx2(src, dst) };
                continue;
            }
            #[cfg(target_arch = "aarch64")]
            {
                unpack_p010_y_row_neon(src, dst);
                continue;
            }
            #[allow(unreachable_code)]
            for (i, dst) in dst.iter_mut().enumerate() {
                *dst = src[i * 2 + 1];
            }
        }
        for y in 0..ch {
            let src = &row(picture, 1, y)[..cw * 4];
            let (u, v) = (target(1, y, cw), target(2, y, cw));
            #[cfg(target_arch = "x86_64")]
            if avx2 {
                unsafe { unpack_p010_uv_row_avx2(src, u, v) };
                continue;
            }
            #[cfg(target_arch = "aarch64")]
            {
                unpack_p010_uv_row_neon(src, u, v);
                continue;
            }
            #[allow(unreachable_code)]
            for i in 0..cw {
                u[i] = src[i * 4 + 1];
                v[i] = src[i * 4 + 3];
            }
        }
        return;
    }
    if picture.format() == Pixel::YUV420P10LE {
        for (plane, (w, h)) in [(0, (width, height)), (1, (cw, ch)), (2, (cw, ch))] {
            for y in 0..h {
                let src = &row(picture, plane, y)[..w * 2];
                let dst = target(plane, y, w);
                #[cfg(target_arch = "x86_64")]
                if avx2 {
                    unsafe { unpack_p10_plane_row_avx2(src, dst) };
                    continue;
                }
                #[cfg(target_arch = "aarch64")]
                {
                    unpack_p10_plane_row_neon(src, dst);
                    continue;
                }
                #[allow(unreachable_code)]
                for (i, dst) in dst.iter_mut().enumerate() {
                    let pos = i * 2;
                    *dst = (u16::from_le_bytes([src[pos], src[pos + 1]]) >> 2).min(255) as u8;
                }
            }
        }
        return;
    }
    if picture.stride(0) == width && strides[0] == width as isize {
        let len = width * height;
        unsafe { std::slice::from_raw_parts_mut(planes[0], len) }
            .copy_from_slice(&picture.data(0)[..len]);
    } else {
        for y in 0..height {
            target(0, y, width).copy_from_slice(&row(picture, 0, y)[..width]);
        }
    }
    if picture.format() == Pixel::NV12 {
        for y in 0..ch {
            let src = &row(picture, 1, y)[..cw * 2];
            let (u, v) = (target(1, y, cw), target(2, y, cw));
            #[cfg(target_arch = "x86_64")]
            if avx2 {
                unsafe { unpack_nv12_row_avx2(src, u, v) };
                continue;
            }
            #[cfg(target_arch = "aarch64")]
            {
                unpack_nv12_row_neon(src, u, v);
                continue;
            }
            #[allow(unreachable_code)]
            let (pairs, _) = src.as_chunks::<2>();
            #[allow(unreachable_code)]
            for ((pair, u), v) in pairs.iter().zip(u).zip(v) {
                [*u, *v] = *pair;
            }
        }
    } else {
        for plane in 1..3 {
            if picture.stride(plane) == cw && strides[plane] == cw as isize {
                let len = cw * ch;
                unsafe { std::slice::from_raw_parts_mut(planes[plane], len) }
                    .copy_from_slice(&picture.data(plane)[..len]);
            } else {
                for y in 0..ch {
                    target(plane, y, cw).copy_from_slice(&row(picture, plane, y)[..cw]);
                }
            }
        }
    }
}

fn planar(picture: frame::Video, meta: &Meta) -> frame::Video {
    if !matches!(
        picture.format(),
        Pixel::NV12 | Pixel::P010LE | Pixel::YUV420P10LE
    ) {
        return picture;
    }
    let planar = frame::Video::new(meta.pixel(), meta.width, meta.height);
    let raw = unsafe { &*planar.as_ptr() };
    let planes = [0, 1, 2].map(|plane| raw.data[plane]);
    let strides = [0, 1, 2].map(|plane| raw.linesize[plane] as isize);
    unpack(&picture, meta, planes, strides);
    planar
}

fn same(gpu: &frame::Video, cpu: &frame::Video) -> bool {
    let shift = usize::from(!yuv444(cpu.format()));
    (gpu.width(), gpu.height()) == (cpu.width(), cpu.height())
        && yuv444(gpu.format()) == yuv444(cpu.format())
        && (0..3).all(|plane| {
            let shift = if plane == 0 { 0 } else { shift };
            let width = cpu.width() as usize >> shift;
            let height = cpu.height() as usize >> shift;
            (0..height).all(|y| row(gpu, plane, y)[..width] == row(cpu, plane, y)[..width])
        })
}

fn rgb(format: Pixel) -> bool {
    let descriptor = unsafe { ffi::av_pix_fmt_desc_get(format.into()).as_ref() };
    descriptor.is_some_and(|descriptor| descriptor.flags & ffi::AV_PIX_FMT_FLAG_RGB as u64 != 0)
}

impl Decoder {
    pub fn open(path: &Path) -> Result<Self> {
        Self::start(path, |_, _| 1, None)
    }

    pub fn threaded(path: &Path, threads: usize) -> Result<Self> {
        Self::start(path, |_, _| threads, None)
    }

    pub fn chosen(path: &Path, threads: impl FnOnce(f64, u32) -> usize) -> Result<Self> {
        Self::start(path, threads, None)
    }

    // the gpu takes the clip only when its first picture equals the one of the cpu
    pub fn gpu(path: &Path) -> Option<Self> {
        let mut cpu = Self::open(path).ok()?;
        Self::gpu_from(path, &mut cpu)
    }

    pub fn gpu_from(path: &Path, cpu: &mut Self) -> Option<Self> {
        // these codecs decode bit exact by their spec, and the gpu gives 8 bit 4:2:0 and 4:4:4 as they are
        // at an odd size a driver can abort
        let exact = matches!(
            cpu.decoder.id(),
            codec::Id::H264 | codec::Id::HEVC | codec::Id::AV1 | codec::Id::VP9
        );
        let format = cpu.decoder.format();
        if !exact || cpu.meta.cropped || !(native(format, false) || yuv444(format)) {
            return None;
        }
        let available = available_gpus();
        if available.is_empty() {
            return None;
        }
        let reference = cpu.picture(0).ok()?;
        let by = cpu.by;
        ffmpeg_next::log::set_level(Level::Quiet);
        let mut rungs = available
            .iter()
            .copied()
            .flat_map(|kind| [(kind, FLIGHT), (kind, 1)]);
        let found = rungs.find_map(|(kind, threads)| {
            let mut decoder = Self::start(path, |_, _| threads, Some(kind))
                .ok()?
                .shrink(by);
            let picture = decoder.picture(0).ok()?;
            decoder.meta = cpu.meta;
            same(&picture, &reference).then_some(decoder)
        });
        ffmpeg_next::log::set_level(Level::Error);
        found
    }

    fn cpu(&self) -> Result<Self> {
        let decoder = Self::threaded(&self.path, budget::decode(self.full.1))?;
        Ok(decoder.following(&self.cancel).shrink(self.by))
    }

    // a gpu that fails in the clip hands the rest of it to the cpu
    fn fetch(&mut self, n: i32) -> Result<frame::Video> {
        match self.decoded(n) {
            Err(_) if self.gpu && self.cancel.check().is_ok() => {
                *self = self.cpu()?;
                self.decoded(n)
            }
            picture => picture,
        }
    }

    pub fn following(mut self, follow: &Follow) -> Self {
        self.cancel = follow.clone();
        self
    }

    pub fn shrink(mut self, by: u32) -> Self {
        if by > 1 {
            if self.meta.space == color::Space::Unspecified {
                self.meta.space = match self.meta.bt601() {
                    true => color::Space::SMPTE170M,
                    false => color::Space::BT709,
                };
            }
            self.by = by;
            self.meta.width = ((self.full.0 / by) & !3).max(4);
            self.meta.height = ((self.full.1 / by) & !3).max(4);
        }
        self
    }

    pub fn history(&self) -> usize {
        self.keep * self.picture
    }

    pub fn by(&self) -> u32 {
        self.by
    }

    fn start(
        path: &Path,
        threads: impl FnOnce(f64, u32) -> usize,
        gpu: Option<Gpu>,
    ) -> Result<Self> {
        ffmpeg_next::init()?;
        if gpu.is_none() {
            ffmpeg_next::log::set_level(Level::Error);
        }
        let input = format::input(&absolute(path))?;
        let stream = video(&input).ok_or("the clip has no video")?;
        let mut rate = stream.avg_frame_rate();
        if rate.numerator() <= 0 {
            rate = stream.rate();
        }
        if rate.numerator() <= 0 || rate.denominator() <= 0 {
            return Err("the clip has no frame rate".into());
        }
        // a recording that lost frames has a lower mean rate than its real one
        // at the real rate each picture is at its time, so the sound stays with it
        let real = stream.rate();
        let lossy = real.denominator() > 0
            && f64::from(real) > f64::from(rate) * 1.001
            && f64::from(real) < f64::from(rate) * 1.5;
        if lossy {
            rate = real;
        }
        let rate = rate.reduce();
        let fps = f64::from(rate);
        let time_base = f64::from(stream.time_base());
        let seconds = if stream.duration() > 0 {
            stream.duration() as f64 * time_base
        } else {
            input.duration().max(0) as f64 / f64::from(ffi::AV_TIME_BASE)
        };
        let known = Some(stream.start_time()).filter(|start| *start != ffi::AV_NOPTS_VALUE);
        let index = stream.index();
        let format = input.format();
        let still = format.name() == "image2" || format.name().ends_with("_pipe");
        let raw = unsafe { (*(*input.as_ptr()).iformat).flags } & ffi::AVFMT_NOTIMESTAMPS != 0;
        let (frames, start) = match stream.frames() {
            frames if frames > 0 && !lossy => (frames, known.unwrap_or(0)),
            _ if seconds > 0.0 => ((seconds * fps).round() as i64, known.unwrap_or(0)),
            _ if still => (1, known.unwrap_or(0)),
            _ => length(path, index, known, time_base * fps).ok_or("the clip has no length")?,
        };

        let mut context = codec::Context::from_parameters(stream.parameters())?;
        context.set_threading(threading::Config {
            kind: threading::Type::Frame,
            count: threads(fps, unsafe { (*stream.parameters().as_ptr()).height }
                as u32),
        });
        if let Some(kind) = gpu {
            let device = unsafe { &mut (*context.as_mut_ptr()).hw_device_ctx };
            let (name, options) = (std::ptr::null(), std::ptr::null_mut());
            let code = unsafe { ffi::av_hwdevice_ctx_create(device, kind, name, options, 0) };
            if code < 0 {
                return Err(ffmpeg_next::Error::from(code).into());
            }
        }
        let decoder = match gpu.is_some() && context.id() == codec::Id::AV1 {
            // the default av1 decoder has no gpu path
            true => {
                let av1 = codec::decoder::find_by_name("av1").ok_or("no av1 decoder")?;
                context.decoder().open_as(av1)?.video()?
            }
            false => context.decoder().video()?,
        };
        let format = decoder.format();
        let jpeg = matches!(format, Pixel::YUVJ420P | Pixel::YUVJ444P);
        let limited = !jpeg && decoder.color_range() != color::Range::JPEG;
        let meta = Meta {
            width: decoder.width() & !1,
            height: decoder.height() & !1,
            fps: (rate.numerator().into(), rate.denominator().into()),
            frames: i32::try_from(frames).unwrap_or(i32::MAX).max(1),
            // the scaler turns rgb into bt709 yuv
            space: match rgb(format) {
                true => color::Space::BT709,
                false => decoder.color_space(),
            },
            range: if limited || rgb(format) {
                color::Range::MPEG
            } else {
                color::Range::JPEG
            },
            primaries: decoder.color_primaries(),
            transfer: decoder.color_transfer_characteristic(),
            still,
            codec: decoder.id().name(),
            yuv444: whole(format),
            matrix: matrix(&stream),
            cropped: decoder.width() % 2 + decoder.height() % 2 > 0,
        };
        if meta.width == 0 || meta.height == 0 {
            return Err("the clip has no picture size".into());
        }
        // the preview jumps, and the host then reads pictures back that a seek makes slow
        let (wide, high) = (decoder.width() as i32, decoder.height() as i32);
        let picture = unsafe { ffi::av_image_get_buffer_size(format.into(), wide, high, 1) };
        // a format with no size, such as a gpu one, counts as 4:2:0 with 16 bits
        let picture = usize::try_from(picture).ok().filter(|size| *size > 0);
        let picture = picture.unwrap_or(wide.max(1) as usize * high.max(1) as usize * 3);
        let keep = match budget::current() {
            Class::Interactive => (HISTORY / picture).clamp(PAST, 64),
            Class::Render => 2,
            Class::Background => PAST,
        };
        Ok(Self {
            path: path.to_owned(),
            input,
            decoder,
            scaler: None,
            packet: Packet::empty(),
            frame: frame::Video::empty(),
            at: -1,
            past: VecDeque::new(),
            staging: Vec::new(),
            keep,
            picture,
            wanted: i32::MIN,
            ahead: frame::Video::empty(),
            after: None,
            full: (meta.width, meta.height),
            by: 1,
            meta,
            stream: index,
            gpu: gpu.is_some(),
            drained: false,
            pending: false,
            lost: false,
            raw,
            start,
            frames_per_tick: time_base * fps,
            cancel: Follow::default(),
        })
    }

    pub fn meta(&self) -> Meta {
        self.meta
    }

    pub fn first(mut self) -> Result<frame::Video> {
        match self.advance()? {
            Some(_) => Ok(self.ahead),
            None => Err("the file has no picture".into()),
        }
    }

    fn damaged(&self, n: i32) {
        let mut all = DAMAGE.lock().unwrap_or_else(|poison| poison.into_inner());
        let fps = self.meta.fps;
        let damage = all.entry(self.path.clone());
        let damage = damage.or_insert((fps.0 as f64 / fps.1 as f64, BTreeSet::new()));
        damage.1.insert(n);
    }

    // decodes the next picture, and gives the number that its time puts it at
    fn advance(&mut self) -> Result<Option<i32>> {
        loop {
            self.cancel.check()?;
            match self.decoder.receive_frame(&mut self.ahead) {
                Ok(()) => {
                    // a cpu picture means the gpu decode failed
                    if self.gpu && !hardware(&self.ahead) {
                        return Err("the gpu does not decode this clip".into());
                    }
                    let ticks = self.ahead.timestamp().map(|time| time - self.start);
                    let timed =
                        ticks.map(|ticks| (ticks as f64 * self.frames_per_tick).round() as i32);
                    let timed = timed.unwrap_or(self.at + 1);
                    if self.gpu && timed.saturating_add(self.keep as i32) >= self.wanted {
                        download(&mut self.ahead, &mut self.staging)?;
                    }
                    if unsafe { (*self.ahead.as_ptr()).decode_error_flags } != 0 {
                        self.damaged(timed);
                    }
                    return Ok(Some(timed));
                }
                Err(ffmpeg_next::Error::Eof) => return Ok(None),
                Err(error)
                    if self.gpu
                        && !matches!(
                            error,
                            ffmpeg_next::Error::Other {
                                errno: ffmpeg_next::error::EAGAIN
                            }
                        ) =>
                {
                    return Err(error.into());
                }
                Err(_) => {}
            }
            if self.drained {
                return Ok(None);
            }
            let again = std::mem::take(&mut self.pending);
            let read = match again {
                true => Ok(()),
                false => next(&mut self.packet, &mut self.input),
            };
            match read {
                Ok(()) if self.packet.stream() == self.stream => {
                    let sent = self.decoder.send_packet(&self.packet);
                    let full = ffmpeg_next::Error::Other {
                        errno: ffmpeg_next::error::EAGAIN,
                    };
                    // a decoder that stays full after it gave no picture would spin, so the packet is lost
                    if !again && sent.is_err_and(|error| error == full) {
                        self.pending = true;
                    } else if self.gpu {
                        sent?;
                    } else if sent.is_err() {
                        let time = self.packet.pts().or(self.packet.dts());
                        let ticks = time.map_or(0, |time| time - self.start);
                        self.damaged((ticks as f64 * self.frames_per_tick).round() as i32);
                    }
                }
                Ok(()) => {}
                Err(ffmpeg_next::Error::Eof) => {
                    self.decoder.send_eof()?;
                    self.drained = true;
                }
                Err(error) => return Err(error.into()),
            }
        }
    }

    fn seek(&mut self, n: i32) -> Result<()> {
        // a raw stream cannot seek: it decodes on to a later frame, and opens again for an earlier one
        if self.raw && n > self.at {
            return Ok(());
        }
        if self.raw {
            self.input = format::input(&absolute(&self.path))?;
        } else {
            // mid-frame, so rounding cannot miss a keyframe at n
            let target = self.start + ((f64::from(n) + 0.5) / self.frames_per_tick) as i64;
            let code = unsafe {
                ffi::av_seek_frame(
                    self.input.as_mut_ptr(),
                    self.stream as i32,
                    target,
                    ffi::AVSEEK_FLAG_BACKWARD,
                )
            };
            if code < 0 {
                return Err(ffmpeg_next::Error::from(code).into());
            }
        }
        self.decoder.flush();
        self.drained = false;
        self.pending = false;
        self.lost = true;
        self.at = -1;
        let limit = ahead(self.picture) + self.keep + 4;
        for (_, old) in self.past.drain(..) {
            recycle(&mut self.staging, old, self.gpu, limit);
        }
        self.after = None;
        Ok(())
    }

    // only a file with an index gives a seek that ends on a keyframe
    pub fn key(&mut self, n: i32) -> Result<i32> {
        let stream = self.input.stream(self.stream).ok_or("no stream")?;
        if self.raw || unsafe { ffi::avformat_index_get_entries_count(stream.as_ptr()) } < 2 {
            return Err("the file has no index".into());
        }
        self.seek(n)?;
        let key = loop {
            self.cancel.check()?;
            next(&mut self.packet, &mut self.input)?;
            if self.packet.stream() == self.stream && self.packet.is_key() {
                let time = self.packet.pts().or(self.packet.dts()).ok_or("no time")?;
                break ((time - self.start) as f64 * self.frames_per_tick).round() as i32;
            }
        };
        self.seek(key)?;
        Ok(key)
    }

    fn scaler(&mut self) -> Result<Option<&mut Scale>> {
        let (format, width, height) =
            (self.frame.format(), self.frame.width(), self.frame.height());
        if native(format, self.meta.yuv444) && self.by == 1 && (width, height) == self.full {
            return Ok(None);
        }
        // a clip can change its size or format midway, and shrink changes the size of the output
        let fits = |scaler: &Scale| {
            scaler.input == (format, width, height)
                && (scaler.output.1, scaler.output.2) == (self.meta.width, self.meta.height)
        };
        if !self.scaler.as_ref().is_some_and(fits) {
            let file = Meta {
                width,
                height,
                ..self.meta
            };
            let pixel = self.meta.pixel();
            self.scaler = Some(file.scaler(format, pixel, self.meta.width, self.meta.height)?);
        }
        Ok(self.scaler.as_mut())
    }

    fn store(&mut self, planes: [*mut u8; 3], strides: [isize; 3]) -> Result<()> {
        let source = unsafe { &*self.frame.as_ptr() };
        let rows = self.frame.height() as i32;
        if let Some(scaler) = self.scaler()? {
            let planes = [planes[0], planes[1], planes[2], std::ptr::null_mut()];
            let strides = [strides[0] as i32, strides[1] as i32, strides[2] as i32, 0];
            unsafe {
                ffi::sws_scale(
                    scaler.as_mut_ptr(),
                    source.data.as_ptr().cast(),
                    source.linesize.as_ptr(),
                    0,
                    rows,
                    planes.as_ptr(),
                    strides.as_ptr(),
                );
            }
            return Ok(());
        }
        unpack(&self.frame, &self.meta, planes, strides);
        Ok(())
    }
}

fn hardware(picture: &frame::Video) -> bool {
    !unsafe { (*picture.as_ptr()).hw_frames_ctx.is_null() }
}

fn recycle(staging: &mut Vec<frame::Video>, frame: frame::Video, gpu: bool, limit: usize) {
    if gpu && !hardware(&frame) && frame.width() > 0 && staging.len() < limit {
        staging.push(frame);
    }
}

fn clear_side_data(frame: &mut frame::Video) {
    unsafe {
        let raw = frame.as_mut_ptr();
        loop {
            if (*raw).nb_side_data <= 0 {
                break;
            }
            let kind = (**(*raw).side_data).type_;
            ffi::av_frame_remove_side_data(raw, kind);
        }
    }
}

fn download(picture: &mut frame::Video, staging: &mut Vec<frame::Video>) -> Result<()> {
    if !hardware(picture) {
        return Err("the gpu does not decode this clip".into());
    }
    let (w, h) = (picture.width(), picture.height());
    let mut copy = staging
        .iter()
        .position(|f| {
            !hardware(f)
                && f.width() == w
                && f.height() == h
                && unsafe { ffi::av_frame_is_writable(f.as_ptr().cast_mut()) != 0 }
        })
        .map(|pos| staging.swap_remove(pos))
        .unwrap_or_else(frame::Video::empty);
    let reused = copy.width() > 0;
    if reused {
        clear_side_data(&mut copy);
    }
    let mut code = unsafe {
        let (to, from) = (copy.as_mut_ptr(), picture.as_ptr());
        match ffi::av_hwframe_transfer_data(to, from, 0) {
            0 => ffi::av_frame_copy_props(to, from),
            code => code,
        }
    };
    if code < 0 && reused {
        copy = frame::Video::empty();
        code = unsafe {
            let (to, from) = (copy.as_mut_ptr(), picture.as_ptr());
            match ffi::av_hwframe_transfer_data(to, from, 0) {
                0 => ffi::av_frame_copy_props(to, from),
                code => code,
            }
        };
    }
    if code < 0 {
        return Err(ffmpeg_next::Error::from(code).into());
    }
    *picture = copy;
    Ok(())
}

impl Decoder {
    fn find(&mut self, n: i32) -> Result<Option<usize>> {
        // a file can end before its frame count says so, and then the last picture stays
        let mut aim = None;
        self.wanted = n;
        if n < self.at {
            // after a jump the host reads some pictures back, and a seek for each one is slow
            if let Some(index) = self.past.iter().rposition(|past| past.0 <= n) {
                return Ok(Some(index));
            }
            aim = Some(n);
        } else if n > self.at + NEAR && !self.drained || self.lost {
            aim = Some(n);
        }
        if let Some(aim) = aim {
            self.seek(aim)?;
        }
        // the picture for a number is the last one at or before its time: a lost one shows the one before it again
        loop {
            if self.after.is_none() {
                self.after = self.advance()?;
            }
            // in a file without an index a seek can end after the frame, then it goes further back
            let late = self.after.is_none_or(|number| number > n);
            if let Some(from) = aim.filter(|aim| *aim >= 0 && self.at < 0 && late && !self.raw) {
                // the last try is before the start, where a time of 0 can still end after the first picture
                let back = match from {
                    0 => -64,
                    from => (from - (n - from) - 64).max(0),
                };
                if self.seek(back).is_err() {
                    self.seek(0)?;
                }
                aim = Some(back);
                continue;
            }
            match self.after {
                Some(number) if number <= n || self.at < 0 => {
                    std::mem::swap(&mut self.frame, &mut self.ahead);
                    let last = std::mem::replace(&mut self.ahead, frame::Video::empty());
                    let limit = ahead(self.picture) + self.keep + 4;
                    match self.at >= 0 && !hardware(&last) {
                        true => self.past.push_back((self.at, last)),
                        false => {
                            recycle(&mut self.staging, last, self.gpu, limit);
                            for (_, old) in self.past.drain(..) {
                                recycle(&mut self.staging, old, self.gpu, limit);
                            }
                        }
                    }
                    if self.past.len() > self.keep
                        && let Some((_, old)) = self.past.pop_front()
                    {
                        recycle(&mut self.staging, old, self.gpu, limit);
                    }
                    self.at = number;
                    self.after = None;
                    self.lost = false;
                }
                _ => break,
            }
        }
        if self.at < 0 {
            return Err(Error("the clip has no frames".into()));
        }
        if hardware(&self.frame) {
            download(&mut self.frame, &mut self.staging)?;
        }
        Ok(None)
    }

    fn with<T>(&mut self, n: i32, run: impl FnOnce(&mut Self) -> Result<T>) -> Result<T> {
        let past = self.find(n)?;
        if let Some(index) = past {
            std::mem::swap(&mut self.frame, &mut self.past[index].1);
        }
        let result = run(self);
        if let Some(index) = past {
            std::mem::swap(&mut self.frame, &mut self.past[index].1);
        }
        result
    }

    pub(crate) fn luma_sampled(&mut self, n: i32, step: usize, out: &mut [u8]) -> Result<()> {
        let (width, height) = (self.meta.width as usize, self.meta.height as usize);
        self.with(n, |decoder| {
            let format = decoder.frame.format();
            if matches!(
                format,
                Pixel::YUV420P
                    | Pixel::YUVJ420P
                    | Pixel::NV12
                    | Pixel::YUV444P
                    | Pixel::YUVJ444P
                    | Pixel::P010LE
                    | Pixel::YUV420P10LE
            ) && (decoder.frame.width(), decoder.frame.height()) == decoder.full
            {
                #[cfg(target_arch = "x86_64")]
                let avx2 = std::arch::is_x86_feature_detected!("avx2");
                for (dst_row, y) in out.chunks_exact_mut(width).zip((0..height).step_by(step)) {
                    match format {
                        Pixel::P010LE => {
                            let src = &row(&decoder.frame, 0, y)[..width * 2];
                            #[cfg(target_arch = "x86_64")]
                            if avx2 {
                                unsafe { unpack_p010_y_row_avx2(src, dst_row) };
                                continue;
                            }
                            #[cfg(target_arch = "aarch64")]
                            {
                                unpack_p010_y_row_neon(src, dst_row);
                                continue;
                            }
                            #[allow(unreachable_code)]
                            for (i, dst) in dst_row.iter_mut().enumerate() {
                                *dst = src[i * 2 + 1];
                            }
                        }
                        Pixel::YUV420P10LE => {
                            let src = &row(&decoder.frame, 0, y)[..width * 2];
                            #[cfg(target_arch = "x86_64")]
                            if avx2 {
                                unsafe { unpack_p10_plane_row_avx2(src, dst_row) };
                                continue;
                            }
                            #[cfg(target_arch = "aarch64")]
                            {
                                unpack_p10_plane_row_neon(src, dst_row);
                                continue;
                            }
                            #[allow(unreachable_code)]
                            for (i, dst) in dst_row.iter_mut().enumerate() {
                                let pos = i * 2;
                                *dst = (u16::from_le_bytes([src[pos], src[pos + 1]]) >> 2)
                                    .min(255) as u8;
                            }
                        }
                        _ => dst_row.copy_from_slice(&row(&decoder.frame, 0, y)[..width]),
                    }
                }
                return Ok(());
            }
            let picture =
                frame::Video::new(decoder.meta.pixel(), decoder.meta.width, decoder.meta.height);
            let raw = unsafe { &*picture.as_ptr() };
            let strides = [0, 1, 2].map(|plane| raw.linesize[plane] as isize);
            decoder.store([raw.data[0], raw.data[1], raw.data[2]], strides)?;
            for (dst_row, y) in out.chunks_exact_mut(width).zip((0..height).step_by(step)) {
                dst_row.copy_from_slice(&row(&picture, 0, y)[..width]);
            }
            Ok(())
        })
    }

    pub fn picture(&mut self, n: i32) -> Result<frame::Video> {
        Ok(planar(self.decoded(n)?, &self.meta))
    }

    fn decoded(&mut self, n: i32) -> Result<frame::Video> {
        self.with(n, |decoder| {
            if decoder.scaler()?.is_none() {
                return Ok(share(&decoder.frame));
            }
            let Meta { width, height, .. } = decoder.meta;
            let picture = frame::Video::new(decoder.meta.pixel(), width, height);
            let raw = unsafe { &*picture.as_ptr() };
            let strides = [0, 1, 2].map(|plane| raw.linesize[plane] as isize);
            decoder.store([raw.data[0], raw.data[1], raw.data[2]], strides)?;
            Ok(picture)
        })
    }
}

pub(crate) fn ahead(picture: usize) -> usize {
    let limit = budget::bytes(budget::current()) / 16;
    (limit / picture.max(1)).clamp(8, 24)
}

type Link = (mpsc::Sender<i32>, mpsc::Receiver<Result<frame::Video>>);

// decodes the next pictures while the host works on the last ones
pub struct Ahead {
    link: Mutex<Link>,
    thread: Option<JoinHandle<()>>,
    meta: Meta,
}

impl Ahead {
    pub fn new(mut decoder: Decoder) -> Self {
        let meta = decoder.meta();
        let depth = ahead(decoder.picture);
        let (wanted, asked) = mpsc::channel::<i32>();
        let (ready, pictures) = mpsc::channel();
        let thread = work::spawn("decoder", move || {
            let mut made = VecDeque::new();
            let (mut next, mut last, mut order) = (0, -1, false);
            loop {
                let asked = match order && made.len() < depth && next < meta.frames {
                    true => asked.try_recv(),
                    false => asked.recv().map_err(|_| TryRecvError::Disconnected),
                };
                let n = match asked {
                    Ok(n) => n,
                    Err(TryRecvError::Empty) => {
                        match decoder.fetch(next) {
                            Ok(picture) => {
                                made.push_back((next, picture));
                                next += 1;
                            }
                            Err(_) => order = false,
                        }
                        continue;
                    }
                    Err(TryRecvError::Disconnected) => break,
                };
                (order, last) = (n > last && n <= last + NEAR, n);
                while made.front().is_some_and(|(index, _)| *index < n) {
                    made.pop_front();
                }
                let picture = match made.pop_front() {
                    Some((index, picture)) if index == n => Ok(picture),
                    _ => {
                        made.clear();
                        next = n + 1;
                        decoder.fetch(n)
                    }
                };
                if ready.send(picture).is_err() {
                    break;
                }
            }
        });
        Self {
            link: Mutex::new((wanted, pictures)),
            thread: Some(thread),
            meta,
        }
    }

    pub fn picture(&self, n: i32) -> Result<frame::Video> {
        Ok(planar(self.decoded(n)?, &self.meta))
    }

    pub fn raw(&self, n: i32) -> Result<frame::Video> {
        let picture = self.decoded(n)?;
        Ok(match picture.format() {
            Pixel::P010LE | Pixel::YUV420P10LE => planar(picture, &self.meta),
            _ => picture,
        })
    }

    fn decoded(&self, n: i32) -> Result<frame::Video> {
        let link = self
            .link
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let stopped = "the decoder stopped";
        link.0.send(n).map_err(|_| stopped)?;
        link.1.recv().map_err(|_| stopped)?
    }
}

impl Reader for Ahead {
    fn read(&self, n: i32, planes: [*mut u8; 3], strides: [isize; 3]) -> Result<()> {
        unpack(&self.decoded(n)?, &self.meta, planes, strides);
        Ok(())
    }
}

impl Drop for Ahead {
    // the thread must end before the process does, or the gpu context goes away under it
    fn drop(&mut self) {
        let link = self
            .link
            .get_mut()
            .unwrap_or_else(|poison| poison.into_inner());
        link.0 = mpsc::channel().0;
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
