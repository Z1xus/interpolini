use ffmpeg_next::format::Pixel;
use ffmpeg_next::frame;
use rayon::prelude::*;

use crate::Result;
use crate::svp::{Clip, Frame};

const SHIFT: u32 = 15;

pub struct Blend {
    weights: Vec<u32>,
    step: (i64, i64),
    last: i64,
    pub frames: i32,
}

impl Blend {
    pub fn new(weights: &[f64], from: (i64, i64), fps: u32, frames: i32) -> Self {
        let scale = f64::from(1 << SHIFT);
        let mut fixed: Vec<u32> = weights
            .iter()
            .map(|weight| (weight * scale).round() as u32)
            .collect();
        let total: u32 = fixed.iter().sum();
        let middle = fixed.len() / 2;
        fixed[middle] = fixed[middle] + (1 << SHIFT) - total;
        let step = (from.0, from.1 * i64::from(fps));
        Self {
            weights: fixed,
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
            rows.for_each_init(
                || vec![0u32; width],
                |sums, (y, row)| {
                    sums.fill(1 << (SHIFT - 1));
                    for (frame, &weight) in frames.iter().zip(&self.weights) {
                        let source = unsafe {
                            let data = frame.data(plane).add(y * frame.stride(plane));
                            std::slice::from_raw_parts(data, width)
                        };
                        for (sum, &sample) in sums.iter_mut().zip(source) {
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
