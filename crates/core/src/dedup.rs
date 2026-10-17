use std::path::Path;
use std::sync::Arc;

use crate::Result;
use crate::decode::{Decoder, Meta};
use crate::svp::{Clip, Reader};

// every fourth sample of every fourth row is enough to find a repeated frame
const STEP: usize = 4;

pub struct Pick {
    pub source: Arc<Clip>,
    pub step: i32,
    pub offset: i32,
    pub width: usize,
    pub height: usize,
    pub chroma: usize,
}

impl Reader for Pick {
    fn read(&self, n: i32, planes: [*mut u8; 3], strides: [isize; 3]) -> Result<()> {
        let frame = self.source.frame(n * self.step + self.offset)?;
        frame.copy(planes, strides, self.width, self.height, self.chroma);
        Ok(())
    }
}

fn distance(first: &[u8], second: &[u8]) -> u64 {
    let pairs = first.iter().zip(second).step_by(STEP);
    pairs.map(|(a, b)| u64::from(a.abs_diff(*b))).sum()
}

fn samples(width: usize, height: usize) -> f32 {
    (width.div_ceil(STEP) * height.div_ceil(STEP)) as f32
}

// how much each sampled frame differs from the frame before it, in rows of frames from three places
pub fn differences(clip: &Path, from: f64, to: f64) -> Result<Vec<Vec<f32>>> {
    const SPOTS: i32 = 3;
    const SAMPLE: i32 = 100;
    let mut decoder = Decoder::threaded(clip, 0)?;
    let meta = decoder.meta();
    let Meta {
        width,
        height,
        frames,
        fps,
        ..
    } = meta;
    let (width, height) = (width as usize, height as usize);
    let luma = width * height;
    let chroma = luma >> (meta.shift() * 2);
    let mut pictures = [vec![0u8; luma + chroma * 2], vec![0u8; luma + chroma * 2]];
    let rate = fps.0 as f64 / fps.1 as f64;
    let (start, end) = ((from * rate) as i32, ((to * rate) as i32).min(frames));
    let mut spots = Vec::new();
    for spot in 1..=SPOTS {
        let first = (start + (end - start) * spot / (SPOTS + 1) - SAMPLE / 2).max(start);
        let mut row = Vec::new();
        for step in 0..SAMPLE.min(end - first) {
            let picture = pictures[step as usize % 2].as_mut_ptr();
            let planes = unsafe { [picture, picture.add(luma), picture.add(luma + chroma)] };
            let line = (width >> meta.shift()) as isize;
            let strides = [width as isize, line, line];
            decoder.read(first + step, planes, strides)?;
            let lines = (0..height)
                .step_by(STEP)
                .map(|y| y * width..(y + 1) * width);
            let total: u64 = lines
                .map(|line| distance(&pictures[0][line.clone()], &pictures[1][line]))
                .sum();
            if step > 0 {
                row.push(total as f32 / samples(width, height));
            }
        }
        spots.push(row);
    }
    Ok(spots)
}

fn gap(differences: &[Vec<f32>]) -> f32 {
    // keeps exact copies finite
    const SHIFT: f32 = 0.01;
    const REPEAT: f32 = 0.1;
    let mut logs: Vec<f32> = differences
        .iter()
        .flatten()
        .map(|d| (d + SHIFT).ln())
        .collect();
    logs.sort_by(f32::total_cmp);
    let (count, total) = (logs.len() as f32, logs.iter().sum::<f32>());
    let (mut below, mut best) = (0.0, (0.0, 0.0, 0.0));
    for i in 1..logs.len() {
        below += logs[i - 1];
        let (low, high) = (below / i as f32, (total - below) / (count - i as f32));
        let share = i as f32 / count;
        let spread = share * (1.0 - share) * (low - high).powi(2);
        if spread > best.0 {
            best = (spread, (logs[i - 1] + logs[i]) / 2.0, low);
        }
    }
    match best.2.exp() - SHIFT < REPEAT {
        true => best.1.exp() - SHIFT,
        false => 0.0,
    }
}

pub fn repeats(differences: &[Vec<f32>]) -> f32 {
    let gap = gap(differences);
    let all = differences.iter().flatten();
    let below = all.clone().filter(|difference| **difference < gap).count();
    below as f32 / all.count().max(1) as f32
}

pub fn threshold(differences: &[Vec<f32>], share: f32) -> f32 {
    let gap = gap(differences);
    let all = differences.iter().flatten().copied();
    let mut repeats: Vec<f32> = all.filter(|difference| *difference < gap).collect();
    repeats.sort_by(f32::total_cmp);
    match (share * repeats.len() as f32).round() as usize {
        0 => 0.0,
        count if count >= repeats.len() => gap,
        count => repeats[count - 1].next_up(),
    }
}
