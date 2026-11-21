use std::path::Path;

use rayon::prelude::*;

use crate::Result;
use crate::budget;
use crate::decode::{Decoder, Meta};
use crate::work::{Cancel, Follow};

const STEP: usize = 4;
const SPOTS: i32 = 3;

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn sad_avx2(first: &[u8], second: &[u8]) -> u64 {
    use std::arch::x86_64::{
        _mm256_add_epi64, _mm256_extract_epi64, _mm256_loadu_si256, _mm256_sad_epu8,
        _mm256_setzero_si256,
    };
    let len = first.len().min(second.len());
    let mut acc = _mm256_setzero_si256();
    let mut i = 0;
    while i + 32 <= len {
        unsafe {
            let va = _mm256_loadu_si256(first.as_ptr().add(i).cast());
            let vb = _mm256_loadu_si256(second.as_ptr().add(i).cast());
            acc = _mm256_add_epi64(acc, _mm256_sad_epu8(va, vb));
        }
        i += 32;
    }
    let mut sum = (_mm256_extract_epi64::<0>(acc) as u64)
        + (_mm256_extract_epi64::<1>(acc) as u64)
        + (_mm256_extract_epi64::<2>(acc) as u64)
        + (_mm256_extract_epi64::<3>(acc) as u64);
    for (&a, &b) in first[i..len].iter().zip(&second[i..len]) {
        sum += u64::from(a.abs_diff(b));
    }
    sum
}

#[cfg(target_arch = "aarch64")]
fn sad_neon(first: &[u8], second: &[u8]) -> u64 {
    use std::arch::aarch64::{
        vabdq_u8, vaddlvq_u32, vdupq_n_u32, vld1q_u8, vpadalq_u16, vpaddlq_u8,
    };
    let len = first.len().min(second.len());
    let mut acc = unsafe { vdupq_n_u32(0) };
    let mut i = 0;
    while i + 16 <= len {
        unsafe {
            let va = vld1q_u8(first.as_ptr().add(i));
            let vb = vld1q_u8(second.as_ptr().add(i));
            acc = vpadalq_u16(acc, vpaddlq_u8(vabdq_u8(va, vb)));
        }
        i += 16;
    }
    let mut sum = unsafe { vaddlvq_u32(acc) };
    for (&a, &b) in first[i..len].iter().zip(&second[i..len]) {
        sum += u64::from(a.abs_diff(b));
    }
    sum
}

fn distance(first: &[u8], second: &[u8]) -> u64 {
    #[cfg(target_arch = "x86_64")]
    if std::arch::is_x86_feature_detected!("avx2") {
        return unsafe { sad_avx2(first, second) };
    }
    #[cfg(target_arch = "aarch64")]
    {
        return sad_neon(first, second);
    }
    #[allow(unreachable_code)]
    first
        .iter()
        .zip(second)
        .map(|(&a, &b)| u64::from(a.abs_diff(b)))
        .sum()
}

pub fn differences(clip: &Path, from: f64, to: f64, cancel: &Cancel) -> Result<Vec<Vec<f32>>> {
    const SAMPLE: i32 = 100;
    let mut probe = Decoder::chosen(clip, |_, height| budget::own().min(budget::useful(height)))?
        .following(&Follow::new(cancel));
    let meta = probe.meta();
    let Meta {
        width,
        height,
        frames,
        fps,
        ..
    } = meta;
    let (width, height) = (width as usize, height as usize);
    let rows = height.div_ceil(STEP);
    let bytes = width * rows;
    let denom = bytes.max(1) as f32;
    let rate = fps.0 as f64 / fps.1 as f64;
    let (start, end) = ((from * rate) as i32, ((to * rate) as i32).min(frames));
    let span = (end - start).max(0);
    if span <= SAMPLE * SPOTS {
        if span <= 1 {
            return Ok(vec![Vec::new()]);
        }
        let mut pictures = [vec![0u8; bytes], vec![0u8; bytes]];
        let mut row = Vec::with_capacity((span - 1) as usize);
        for step in 0..span {
            cancel.check()?;
            probe.luma_sampled(start + step, STEP, &mut pictures[step as usize % 2])?;
            if step > 0 {
                row.push(distance(&pictures[0], &pictures[1]) as f32 / denom);
            }
        }
        return Ok(vec![row]);
    }
    let threads = (budget::own().min(budget::useful(meta.height)) / SPOTS as usize).max(1);
    let third = span / SPOTS;
    let mut probe = Some(probe);
    let tasks: Vec<_> = (0..SPOTS)
        .map(|spot| {
            let zone_start = start + spot * third;
            let zone_end = if spot + 1 == SPOTS {
                end
            } else {
                start + (spot + 1) * third
            };
            let first = zone_start + (zone_end - zone_start - SAMPLE) / 2;
            (
                first..(first + SAMPLE).min(zone_end),
                if spot == 0 { probe.take() } else { None },
            )
        })
        .collect();
    let run = |(range, reused): (std::ops::Range<i32>, Option<Decoder>)| -> Result<Vec<f32>> {
        let mut decoder = match reused {
            Some(decoder) => decoder,
            None => Decoder::threaded(clip, threads)?.following(&Follow::new(cancel)),
        };
        let mut pictures = [vec![0u8; bytes], vec![0u8; bytes]];
        let len = (range.end - range.start).max(0);
        let mut row = Vec::with_capacity(len.saturating_sub(1) as usize);
        for step in 0..len {
            cancel.check()?;
            decoder.luma_sampled(range.start + step, STEP, &mut pictures[step as usize % 2])?;
            if step > 0 {
                row.push(distance(&pictures[0], &pictures[1]) as f32 / denom);
            }
        }
        Ok(row)
    };
    budget::install(|| tasks.into_par_iter().map(run).collect())
}

fn gap(differences: &[Vec<f32>]) -> f32 {
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
