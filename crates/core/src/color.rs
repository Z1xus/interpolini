use std::path::Path;

use ffmpeg_next::format::Pixel;
use ffmpeg_next::frame;
use rayon::prelude::*;

use crate::decode::Meta;
use crate::lut::Lut;
use crate::{Result, config};

const FIXED: i32 = 1 << 16;

struct Matrix {
    kr: f32,
    kb: f32,
    offset: f32,
    luma: f32,
    chroma: f32,
}

impl Matrix {
    fn rgb(&self, y: f32, cb: f32, cr: f32) -> [f32; 3] {
        let y = (y - self.offset) / self.luma;
        let r = y + 2.0 * (1.0 - self.kr) * cr / self.chroma;
        let b = y + 2.0 * (1.0 - self.kb) * cb / self.chroma;
        let g = (y - self.kr * r - self.kb * b) / (1.0 - self.kr - self.kb);
        [r, g, b]
    }

    fn yuv(&self, [r, g, b]: [f32; 3]) -> [f32; 3] {
        let y = self.kr * r + (1.0 - self.kr - self.kb) * g + self.kb * b;
        [
            y * self.luma + self.offset,
            (b - y) / (2.0 * (1.0 - self.kb)) * self.chroma,
            (r - y) / (2.0 * (1.0 - self.kr)) * self.chroma,
        ]
    }
}

pub struct Grade {
    luma: [u8; 256],
    turn: (i32, i32),
    lut: Option<Lut>,
    matrix: Matrix,
}

fn byte(value: f32) -> u8 {
    value.round().clamp(0.0, 255.0) as u8
}

impl Grade {
    pub fn new(color: &config::Color, meta: &Meta) -> Result<Option<Self>> {
        if !color.enabled {
            return Ok(None);
        }
        let lut = if color.lut.is_empty() {
            None
        } else {
            let path = Path::new(&color.lut);
            let local = config::app_dir().join(path);
            Some(Lut::load(if path.exists() { path } else { &local })?)
        };
        // should prolly ask couleur why 126
        let luma = std::array::from_fn(|value| {
            byte(((value as f32 - 126.0) * color.contrast + 126.0) * color.brightness)
        });
        let (sin, cos) = color.hue.to_radians().sin_cos();
        let scale = color.saturation * FIXED as f32;
        let (kr, kb) = if meta.bt601() {
            (0.299, 0.114)
        } else {
            (0.2126, 0.0722)
        };
        let (offset, luma_scale, chroma) = if meta.full() {
            (0.0, 255.0, 255.0)
        } else {
            (16.0, 219.0, 224.0)
        };
        Ok(Some(Self {
            luma,
            turn: ((cos * scale) as i32, (sin * scale) as i32),
            lut,
            matrix: Matrix {
                kr,
                kb,
                offset,
                luma: luma_scale,
                chroma,
            },
        }))
    }

    pub fn apply(&self, source: &frame::Video) -> frame::Video {
        let (width, height) = (source.width() as usize, source.height() as usize);
        let output = frame::Video::new(Pixel::YUV420P, source.width(), source.height());
        let from: [(&[u8], usize); 3] =
            std::array::from_fn(|plane| (source.data(plane), source.stride(plane)));
        let strides: [usize; 3] = std::array::from_fn(|plane| output.stride(plane));
        let raw = unsafe { &*output.as_ptr() };
        let [y, u, v] = std::array::from_fn(|plane| {
            let rows = if plane == 0 { height } else { height / 2 };
            unsafe { std::slice::from_raw_parts_mut(raw.data[plane], strides[plane] * rows) }
        });
        y.par_chunks_mut(strides[0] * 2)
            .zip(u.par_chunks_mut(strides[1]))
            .zip(v.par_chunks_mut(strides[2]))
            .enumerate()
            .for_each(|(row, ((y, u), v))| {
                let (top, bottom) = y.split_at_mut(strides[0]);
                let luma = &from[0].0[row * 2 * from[0].1..];
                let pairs = [
                    (&luma[..width], &mut top[..width]),
                    (&luma[from[0].1..][..width], &mut bottom[..width]),
                ];
                let u = (
                    &from[1].0[row * from[1].1..][..width / 2],
                    &mut u[..width / 2],
                );
                let v = (
                    &from[2].0[row * from[2].1..][..width / 2],
                    &mut v[..width / 2],
                );
                self.rows(pairs, u, v);
            });
        output
    }

    fn rows(&self, luma: [(&[u8], &mut [u8]); 2], u: (&[u8], &mut [u8]), v: (&[u8], &mut [u8])) {
        let (cos, sin) = self.turn;
        let turn =
            |cb: i32, cr: i32| ((cb * cos - cr * sin) / FIXED, (cb * sin + cr * cos) / FIXED);
        let Some(lut) = &self.lut else {
            for (source, target) in luma {
                for (target, &source) in target.iter_mut().zip(source) {
                    *target = self.luma[usize::from(source)];
                }
            }
            for x in 0..u.0.len() {
                let (cb, cr) = turn(i32::from(u.0[x]) - 128, i32::from(v.0[x]) - 128);
                u.1[x] = (cb + 128).clamp(0, 255) as u8;
                v.1[x] = (cr + 128).clamp(0, 255) as u8;
            }
            return;
        };
        let [(top, top_out), (bottom, bottom_out)] = luma;
        for x in 0..u.0.len() {
            let (cb, cr) = turn(i32::from(u.0[x]) - 128, i32::from(v.0[x]) - 128);
            let mut sums = (0.0, 0.0);
            let mut pixel = |source: u8, target: &mut u8| {
                let y = f32::from(self.luma[usize::from(source)]);
                let [y, cb, cr] = self
                    .matrix
                    .yuv(lut.sample(self.matrix.rgb(y, cb as f32, cr as f32)));
                *target = byte(y);
                sums = (sums.0 + cb, sums.1 + cr);
            };
            pixel(top[x * 2], &mut top_out[x * 2]);
            pixel(top[x * 2 + 1], &mut top_out[x * 2 + 1]);
            pixel(bottom[x * 2], &mut bottom_out[x * 2]);
            pixel(bottom[x * 2 + 1], &mut bottom_out[x * 2 + 1]);
            u.1[x] = byte(sums.0 / 4.0 + 128.0);
            v.1[x] = byte(sums.1 / 4.0 + 128.0);
        }
    }
}
