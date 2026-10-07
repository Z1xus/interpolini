use std::path::Path;

use ffmpeg_next::codec::{self, threading};
use ffmpeg_next::format::{self, Pixel};
use ffmpeg_next::packet::Mut;
use ffmpeg_next::software::scaling;
use ffmpeg_next::{Packet, color, ffi, frame, media};

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

    // marks a picture with the color of this clip, so a scaler can bring it to another clip
    pub fn tag(&self, picture: &mut frame::Video) {
        let raw = unsafe { &mut *picture.as_mut_ptr() };
        raw.colorspace = match self.bt601() {
            true => ffi::AVColorSpace::AVCOL_SPC_SMPTE170M,
            false => ffi::AVColorSpace::AVCOL_SPC_BT709,
        };
        raw.color_range = match self.full() {
            true => ffi::AVColorRange::AVCOL_RANGE_JPEG,
            false => ffi::AVColorRange::AVCOL_RANGE_MPEG,
        };
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
    let input = format::input(path)?;
    let audio = |stream: &format::stream::Stream| {
        let parameters = stream.parameters();
        (parameters.medium() == media::Type::Audio).then(|| {
            let channels = unsafe { (*parameters.as_ptr()).ch_layout.nb_channels };
            format!("{}, {channels} ch", parameters.id().name())
        })
    };
    let tracks: Vec<String> = input
        .streams()
        .filter_map(|stream| audio(&stream))
        .collect();
    let length = input.duration() as f64 / f64::from(ffi::AV_TIME_BASE);
    drop(input);
    let meta = match Decoder::open(path) {
        Ok(decoder) => Some(decoder.meta),
        Err(error) if tracks.is_empty() => return Err(error),
        Err(_) => None,
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

pub struct Decoder {
    input: format::context::Input,
    decoder: codec::decoder::Video,
    scaler: Option<scaling::Context>,
    packet: Packet,
    // the picture that shows now and its number, and the one after it when it is decoded
    frame: frame::Video,
    at: i32,
    ahead: frame::Video,
    after: Option<i32>,
    meta: Meta,
    stream: usize,
    drained: bool,
    start: i64,
    frames_per_tick: f64,
}

// the scaler has no thread affinity and the host reads one frame at a time
unsafe impl Send for Decoder {}

fn rgb(format: Pixel) -> bool {
    let descriptor = unsafe { ffi::av_pix_fmt_desc_get(format.into()).as_ref() };
    descriptor.is_some_and(|descriptor| descriptor.flags & ffi::AV_PIX_FMT_FLAG_RGB as u64 != 0)
}

impl Decoder {
    pub fn open(path: &Path) -> Result<Self> {
        ffmpeg_next::init()?;
        ffmpeg_next::log::set_level(ffmpeg_next::log::Level::Error);
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
        context.set_threading(threading::Config::kind(threading::Type::Frame));
        let decoder = context.decoder().video()?;
        let format = decoder.format();
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
            ahead: frame::Video::empty(),
            after: None,
            meta,
            stream: index,
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
        self.after = None;
        Ok(())
    }

    fn scaler(&mut self) -> Result<Option<&mut scaling::Context>> {
        let format = self.frame.format();
        if matches!(format, Pixel::YUV420P | Pixel::YUVJ420P) {
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
        for plane in 0..3 {
            let (width, height) = if plane == 0 {
                (width, height)
            } else {
                (width / 2, height / 2)
            };
            for y in 0..height as isize {
                unsafe {
                    std::ptr::copy_nonoverlapping(
                        source.data[plane].offset(y * source.linesize[plane] as isize),
                        planes[plane].offset(y * strides[plane]),
                        width,
                    );
                }
            }
        }
        Ok(())
    }
}

impl Reader for Decoder {
    fn read(&mut self, n: i32, planes: [*mut u8; 3], strides: [isize; 3]) -> Result<()> {
        // the host reads short gaps in order, so a longer jump is a seek
        // a file can end before its frame count says so, and then the last picture stays
        if n < self.at || (n > self.at + 64 && !self.drained) {
            self.seek(n)?;
        }
        // the picture for a number is the last one at or before its time: a lost one shows the one before it again
        loop {
            if self.after.is_none() {
                self.after = self.advance()?;
            }
            match self.after {
                Some(number) if number <= n || self.at < 0 => {
                    std::mem::swap(&mut self.frame, &mut self.ahead);
                    self.at = number;
                    self.after = None;
                }
                _ => break,
            }
        }
        if self.at < 0 {
            return Err(Error("the clip has no frames".into()));
        }
        self.store(planes, strides)
    }
}
