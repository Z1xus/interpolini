use std::path::Path;
use std::sync::Arc;

use crate::Result;
use crate::decode::{Decoder, Meta};
use crate::svp::{Clip, Frame, Reader};

// every fourth sample of every fourth row is enough to find a repeated frame
const STEP: usize = 4;

pub struct Pick {
    pub source: Arc<Clip>,
    pub step: i32,
    pub offset: i32,
    pub width: usize,
    pub height: usize,
}

impl Reader for Pick {
    fn read(&mut self, n: i32, planes: [*mut u8; 3], strides: [isize; 3]) -> Result<()> {
        let frame = self.source.frame(n * self.step + self.offset)?;
        frame.copy(planes, strides, self.width, self.height);
        Ok(())
    }
}

pub struct Dedup {
    pub source: Arc<Clip>,
    pub fills: [Clip; 2],
    pub threshold: f32,
    pub last: i32,
    pub width: usize,
    pub height: usize,
}

impl Dedup {
    fn difference(&self, first: &Frame, second: &Frame) -> f32 {
        let mut total = 0u64;
        for y in (0..self.height).step_by(STEP) {
            let row = |frame: &Frame| unsafe {
                std::slice::from_raw_parts(frame.data(0).add(y * frame.stride(0)), self.width)
            };
            total += distance(row(first), row(second));
        }
        total as f32 / samples(self.width, self.height)
    }
}

fn distance(first: &[u8], second: &[u8]) -> u64 {
    let pairs = first.iter().zip(second).step_by(STEP);
    pairs.map(|(a, b)| u64::from(a.abs_diff(*b))).sum()
}

fn samples(width: usize, height: usize) -> f32 {
    (width.div_ceil(STEP) * height.div_ceil(STEP)) as f32
}

// the part of the frames between two times of a clip that repeat the frame before them
// a long row of equal frames is a still picture and not a stutter, so it does not count
pub fn repeats(clip: &Path, threshold: f32, from: f64, to: f64) -> Result<f32> {
    const SPOTS: i32 = 3;
    const SAMPLE: i32 = 100;
    const STILL: i32 = 8;
    let mut decoder = Decoder::open(clip)?;
    let Meta {
        width,
        height,
        frames,
        fps,
        ..
    } = decoder.meta();
    let (width, height) = (width as usize, height as usize);
    let luma = width * height;
    let mut pictures = [vec![0u8; luma * 3 / 2], vec![0u8; luma * 3 / 2]];
    let rate = fps.0 as f64 / fps.1 as f64;
    let (start, end) = ((from * rate) as i32, ((to * rate) as i32).min(frames));
    let (mut checked, mut same) = (0, 0);
    for spot in 1..=SPOTS {
        let first = (start + (end - start) * spot / (SPOTS + 1) - SAMPLE / 2).max(start);
        let mut row = 0;
        for step in 0..SAMPLE.min(end - first) {
            let picture = pictures[step as usize % 2].as_mut_ptr();
            let planes = unsafe { [picture, picture.add(luma), picture.add(luma * 5 / 4)] };
            let strides = [width as isize, width as isize / 2, width as isize / 2];
            decoder.read(first + step, planes, strides)?;
            let lines = (0..height)
                .step_by(STEP)
                .map(|y| y * width..(y + 1) * width);
            let total: u64 = lines
                .map(|line| distance(&pictures[0][line.clone()], &pictures[1][line]))
                .sum();
            if step == 0 {
                continue;
            }
            checked += 1;
            // a row counts when it ends, and only when it was short
            match total as f32 / samples(width, height) < threshold {
                true => row += 1,
                false => {
                    same += if row <= STILL { row } else { 0 };
                    row = 0;
                }
            }
        }
    }
    Ok(same as f32 / checked.max(1) as f32)
}

impl Reader for Dedup {
    fn read(&mut self, n: i32, planes: [*mut u8; 3], strides: [isize; 3]) -> Result<()> {
        let mut frame = self.source.frame(n)?;
        if n > 0
            && n < self.last
            && self.difference(&frame, &self.source.frame(n - 1)?) < self.threshold
        {
            // the middle of the frames around the repeat, from the clip that skips it
            let before = n - 1;
            frame = self.fills[before as usize % 2].frame(before / 2 * 2 + 1)?;
        }
        frame.copy(planes, strides, self.width, self.height);
        Ok(())
    }
}
