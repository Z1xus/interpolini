use ffmpeg_next::format::Pixel;
use ffmpeg_next::frame;
use rayon::prelude::*;

use crate::Result;
use crate::config::Blending;
use crate::svp::{Clip, Frame};

const SHIFT: u32 = 15;

pub struct Blend {
    weights: Vec<u32>,
    gamma: f32,
    curve: Option<[f32; 256]>,
    step: (i64, i64),
    last: i64,
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
            frames: (i64::from(frames) * step.1 / step.0).max(1) as i32,
        }
    }

    pub fn frame(&self, clip: &Clip, n: i32, width: u32, height: u32) -> Result<frame::Video> {
        let radius = (self.weights.len() / 2) as i64;
        let center = i64::from(n) * self.step.0 / self.step.1;
        let frames: Vec<Frame> = (center - radius..=center + radius)
            .map(|index| clip.frame(index.clamp(0, self.last) as i32))
            .collect::<Result<_>>()?;

        let mut output = frame::Video::new(Pixel::YUV420P, width, height);
        for plane in 0..3 {
            let shift = usize::from(plane > 0);
            let (width, height) = (width as usize >> shift, height as usize >> shift);
            let stride = output.stride(plane);
            let rows = output
                .data_mut(plane)
                .par_chunks_mut(stride)
                .take(height)
                .enumerate();
            let line = |frame: &Frame, y| unsafe {
                let data = frame.data(plane).add(y * frame.stride(plane));
                std::slice::from_raw_parts(data, width)
            };
            let frames = || frames.iter().zip(&self.weights);
            // luma only, as in open-svpflow
            if let Some(curve) = self.curve.filter(|_| plane == 0) {
                rows.for_each_init(
                    || vec![0f32; width],
                    |sums, (y, row)| {
                        sums.fill(0.0);
                        for (frame, &weight) in frames() {
                            let weight = weight as f32 / (1 << SHIFT) as f32;
                            for (sum, &sample) in sums.iter_mut().zip(line(frame, y)) {
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
            rows.for_each_init(
                || vec![0u32; width],
                |sums, (y, row)| {
                    sums.fill(1 << (SHIFT - 1));
                    for (frame, &weight) in frames() {
                        for (sum, &sample) in sums.iter_mut().zip(line(frame, y)) {
                            *sum += u32::from(sample) * weight;
                        }
                    }
                    for (sample, sum) in row.iter_mut().zip(sums.iter()) {
                        *sample = (sum >> SHIFT) as u8;
                    }
                },
            );
        }
        Ok(output)
    }
}
