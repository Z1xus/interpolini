use std::sync::Mutex;

use ffmpeg_next::format::Pixel;
use ffmpeg_next::frame;
use rayon::prelude::*;

use crate::Result;
use crate::budget;
use crate::compose::share;
use crate::config::Blending;
use crate::decode;

const SHIFT: u32 = 15;
// the low mantissa bits that the inverse curve leaves out
const INEXACT: u32 = 13;

pub struct Blend {
    weights: Vec<u32>,
    exact: Vec<f32>,
    curve: Option<[f32; 256]>,
    // the way back from the curve, found by the top bits of the sum
    inverse: Vec<u8>,
    step: (i64, i64),
    last: i64,
    held: Mutex<Vec<(i64, frame::Video)>>,
    pub frames: i32,
}

impl Blend {
    pub fn new(weights: &[f64], from: (i64, i64), blending: &Blending, frames: i32) -> Self {
        // steps of the rounded running sum add up to one
        let (scale, sum) = (f64::from(1 << SHIFT), weights.iter().sum::<f64>());
        let (mut total, mut before) = (0.0, 0);
        let fixed = weights
            .iter()
            .map(|weight| {
                total += weight;
                let at = (total / sum * scale).round() as u32;
                let weight = at.saturating_sub(before);
                before = before.max(at);
                weight
            })
            .collect();
        let step = (from.0, from.1 * i64::from(blending.fps));
        let gamma = blending.gamma;
        let curve = |value| (value as f32 / 255.0).powf(gamma);
        let back = |index: u32| {
            let middle = f32::from_bits((index << INEXACT) | (1 << (INEXACT - 1)));
            (middle.min(1.0).powf(1.0 / gamma) * 255.0).round() as u8
        };
        let inverse = match gamma != 1.0 {
            true => (0..=1f32.to_bits() >> INEXACT).map(back).collect(),
            false => Vec::new(),
        };
        Self {
            weights: fixed,
            exact: weights.iter().map(|weight| (weight / sum) as f32).collect(),
            curve: (gamma != 1.0).then(|| std::array::from_fn(curve)),
            inverse,
            step,
            last: i64::from(frames - 1),
            held: Mutex::new(Vec::new()),
            frames: i32::try_from(
                (i128::from(frames) * i128::from(step.1) + i128::from(step.0) - 1)
                    / i128::from(step.0),
            )
            .unwrap_or(i32::MAX)
            .max(1),
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
                .enumerate()
                .with_min_len(budget::piece(height));
            let lines: Vec<(&[u8], usize, u32)> = held
                .iter()
                .zip(&self.weights)
                .map(|((_, picture), &weight)| (picture.data(plane), picture.stride(plane), weight))
                .collect();
            // luma only, as in open-svpflow
            if let Some(curve) = self.curve.filter(|_| plane == 0) {
                budget::install(|| {
                    rows.for_each_init(
                        || vec![0f32; width],
                        |sums, (y, row)| {
                            sums.fill(0.0);
                            for (&(data, stride, _), &weight) in lines.iter().zip(&self.exact) {
                                for (sum, &sample) in sums.iter_mut().zip(&data[y * stride..]) {
                                    *sum += weight * curve[usize::from(sample)];
                                }
                            }
                            for (sample, sum) in row.iter_mut().zip(sums.iter()) {
                                let index = sum.clamp(0.0, 1.0).to_bits() >> INEXACT;
                                *sample = self.inverse[index as usize];
                            }
                        },
                    )
                });
                continue;
            }
            budget::install(|| {
                rows.for_each_init(
                    || vec![0u32; width],
                    |sums, (y, row)| {
                        sums.fill(1 << (SHIFT - 1));
                        for &(data, stride, weight) in &lines {
                            for (sum, &sample) in sums.iter_mut().zip(&data[y * stride..]) {
                                *sum += u32::from(sample) * weight;
                            }
                        }
                        for (sample, sum) in row.iter_mut().zip(sums.iter()) {
                            *sample = (sum >> SHIFT).min(255) as u8;
                        }
                    },
                )
            });
        }
        Ok(output)
    }
}
