use ffmpeg_next::format::Pixel;
use ffmpeg_next::{Packet, Rational, codec, color, decoder, encoder, ffi, frame};

use serde::{Deserialize, Serialize};

use crate::Result;
use crate::compose::Scaler;
use crate::decode::Meta;

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Munch {
    // 1 to 10
    pub amount: u8,
    pub fps: Option<u32>,
    // 0 to 10
    pub fry: u8,
    // 0 to 10
    pub loud: u8,
}

impl Munch {
    pub fn size(&self, (width, height): (u32, u32)) -> (u32, u32) {
        let by = 1.0 + 0.6 * f64::from(self.amount - 1);
        let side = |length: u32| ((f64::from(length) / by).round() as u32 & !1).max(16);
        (side(width), side(height))
    }
}

pub struct Muncher {
    pub meta: Meta,
    fry: f32,
    quality: i32,
    down: Scaler,
    up: Scaler,
    encoder: encoder::video::Encoder,
    decoder: decoder::Video,
    packet: Packet,
    space: ffi::AVColorSpace,
}

impl Muncher {
    pub fn new(meta: &Meta, munch: &Munch) -> Result<Self> {
        let (width, height) = munch.size((meta.width, meta.height));
        let meta = Meta {
            width,
            height,
            space: match meta.bt601() {
                true => color::Space::SMPTE170M,
                false => color::Space::BT709,
            },
            range: color::Range::MPEG,
            yuv444: false,
            ..*meta
        };
        let jpeg = encoder::find(codec::Id::MJPEG).ok_or("there is no mjpeg encoder")?;
        let mut video = codec::Context::new_with_codec(jpeg).encoder().video()?;
        video.set_width(width);
        video.set_height(height);
        video.set_format(Pixel::YUVJ420P);
        video.set_time_base(Rational(1, 25));
        video.set_flags(codec::Flags::QSCALE);
        video.set_threading(codec::threading::Config {
            kind: codec::threading::Type::Slice,
            count: 0,
        });
        let encoder = video.open_as(jpeg)?;
        let reader = decoder::find(codec::Id::MJPEG).ok_or("there is no mjpeg decoder")?;
        let decoder = codec::Context::new_with_codec(reader)
            .decoder()
            .open_as(reader)?
            .video()?;
        Ok(Self {
            meta,
            fry: f32::from(munch.fry) / 10.0,
            quality: (2 + i32::from(munch.amount - 1) * 29 / 9) * ffi::FF_QP2LAMBDA,
            down: Scaler::new(),
            up: Scaler::new(),
            encoder,
            decoder,
            packet: Packet::empty(),
            space: meta.tags().0,
        })
    }

    pub fn run(&mut self, picture: &frame::Video) -> Result<frame::Video> {
        let (width, height) = (self.meta.width, self.meta.height);
        let mut small = frame::Video::new(Pixel::YUVJ420P, width, height);
        self.tag(&mut small);
        self.down.run(&mut small, picture)?;
        if self.fry > 0.0 {
            fry(&mut small, self.fry);
        }
        // the encoder takes the qscale of each frame
        unsafe { (*small.as_mut_ptr()).quality = self.quality };
        self.encoder.send_frame(&small)?;
        self.encoder.receive_packet(&mut self.packet)?;
        self.decoder.send_packet(&self.packet)?;
        let mut munched = frame::Video::empty();
        self.decoder.receive_frame(&mut munched)?;
        self.tag(&mut munched);
        let mut output = frame::Video::new(Pixel::YUV420P, width, height);
        self.meta.tag(&mut output);
        self.up.run(&mut output, &munched)?;
        Ok(output)
    }

    // the full range that a jpeg has
    fn tag(&self, picture: &mut frame::Video) {
        let raw = unsafe { &mut *picture.as_mut_ptr() };
        (raw.colorspace, raw.color_range) = (self.space, ffi::AVColorRange::AVCOL_RANGE_JPEG);
    }
}

fn fry(picture: &mut frame::Video, k: f32) {
    let (width, height) = (picture.width() as usize, picture.height() as usize);
    let stride = picture.stride(0);
    let luma = picture.data_mut(0);
    let mut flat = Vec::with_capacity(width * height);
    for row in luma.chunks_mut(stride).take(height) {
        for y in &mut row[..width] {
            *y = ((f32::from(*y) - 128.0) * (1.0 + k) + 128.0)
                .round()
                .clamp(0.0, 255.0) as u8;
        }
        flat.extend_from_slice(&row[..width]);
    }
    let at = |x: usize, y: usize| f32::from(flat[y * width + x]);
    for (y, row) in luma.chunks_mut(stride).take(height).enumerate() {
        for (x, to) in row[..width].iter_mut().enumerate() {
            let near = at(x.saturating_sub(1), y)
                + at((x + 1).min(width - 1), y)
                + at(x, y.saturating_sub(1))
                + at(x, (y + 1).min(height - 1));
            let sharp = at(x, y) + 1.5 * k * (at(x, y) - near / 4.0);
            *to = sharp.round().clamp(0.0, 255.0) as u8;
        }
    }
    for (plane, shift) in [(1, -10.0 * k), (2, 14.0 * k)] {
        let (stride, wide, high) = (picture.stride(plane), width / 2, height / 2);
        for row in picture.data_mut(plane).chunks_mut(stride).take(high) {
            for pixel in &mut row[..wide] {
                let value = (f32::from(*pixel) - 128.0) * (1.0 + 2.0 * k) + 128.0 + shift;
                *pixel = value.round().clamp(0.0, 255.0) as u8;
            }
        }
    }
}

// position counts samples on the timeline, held is the last sample of the call before at the position just before
pub fn crush(samples: &mut [Vec<f32>; 2], position: i64, munch: &Munch, held: &mut [f32; 2]) {
    let step = (1.0 + f64::from(munch.amount - 1) * 0.7).round() as i64;
    let levels = f32::from(1u16 << (16 - munch.amount));
    let gain = 10f32.powf(f32::from(munch.loud) * 1.8 / 20.0);
    for (channel, held) in samples.iter_mut().zip(held) {
        if step > 1 {
            for index in 0..channel.len() {
                let behind = (position + index as i64).rem_euclid(step) as usize;
                channel[index] = index
                    .checked_sub(behind)
                    .map_or(*held, |from| channel[from]);
            }
            *held = channel.last().copied().unwrap_or(*held);
        }
        for sample in channel {
            if munch.amount > 1 {
                *sample = (*sample * levels).round() / levels;
            }
            if munch.loud > 0 {
                *sample = (*sample * gain).clamp(-1.0, 1.0);
            }
        }
    }
}
