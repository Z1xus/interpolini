use std::collections::VecDeque;
use std::path::Path;
use std::sync::OnceLock;
use std::sync::mpsc::{self, TryRecvError};
use std::thread::JoinHandle;

use ffmpeg_next::codec::{self, threading};
use ffmpeg_next::format::{self, Pixel};
use ffmpeg_next::log::Level;
use ffmpeg_next::packet::Mut;
use ffmpeg_next::software::scaling;
use ffmpeg_next::{Packet, color, ffi, frame, media};
use libloading::Library;

use crate::compose::share;
use crate::svp::Reader;
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
}

impl Meta {
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

// the time in a file where its picture starts, in the time base of ffmpeg
// the sound is cut from the same time, so the two stay together
pub fn origin(input: &format::context::Input) -> i64 {
    let video = input.streams().best(media::Type::Video);
    let start = video.and_then(|stream| match stream.start_time() {
        ffi::AV_NOPTS_VALUE => None,
        start => Some((start as f64 * f64::from(stream.time_base()) * 1e6) as i64),
    });
    start.unwrap_or(unsafe { (*input.as_ptr()).start_time }.max(0))
}

const PAST: usize = 16;

pub struct Decoder {
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
    stream: usize,
    gpu: bool,
    drained: bool,
    start: i64,
    frames_per_tick: f64,
}

// the scaler has no thread affinity and the host reads one frame at a time
unsafe impl Send for Decoder {}

pub type Gpu = ffi::AVHWDeviceType;

pub fn gpus() -> Vec<Gpu> {
    let mut gpus = Vec::new();
    if nvidia() {
        gpus.push(Gpu::AV_HWDEVICE_TYPE_CUDA);
    }
    gpus.push(if cfg!(windows) {
        Gpu::AV_HWDEVICE_TYPE_D3D11VA
    } else if cfg!(target_os = "macos") {
        Gpu::AV_HWDEVICE_TYPE_VIDEOTOOLBOX
    } else {
        Gpu::AV_HWDEVICE_TYPE_VAAPI
    });
    gpus
}

// asks the driver itself, because ffmpeg prints an error on a computer without the gpu
fn nvidia() -> bool {
    static FOUND: OnceLock<bool> = OnceLock::new();
    *FOUND.get_or_init(|| unsafe {
        let name = if cfg!(windows) {
            "nvcuda.dll"
        } else {
            "libcuda.so.1"
        };
        let Ok(driver) = Library::new(name) else {
            return false;
        };
        // ffmpeg loads the driver again, so it stays in the process
        let driver = Box::leak(Box::new(driver));
        let start = driver.get::<unsafe extern "C" fn(u32) -> i32>(b"cuInit\0");
        let count = driver.get::<unsafe extern "C" fn(*mut i32) -> i32>(b"cuDeviceGetCount\0");
        let mut gpus = 0;
        matches!((start, count), (Ok(start), Ok(count)) if start(0) == 0 && count(&mut gpus) == 0)
            && gpus > 0
    })
}

fn full(format: Pixel) -> bool {
    matches!(format, Pixel::YUV444P | Pixel::YUVJ444P)
}

fn rgb(format: Pixel) -> bool {
    let descriptor = unsafe { ffi::av_pix_fmt_desc_get(format.into()).as_ref() };
    descriptor.is_some_and(|descriptor| descriptor.flags & ffi::AV_PIX_FMT_FLAG_RGB as u64 != 0)
}

impl Decoder {
    pub fn open(path: &Path) -> Result<Self> {
        Self::start(path, 0, None)
    }

    // 0 lets ffmpeg take one thread for each core, up to 16
    pub fn threaded(path: &Path, threads: usize) -> Result<Self> {
        Self::start(path, threads, None)
    }

    pub fn gpu(path: &Path, gpu: Gpu) -> Result<Self> {
        Self::start(path, 1, Some(gpu))
    }

    fn start(path: &Path, threads: usize, gpu: Option<Gpu>) -> Result<Self> {
        ffmpeg_next::init()?;
        // only once, so that it does not end the quiet time of a trial of the gpu
        static LEVEL: OnceLock<()> = OnceLock::new();
        LEVEL.get_or_init(|| ffmpeg_next::log::set_level(Level::Error));
        let input = format::input(path)?;
        let stream = input
            .streams()
            .best(media::Type::Video)
            .ok_or("the clip has no video")?;
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
            count: threads,
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
        // only these give the same pictures on each decoder, and the gpu gives 8 bits as they are
        let exact = matches!(
            decoder.id(),
            codec::Id::H264 | codec::Id::HEVC | codec::Id::AV1 | codec::Id::VP9
        );
        if gpu.is_some() && !(exact && matches!(format, Pixel::YUV420P | Pixel::YUVJ420P)) {
            return Err("the gpu does not decode this clip".into());
        }
        let limited = format != Pixel::YUVJ420P && decoder.color_range() != color::Range::JPEG;
        let meta = Meta {
            width: decoder.width() & !1,
            height: decoder.height() & !1,
            fps: (rate.numerator().into(), rate.denominator().into()),
            frames: i32::try_from(frames).unwrap_or(i32::MAX).max(1),
            space: decoder.color_space(),
            range: if limited || rgb(format) {
                color::Range::MPEG
            } else {
                color::Range::JPEG
            },
            primaries: decoder.color_primaries(),
            transfer: decoder.color_transfer_characteristic(),
            still,
        };
        if meta.width == 0 || meta.height == 0 {
            return Err("the clip has no picture size".into());
        }
        Ok(Self {
            input,
            decoder,
            scaler: None,
            packet: Packet::empty(),
            frame: frame::Video::empty(),
            at: -1,
            past: VecDeque::new(),
            ahead: frame::Video::empty(),
            after: None,
            meta,
            stream: index,
            gpu: gpu.is_some(),
            drained: false,
            start,
            frames_per_tick: time_base * fps,
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

    // decodes the next picture, and gives the number that its time puts it at
    fn advance(&mut self) -> Result<Option<i32>> {
        loop {
            if self.decoder.receive_frame(&mut self.ahead).is_ok() {
                if self.gpu {
                    // a clip that the gpu cannot decode comes from the cpu, with one thread
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
                }
                let ticks = self.ahead.timestamp().map(|time| time - self.start);
                let timed = ticks.map(|ticks| (ticks as f64 * self.frames_per_tick).round() as i32);
                return Ok(Some(timed.unwrap_or(self.at + 1)));
            }
            if self.drained {
                return Ok(None);
            }
            match next(&mut self.packet, &mut self.input) {
                Ok(()) if self.packet.stream() == self.stream => {
                    // a broken packet must not stop the clip
                    let _ = self.decoder.send_packet(&self.packet);
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
            next(&mut self.packet, &mut self.input)?;
            if self.packet.stream() == self.stream && self.packet.is_key() {
                let time = self.packet.pts().or(self.packet.dts()).ok_or("no time")?;
                return Ok(((time - self.start) as f64 * self.frames_per_tick).round() as i32);
            }
        }
    }

    fn scaler(&mut self) -> Result<Option<&mut scaling::Context>> {
        let format = self.frame.format();
        if matches!(format, Pixel::YUV420P | Pixel::YUVJ420P) || full(format) {
            return Ok(None);
        }
        if self.scaler.is_none() {
            let Meta { width, height, .. } = self.meta;
            self.scaler = Some(self.meta.scaler(format, Pixel::YUV420P, width, height)?);
        }
        Ok(self.scaler.as_mut())
    }

    fn store(&mut self, planes: [*mut u8; 3], strides: [isize; 3]) -> Result<()> {
        let (width, height) = (self.meta.width as usize, self.meta.height as usize);
        let source = unsafe { &*self.frame.as_ptr() };
        if let Some(scaler) = self.scaler()? {
            let planes = [planes[0], planes[1], planes[2], std::ptr::null_mut()];
            let strides = [strides[0] as i32, strides[1] as i32, strides[2] as i32, 0];
            unsafe {
                ffi::sws_scale(
                    scaler.as_mut_ptr(),
                    source.data.as_ptr().cast(),
                    source.linesize.as_ptr(),
                    0,
                    height as i32,
                    planes.as_ptr(),
                    strides.as_ptr(),
                );
            }
            return Ok(());
        }
        let full = full(self.frame.format());
        for plane in 0..3 {
            let (width, height) = if plane == 0 {
                (width, height)
            } else {
                (width / 2, height / 2)
            };
            let line = source.linesize[plane] as isize;
            for y in 0..height as isize {
                let target = unsafe { planes[plane].offset(y * strides[plane]) };
                if plane == 0 || !full {
                    let row = unsafe { source.data[plane].offset(y * line) };
                    unsafe { std::ptr::copy_nonoverlapping(row, target, width) };
                    continue;
                }
                // the scaler is many times slower here
                let row = |y| unsafe {
                    std::slice::from_raw_parts(source.data[plane].offset(y * line), width * 2)
                };
                let (above, below) = (
                    row(y * 2).as_chunks::<2>().0,
                    row(y * 2 + 1).as_chunks::<2>().0,
                );
                let target = unsafe { std::slice::from_raw_parts_mut(target, width) };
                for (sample, (above, below)) in target.iter_mut().zip(above.iter().zip(below)) {
                    let sum = u16::from(above[0])
                        + u16::from(above[1])
                        + u16::from(below[0])
                        + u16::from(below[1]);
                    *sample = ((sum + 2) >> 2) as u8;
                }
            }
        }
        Ok(())
    }
}

impl Decoder {
    fn find(&mut self, n: i32) -> Result<Option<usize>> {
        // the host reads short gaps in order, so a longer jump is a seek
        // a file can end before its frame count says so, and then the last picture stays
        let mut aim = None;
        if n < self.at {
            // after a jump the host reads some pictures back, and a seek for each one is slow
            if let Some(index) = self.past.iter().rposition(|past| past.0 <= n) {
                return Ok(Some(index));
            }
            aim = Some(n);
        } else if n > self.at + 64 && !self.drained {
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

    pub fn picture(&mut self, n: i32) -> Result<frame::Video> {
        self.with(n, |decoder| {
            let format = decoder.frame.format();
            if matches!(format, Pixel::YUV420P | Pixel::YUVJ420P | Pixel::NV12) {
                return Ok(share(&decoder.frame));
            }
            let Meta { width, height, .. } = decoder.meta;
            let picture = frame::Video::new(Pixel::YUV420P, width, height);
            let raw = unsafe { &*picture.as_ptr() };
            let strides = [0, 1, 2].map(|plane| raw.linesize[plane] as isize);
            decoder.store([raw.data[0], raw.data[1], raw.data[2]], strides)?;
            Ok(picture)
        })
    }
}

const AHEAD: usize = 16;

pub struct Ahead {
    wanted: mpsc::Sender<(i32, i32)>,
    pictures: mpsc::Receiver<Result<frame::Video>>,
    thread: Option<JoinHandle<()>>,
}

impl Ahead {
    pub fn new(mut decoder: Decoder) -> Self {
        let (wanted, asked) = mpsc::channel();
        let (ready, pictures) = mpsc::channel();
        let thread = std::thread::spawn(move || {
            let mut made = VecDeque::new();
            let (mut next, mut last) = (0, -1);
            loop {
                let asked = match made.len() >= AHEAD || next > last {
                    true => asked.recv().map_err(|_| TryRecvError::Disconnected),
                    false => asked.try_recv(),
                };
                match asked {
                    Ok((n, end)) => {
                        last = end;
                        if next - made.len() as i32 > n || n >= next {
                            made.clear();
                            made.push_back(decoder.picture(n));
                            next = n + 1;
                        }
                        made.drain(..made.len() - (next - n) as usize);
                        let _ = ready.send(made.pop_front().unwrap_or(Err("no picture".into())));
                    }
                    Err(TryRecvError::Empty) => {
                        made.push_back(decoder.picture(next));
                        next += 1;
                    }
                    Err(TryRecvError::Disconnected) => break,
                }
            }
        });
        Self {
            wanted,
            pictures,
            thread: Some(thread),
        }
    }

    pub fn picture(&self, n: i32, last: i32) -> Result<frame::Video> {
        let stopped = "the decoder stopped";
        self.wanted.send((n, last)).map_err(|_| stopped)?;
        self.pictures.recv().map_err(|_| stopped)?
    }
}

impl Drop for Ahead {
    // the thread must end before the process does, or the gpu context goes away under it
    fn drop(&mut self) {
        self.wanted = mpsc::channel().0;
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Reader for Decoder {
    fn read(&mut self, n: i32, planes: [*mut u8; 3], strides: [isize; 3]) -> Result<()> {
        self.with(n, |decoder| decoder.store(planes, strides))
    }
}
