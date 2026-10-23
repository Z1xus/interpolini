use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::mpsc::{self, TryRecvError};
use std::thread::JoinHandle;

use ffmpeg_next::codec::{self, threading};
use ffmpeg_next::format::{self, Pixel};
use ffmpeg_next::log::Level;
use ffmpeg_next::packet::Mut;
use ffmpeg_next::software::scaling;
use ffmpeg_next::{Packet, color, ffi, frame, media};

use crate::budget;
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
    ) -> Result<scaling::Context> {
        let flags = scaling::Flags::BILINEAR;
        let mut scaler =
            scaling::Context::get(from, self.width, self.height, to, width, height, flags)?;
        let standard = if self.bt601() {
            ffi::SWS_CS_ITU601
        } else {
            ffi::SWS_CS_ITU709
        };
        let range = |format| i32::from(rgb(format) || self.full());
        unsafe {
            let table = ffi::sws_getCoefficients(standard);
            // 0 and 1 << 16 are the neutral brightness, contrast and saturation
            ffi::sws_setColorspaceDetails(
                scaler.as_mut_ptr(),
                table,
                range(from),
                table,
                range(to),
                0,
                1 << 16,
                1 << 16,
            );
        }
        Ok(scaler)
    }
}

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

// a file with sound only has a frame rate of 0
pub fn probe(path: &Path) -> Result<Probe> {
    ffmpeg_next::init()?;
    let audio = |stream: &format::stream::Stream| {
        let parameters = stream.parameters();
        (parameters.medium() == media::Type::Audio).then(|| {
            let channels = unsafe { (*parameters.as_ptr()).ch_layout.nb_channels };
            format!("{}, {channels} ch", parameters.id().name())
        })
    };
    let read = |input: &format::context::Input| {
        let tracks: Vec<String> = input
            .streams()
            .filter_map(|stream| audio(&stream))
            .collect();
        (
            tracks,
            input.duration() as f64 / f64::from(ffi::AV_TIME_BASE),
        )
    };
    let (meta, (tracks, length)) = match Decoder::open(path) {
        Ok(decoder) => (Some(decoder.meta), read(&decoder.input)),
        Err(error) => match read(&format::input(path)?) {
            (tracks, _) if tracks.is_empty() => return Err(error),
            sound => (None, sound),
        },
    };
    let video = meta.filter(|meta| !meta.still);
    let fps = video.map_or(0.0, |meta| meta.fps.0 as f64 / meta.fps.1 as f64);
    Ok(Probe {
        seconds: match (meta, video) {
            (_, Some(meta)) => f64::from(meta.frames) / fps,
            (Some(_), None) => 0.0,
            (None, _) => length,
        },
        fps,
        tracks,
        width: meta.map_or(0, |meta| meta.width),
        height: meta.map_or(0, |meta| meta.height),
        still: meta.is_some_and(|meta| meta.still),
        yuv444: meta.is_some_and(|meta| meta.yuv444),
    })
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
pub(crate) const REFERENCES: usize = 16;
// a shorter step forward is decoded through, a longer one seeks
const NEAR: i32 = 64;

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
    scaler: Option<scaling::Context>,
    packet: Packet,
    // the picture that shows now and its number, and the one after it when it is decoded
    frame: frame::Video,
    at: i32,
    past: VecDeque<(i32, frame::Video)>,
    ahead: frame::Video,
    after: Option<i32>,
    meta: Meta,
    full: (u32, u32),
    by: u32,
    stream: usize,
    gpu: bool,
    drained: bool,
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
        false => matches!(format, Pixel::YUV420P | Pixel::YUVJ420P),
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

fn row(picture: &frame::Video, plane: usize, y: usize) -> &[u8] {
    &picture.data(plane)[y * picture.stride(plane)..]
}

fn same(gpu: &frame::Video, cpu: &frame::Video) -> bool {
    let (width, height) = (cpu.width() as usize, cpu.height() as usize);
    let plane = |p: usize| (0..height).all(|y| row(gpu, p, y)[..width] == row(cpu, p, y)[..width]);
    match (gpu.format(), yuv444(cpu.format())) {
        (Pixel::NV12, false) => {
            plane(0)
                && (0..height / 2).all(|y| {
                    let both = &row(gpu, 1, y)[..width];
                    let (u, v) = (&row(cpu, 1, y)[..width / 2], &row(cpu, 2, y)[..width / 2]);
                    both.iter().step_by(2).eq(u) && both.iter().skip(1).step_by(2).eq(v)
                })
        }
        (Pixel::YUV444P, true) => (0..3).all(plane),
        _ => false,
    }
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
        // these codecs decode bit exact by their spec, and the gpu gives 8 bit 4:2:0 and 4:4:4 as they are
        let exact = matches!(
            cpu.decoder.id(),
            codec::Id::H264 | codec::Id::HEVC | codec::Id::AV1 | codec::Id::VP9
        );
        let format = cpu.decoder.format();
        if !exact || !(native(format, false) || yuv444(format)) {
            return None;
        }
        let reference = cpu.picture(0).ok()?;
        ffmpeg_next::log::set_level(Level::Quiet);
        let found = gpus().into_iter().find_map(|kind| {
            let mut decoder = Self::start(path, |_, _| 1, Some(kind)).ok()?;
            let picture = decoder.picture(0).ok()?;
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
        match self.picture(n) {
            Err(_) if self.gpu && self.cancel.check().is_ok() => {
                *self = self.cpu()?;
                self.picture(n)
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
        let input = format::input(path)?;
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
        let fps = f64::from(rate);
        let time_base = f64::from(stream.time_base());
        let seconds = if stream.duration() > 0 {
            stream.duration() as f64 * time_base
        } else {
            input.duration() as f64 / f64::from(ffi::AV_TIME_BASE)
        };
        let frames = match stream.frames() {
            frames if frames > 0 && !lossy => frames,
            _ => (seconds * fps).round() as i64,
        };
        let start = match stream.start_time() {
            ffi::AV_NOPTS_VALUE => 0,
            start => start,
        };
        let index = stream.index();
        let format = input.format();
        let still = format.name() == "image2" || format.name().ends_with("_pipe");

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
        let decoder = context.decoder().video()?;
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
        Ok(Self {
            path: path.to_owned(),
            input,
            decoder,
            scaler: None,
            packet: Packet::empty(),
            frame: frame::Video::empty(),
            at: -1,
            past: VecDeque::new(),
            ahead: frame::Video::empty(),
            after: None,
            full: (meta.width, meta.height),
            by: 1,
            meta,
            stream: index,
            gpu: gpu.is_some(),
            drained: false,
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
            if self.decoder.receive_frame(&mut self.ahead).is_ok() {
                if self.gpu {
                    self.download()?;
                }
                let ticks = self.ahead.timestamp().map(|time| time - self.start);
                let timed = ticks.map(|ticks| (ticks as f64 * self.frames_per_tick).round() as i32);
                let timed = timed.unwrap_or(self.at + 1);
                if unsafe { (*self.ahead.as_ptr()).decode_error_flags } != 0 {
                    self.damaged(timed);
                }
                return Ok(Some(timed));
            }
            if self.drained {
                return Ok(None);
            }
            match next(&mut self.packet, &mut self.input) {
                Ok(()) if self.packet.stream() == self.stream => {
                    // a broken packet must not stop the clip
                    if self.decoder.send_packet(&self.packet).is_err() {
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

    fn download(&mut self) -> Result<()> {
        if unsafe { (*self.ahead.as_ptr()).hw_frames_ctx.is_null() } {
            return Err("the gpu does not decode this clip".into());
        }
        let mut copy = frame::Video::empty();
        let code = unsafe {
            let (to, from) = (copy.as_mut_ptr(), self.ahead.as_ptr());
            match ffi::av_hwframe_transfer_data(to, from, 0) {
                0 => ffi::av_frame_copy_props(to, from),
                code => code,
            }
        };
        if code < 0 {
            return Err(ffmpeg_next::Error::from(code).into());
        }
        self.ahead = copy;
        Ok(())
    }

    fn seek(&mut self, n: i32) -> Result<()> {
        let target = self.start + (f64::from(n) / self.frames_per_tick) as i64;
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
        self.decoder.flush();
        self.drained = false;
        self.at = -1;
        self.past.clear();
        self.after = None;
        Ok(())
    }

    // only a file with an index gives a seek that ends on a keyframe
    pub fn key(&mut self, n: i32) -> Result<i32> {
        let stream = self.input.stream(self.stream).ok_or("no stream")?;
        if unsafe { ffi::avformat_index_get_entries_count(stream.as_ptr()) } < 2 {
            return Err("the file has no index".into());
        }
        self.seek(n)?;
        loop {
            self.cancel.check()?;
            next(&mut self.packet, &mut self.input)?;
            if self.packet.stream() == self.stream && self.packet.is_key() {
                let time = self.packet.pts().or(self.packet.dts()).ok_or("no time")?;
                return Ok(((time - self.start) as f64 * self.frames_per_tick).round() as i32);
            }
        }
    }

    fn scaler(&mut self) -> Result<Option<&mut scaling::Context>> {
        let format = self.frame.format();
        if native(format, self.meta.yuv444) && self.by == 1 {
            return Ok(None);
        }
        if self.scaler.is_none() {
            let Meta { width, height, .. } = self.meta;
            let (full_width, full_height) = self.full;
            let file = Meta {
                width: full_width,
                height: full_height,
                ..self.meta
            };
            self.scaler = Some(file.scaler(format, self.meta.pixel(), width, height)?);
        }
        Ok(self.scaler.as_mut())
    }

    fn store(&mut self, planes: [*mut u8; 3], strides: [isize; 3]) -> Result<()> {
        let (width, height) = (self.meta.width as usize, self.meta.height as usize);
        let source = unsafe { &*self.frame.as_ptr() };
        let rows = self.full.1 as i32;
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
        let shift = self.meta.shift();
        for plane in 0..3 {
            let (width, height) = match plane {
                0 => (width, height),
                _ => (width >> shift, height >> shift),
            };
            let line = source.linesize[plane] as isize;
            for y in 0..height as isize {
                let row = unsafe { source.data[plane].offset(y * line) };
                let target = unsafe { planes[plane].offset(y * strides[plane]) };
                unsafe { std::ptr::copy_nonoverlapping(row, target, width) };
            }
        }
        Ok(())
    }
}

impl Decoder {
    fn find(&mut self, n: i32) -> Result<Option<usize>> {
        // a file can end before its frame count says so, and then the last picture stays
        let mut aim = None;
        if n < self.at {
            // after a jump the host reads some pictures back, and a seek for each one is slow
            if let Some(index) = self.past.iter().rposition(|past| past.0 <= n) {
                return Ok(Some(index));
            }
            aim = Some(n);
        } else if n > self.at + NEAR && !self.drained {
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
            if let Some(from) = aim.filter(|aim| *aim >= 0 && self.at < 0 && late) {
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
                    if self.at >= 0 {
                        self.past.push_back((self.at, last));
                    }
                    if self.past.len() > PAST {
                        self.past.pop_front();
                    }
                    self.at = number;
                    self.after = None;
                }
                _ => break,
            }
        }
        if self.at < 0 {
            return Err(Error("the clip has no frames".into()));
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

    pub fn read(&mut self, n: i32, planes: [*mut u8; 3], strides: [isize; 3]) -> Result<()> {
        self.with(n, |decoder| decoder.store(planes, strides))
    }

    pub fn picture(&mut self, n: i32) -> Result<frame::Video> {
        self.with(n, |decoder| {
            let format = decoder.frame.format();
            let whole = decoder.by == 1;
            if whole && (native(format, decoder.meta.yuv444) || format == Pixel::NV12) {
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

pub(crate) const AHEAD: usize = 8;

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
        let (wanted, asked) = mpsc::channel::<i32>();
        let (ready, pictures) = mpsc::channel();
        let thread = work::spawn("decoder", move || {
            let mut made = VecDeque::new();
            let (mut next, mut last, mut order) = (0, -1, false);
            loop {
                let asked = match order && made.len() < AHEAD && next < meta.frames {
                    true => asked.try_recv(),
                    false => asked.recv().map_err(|_| TryRecvError::Disconnected),
                };
                let n = match asked {
                    Ok(n) => n,
                    Err(TryRecvError::Empty) => {
                        made.push_back((next, decoder.fetch(next)));
                        next += 1;
                        continue;
                    }
                    Err(TryRecvError::Disconnected) => break,
                };
                (order, last) = (n > last && n <= last + NEAR, n);
                while made.front().is_some_and(|(index, _)| *index < n) {
                    made.pop_front();
                }
                let picture = match made.pop_front() {
                    Some((index, picture)) if index == n => picture,
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
        let picture = self.picture(n)?;
        let (width, height) = (self.meta.width as usize, self.meta.height as usize);
        let target = |plane: usize, y: usize, length: usize| unsafe {
            std::slice::from_raw_parts_mut(
                planes[plane].offset(y as isize * strides[plane]),
                length,
            )
        };
        for y in 0..height {
            target(0, y, width).copy_from_slice(&row(&picture, 0, y)[..width]);
        }
        if picture.format() == Pixel::NV12 {
            for y in 0..height / 2 {
                let (u, v) = (target(1, y, width / 2), target(2, y, width / 2));
                let both = row(&picture, 1, y)[..width].as_chunks::<2>().0;
                for ((u, v), &[first, second]) in u.iter_mut().zip(v).zip(both) {
                    (*u, *v) = (first, second);
                }
            }
            return Ok(());
        }
        let shift = self.meta.shift();
        let (width, height) = (width >> shift, height >> shift);
        for plane in 1..3 {
            for y in 0..height {
                target(plane, y, width).copy_from_slice(&row(&picture, plane, y)[..width]);
            }
        }
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
