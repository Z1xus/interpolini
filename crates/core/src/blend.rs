use std::sync::Mutex;

use ffmpeg_next::format::Pixel;
use ffmpeg_next::frame;
use rayon::prelude::*;

use crate::Result;
use crate::compose::share;
use crate::config::Blending;
use crate::decode;

const SHIFT: u32 = 15;

pub struct Blend {
    weights: Vec<u32>,
    gamma: f32,
    curve: Option<[f32; 256]>,
    step: (i64, i64),
    last: i64,
    held: Mutex<Vec<(i64, frame::Video)>>,
    pub frames: i32,
}

impl Blend {
    pub fn new(weights: &[f64], from: (i64, i64), blending: &Blending, frames: i32) -> Self {
        let scale = f64::from(1 << SHIFT);
        let mut fixed: Vec<u32> = weights
            .iter()
            .map(|weight| (weight * scale).round() as u32)
            .collect();
        let total: u32 = fixed.iter().sum();
        let middle = fixed.len() / 2;
        fixed[middle] = fixed[middle] + (1 << SHIFT) - total;
        let step = (from.0, from.1 * i64::from(blending.fps));
        let gamma = blending.gamma;
        let curve = |value| (value as f32 / 255.0).powf(gamma);
        Self {
            weights: fixed,
            gamma,
            curve: (gamma != 1.0).then(|| std::array::from_fn(curve)),
            step,
            last: i64::from(frames - 1),
            held: Mutex::new(Vec::new()),
            frames: (i64::from(frames) * step.1 / step.0).max(1) as i32,
        }
    }

    fn radius(&self) -> i64 {
        (self.weights.len() / 2) as i64
    }

    pub fn first(&self, n: i32) -> i32 {
        let center = i64::from(n) * self.step.0 / self.step.1;
        (center - self.radius()).clamp(0, self.last) as i32
    }

    pub fn window(&self) -> usize {
        self.weights.len()
    }

    pub fn pace(&self) -> f32 {
        self.step.0 as f32 / self.step.1 as f32
    }

    pub fn last(&self, n: i32) -> i32 {
        let center = i64::from(n) * self.step.0 / self.step.1;
        (center + self.radius()).clamp(0, self.last) as i32
    }

    pub fn after(&self, picture: i32) -> i32 {
        let reach = (i64::from(picture) + self.radius()) * self.step.1;
        ((reach + self.step.0 - 1) / self.step.0) as i32
    }

    pub fn frame(
        &self,
        mut source: impl FnMut(i32) -> Result<frame::Video>,
        n: i32,
        width: u32,
        height: u32,
    ) -> Result<frame::Video> {
        let radius = self.radius();
        let center = i64::from(n) * self.step.0 / self.step.1;
        let mut held = self
            .held
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        *held = (center - radius..=center + radius)
            .map(|index| {
                let index = index.clamp(0, self.last);
                let picture = match held.iter().find(|held| held.0 == index) {
                    Some(held) => share(&held.1),
                    None => source(index as i32)?,
                };
                Ok((index, picture))
            })
            .collect::<Result<_>>()?;

        let yuv444 = held
            .first()
            .is_some_and(|(_, picture)| decode::yuv444(picture.format()));
        let format = if yuv444 {
            Pixel::YUV444P
        } else {
            Pixel::YUV420P
        };
        let mut output = frame::Video::new(format, width, height);
        for plane in 0..3 {
            let shift = usize::from(plane > 0 && !yuv444);
            let (width, height) = (width as usize >> shift, height as usize >> shift);
            let stride = output.stride(plane);
            let rows = output
                .data_mut(plane)
                .par_chunks_mut(stride)
                .take(height)
                .enumerate();
            // a picture from the gpu has both color planes in one, a sample of each in turn
            let lines: Vec<(&[u8], usize, u32, usize)> = held
                .iter()
                .zip(&self.weights)
                .map(|((_, picture), &weight)| {
                    let (from, side) = match picture.format() {
                        Pixel::NV12 if plane > 0 => (1, plane),
                        _ => (plane, 0),
                    };
                    (picture.data(from), picture.stride(from), weight, side)
                })
                .collect();
            // luma only, as in open-svpflow
            if let Some(curve) = self.curve.filter(|_| plane == 0) {
                rows.for_each_init(
                    || vec![0f32; width],
                    |sums, (y, row)| {
                        sums.fill(0.0);
                        for &(data, stride, weight, _) in &lines {
                            let weight = weight as f32 / (1 << SHIFT) as f32;
                            for (sum, &sample) in sums.iter_mut().zip(&data[y * stride..]) {
                                *sum += weight * curve[usize::from(sample)];
                            }
                        }
                        for (sample, sum) in row.iter_mut().zip(sums.iter()) {
                            *sample = (sum.powf(1.0 / self.gamma) * 255.0).round() as u8;
                        }
                    },
                );
                continue;
            }
            // 16 bits hold a sum with 7 bits after the point, and each product loses half a step
            let start = 64 + lines.len() as u16 / 2;
            rows.for_each_init(
                || (vec![0u16; width], vec![0u16; width]),
                |(sums, wide), (y, row)| {
                    sums.fill(start);
                    for &(data, stride, weight, side) in &lines {
                        // a separate pass, so that the compiler multiplies in 16 bits
                        let line = &data[y * stride..];
                        match side {
                            0 => {
                                for (wide, &sample) in wide.iter_mut().zip(line) {
                                    *wide = u16::from(sample) << 8;
                                }
                            }
                            1 => {
                                for (wide, pair) in wide.iter_mut().zip(line.as_chunks::<2>().0) {
                                    *wide = u16::from_le_bytes(*pair) << 8;
                                }
                            }
                            _ => {
                                for (wide, pair) in wide.iter_mut().zip(line.as_chunks::<2>().0) {
                                    *wide = u16::from_le_bytes(*pair) & 0xff00;
                                }
                            }
                        }
                        let weight = weight as u16;
                        for (sum, &wide) in sums.iter_mut().zip(wide.iter()) {
                            *sum += ((u32::from(wide) * u32::from(weight)) >> 16) as u16;
                        }
                    }
                    for (sample, sum) in row.iter_mut().zip(sums.iter()) {
                        *sample = (sum >> 7) as u8;
                    }
                },
            );
        }
        Ok(output)
    }
}
