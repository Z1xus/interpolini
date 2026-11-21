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

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn blend_plane_row_avx2(out: &mut [u8], lines: &[(&[u8], usize, u32)], y: usize) {
    use std::arch::x86_64::{
        _mm_loadu_si128, _mm_storeu_si128, _mm256_add_epi32, _mm256_castsi256_si128,
        _mm256_cvtepu8_epi16, _mm256_madd_epi16, _mm256_packus_epi16, _mm256_packus_epi32,
        _mm256_permute4x64_epi64, _mm256_set1_epi32, _mm256_setzero_si256, _mm256_srli_epi32,
        _mm256_unpackhi_epi16, _mm256_unpacklo_epi16,
    };
    let width = out.len();
    let half = _mm256_set1_epi32(1 << (SHIFT - 1));
    let zero = _mm256_setzero_si256();
    let pairs = lines.len() / 2;
    let odd = lines.len() % 2 == 1;
    let mut x = 0;
    while x + 16 <= width {
        let mut acc_lo = half;
        let mut acc_hi = half;
        for pair in 0..pairs {
            let (d0, s0, w0) = lines[pair * 2];
            let (d1, s1, w1) = lines[pair * 2 + 1];
            let w = _mm256_set1_epi32((w0 as i32) | ((w1 as i32) << 16));
            unsafe {
                let a = _mm256_cvtepu8_epi16(_mm_loadu_si128(d0.as_ptr().add(y * s0 + x).cast()));
                let b = _mm256_cvtepu8_epi16(_mm_loadu_si128(d1.as_ptr().add(y * s1 + x).cast()));
                let lo = _mm256_unpacklo_epi16(a, b);
                let hi = _mm256_unpackhi_epi16(a, b);
                acc_lo = _mm256_add_epi32(acc_lo, _mm256_madd_epi16(lo, w));
                acc_hi = _mm256_add_epi32(acc_hi, _mm256_madd_epi16(hi, w));
            }
        }
        if odd {
            let (d0, s0, w0) = lines[lines.len() - 1];
            let w = _mm256_set1_epi32(w0 as i32);
            unsafe {
                let a = _mm256_cvtepu8_epi16(_mm_loadu_si128(d0.as_ptr().add(y * s0 + x).cast()));
                let lo = _mm256_unpacklo_epi16(a, zero);
                let hi = _mm256_unpackhi_epi16(a, zero);
                acc_lo = _mm256_add_epi32(acc_lo, _mm256_madd_epi16(lo, w));
                acc_hi = _mm256_add_epi32(acc_hi, _mm256_madd_epi16(hi, w));
            }
        }
        let s_lo = _mm256_srli_epi32::<{ SHIFT as i32 }>(acc_lo);
        let s_hi = _mm256_srli_epi32::<{ SHIFT as i32 }>(acc_hi);
        let p16 = _mm256_packus_epi32(s_lo, s_hi);
        let p8 = _mm256_packus_epi16(p16, p16);
        let ord = _mm256_permute4x64_epi64::<0xD8>(p8);
        unsafe {
            _mm_storeu_si128(out.as_mut_ptr().add(x).cast(), _mm256_castsi256_si128(ord));
        }
        x += 16;
    }
    for (dx, sample) in out[x..].iter_mut().enumerate() {
        let col = x + dx;
        let mut sum = 1u32 << (SHIFT - 1);
        for &(data, stride, weight) in lines {
            sum += u32::from(data[y * stride + col]) * weight;
        }
        *sample = (sum >> SHIFT).min(255) as u8;
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn blend_nv12_uv_row_avx2(
    out_u: &mut [u8],
    out_v: &mut [u8],
    lines: &[(&[u8], usize, u32)],
    y: usize,
) {
    use std::arch::x86_64::{
        _mm_storeu_si128, _mm256_add_epi32, _mm256_and_si256, _mm256_castsi256_si128,
        _mm256_loadu_si256, _mm256_madd_epi16, _mm256_packus_epi16, _mm256_packus_epi32,
        _mm256_permute4x64_epi64, _mm256_set1_epi16, _mm256_set1_epi32, _mm256_setzero_si256,
        _mm256_srli_epi16, _mm256_srli_epi32, _mm256_unpackhi_epi16, _mm256_unpacklo_epi16,
    };
    let width = out_u.len();
    let half = _mm256_set1_epi32(1 << (SHIFT - 1));
    let mask = _mm256_set1_epi16(0x00FF);
    let zero = _mm256_setzero_si256();
    let pairs = lines.len() / 2;
    let odd = lines.len() % 2 == 1;
    let mut x = 0;
    while x + 16 <= width {
        let mut u_lo = half;
        let mut u_hi = half;
        let mut v_lo = half;
        let mut v_hi = half;
        for pair in 0..pairs {
            let (d0, s0, w0) = lines[pair * 2];
            let (d1, s1, w1) = lines[pair * 2 + 1];
            let w = _mm256_set1_epi32((w0 as i32) | ((w1 as i32) << 16));
            unsafe {
                let r0 = _mm256_loadu_si256(d0.as_ptr().add(y * s0 + x * 2).cast());
                let r1 = _mm256_loadu_si256(d1.as_ptr().add(y * s1 + x * 2).cast());
                let ua = _mm256_and_si256(r0, mask);
                let ub = _mm256_and_si256(r1, mask);
                u_lo = _mm256_add_epi32(u_lo, _mm256_madd_epi16(_mm256_unpacklo_epi16(ua, ub), w));
                u_hi = _mm256_add_epi32(u_hi, _mm256_madd_epi16(_mm256_unpackhi_epi16(ua, ub), w));
                let va = _mm256_srli_epi16::<8>(r0);
                let vb = _mm256_srli_epi16::<8>(r1);
                v_lo = _mm256_add_epi32(v_lo, _mm256_madd_epi16(_mm256_unpacklo_epi16(va, vb), w));
                v_hi = _mm256_add_epi32(v_hi, _mm256_madd_epi16(_mm256_unpackhi_epi16(va, vb), w));
            }
        }
        if odd {
            let (d0, s0, w0) = lines[lines.len() - 1];
            let w = _mm256_set1_epi32(w0 as i32);
            unsafe {
                let r0 = _mm256_loadu_si256(d0.as_ptr().add(y * s0 + x * 2).cast());
                let ua = _mm256_and_si256(r0, mask);
                u_lo =
                    _mm256_add_epi32(u_lo, _mm256_madd_epi16(_mm256_unpacklo_epi16(ua, zero), w));
                u_hi =
                    _mm256_add_epi32(u_hi, _mm256_madd_epi16(_mm256_unpackhi_epi16(ua, zero), w));
                let va = _mm256_srli_epi16::<8>(r0);
                v_lo =
                    _mm256_add_epi32(v_lo, _mm256_madd_epi16(_mm256_unpacklo_epi16(va, zero), w));
                v_hi =
                    _mm256_add_epi32(v_hi, _mm256_madd_epi16(_mm256_unpackhi_epi16(va, zero), w));
            }
        }
        let u16v = _mm256_packus_epi32(
            _mm256_srli_epi32::<{ SHIFT as i32 }>(u_lo),
            _mm256_srli_epi32::<{ SHIFT as i32 }>(u_hi),
        );
        let u8v = _mm256_permute4x64_epi64::<0xD8>(_mm256_packus_epi16(u16v, u16v));
        let v16v = _mm256_packus_epi32(
            _mm256_srli_epi32::<{ SHIFT as i32 }>(v_lo),
            _mm256_srli_epi32::<{ SHIFT as i32 }>(v_hi),
        );
        let v8v = _mm256_permute4x64_epi64::<0xD8>(_mm256_packus_epi16(v16v, v16v));
        unsafe {
            _mm_storeu_si128(
                out_u.as_mut_ptr().add(x).cast(),
                _mm256_castsi256_si128(u8v),
            );
            _mm_storeu_si128(
                out_v.as_mut_ptr().add(x).cast(),
                _mm256_castsi256_si128(v8v),
            );
        }
        x += 16;
    }
    for dx in 0..(width - x) {
        let col = x + dx;
        let mut su = 1u32 << (SHIFT - 1);
        let mut sv = 1u32 << (SHIFT - 1);
        for &(data, stride, weight) in lines {
            let at = y * stride + col * 2;
            su += u32::from(data[at]) * weight;
            sv += u32::from(data[at + 1]) * weight;
        }
        out_u[col] = (su >> SHIFT).min(255) as u8;
        out_v[col] = (sv >> SHIFT).min(255) as u8;
    }
}

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
        #[cfg(target_arch = "x86_64")]
        let simd = std::arch::is_x86_feature_detected!("avx2")
            && self.weights.len() >= 2
            && self.weights.iter().all(|&w| w <= 32767);
        let all_nv12 = !yuv444 && held.iter().all(|(_, p)| p.format() == Pixel::NV12);
        let any_nv12 = !yuv444 && held.iter().any(|(_, p)| p.format() == Pixel::NV12);
        let planes_end = if all_nv12 { 1 } else { 3 };
        for plane in 0..planes_end {
            let shift = usize::from(plane > 0 && !yuv444);
            let (width, height) = (width as usize >> shift, height as usize >> shift);
            let stride = output.stride(plane);
            let rows = output
                .data_mut(plane)
                .par_chunks_mut(stride)
                .take(height)
                .enumerate()
                .with_min_len(budget::piece(height));
            // luma only, as in open-svpflow
            if let Some(curve) = self.curve.filter(|_| plane == 0) {
                let lines: Vec<(&[u8], usize)> = held
                    .iter()
                    .map(|(_, picture)| (picture.data(0), picture.stride(0)))
                    .collect();
                budget::install(|| {
                    rows.for_each_init(
                        || vec![0f32; width],
                        |sums, (y, row)| {
                            sums.fill(0.0);
                            for (&(data, stride), &weight) in lines.iter().zip(&self.exact) {
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
            if plane > 0 && any_nv12 {
                let lines: Vec<(&[u8], usize, u32, bool)> = held
                    .iter()
                    .zip(&self.weights)
                    .map(|((_, picture), &weight)| {
                        let nv12 = picture.format() == Pixel::NV12;
                        let src_plane = if nv12 { 1 } else { plane };
                        (
                            picture.data(src_plane),
                            picture.stride(src_plane),
                            weight,
                            nv12,
                        )
                    })
                    .collect();
                let ch_off = plane - 1;
                budget::install(|| {
                    rows.for_each_init(
                        || vec![0u32; width],
                        |sums, (y, row)| {
                            sums.fill(1 << (SHIFT - 1));
                            for &(data, stride, weight, nv12) in &lines {
                                let src = &data[y * stride..];
                                if nv12 {
                                    for (x, sum) in sums.iter_mut().enumerate() {
                                        *sum += u32::from(src[x * 2 + ch_off]) * weight;
                                    }
                                } else {
                                    for (sum, &sample) in sums.iter_mut().zip(src) {
                                        *sum += u32::from(sample) * weight;
                                    }
                                }
                            }
                            for (sample, sum) in row.iter_mut().zip(sums.iter()) {
                                *sample = (sum >> SHIFT).min(255) as u8;
                            }
                        },
                    )
                });
                continue;
            }
            let lines: Vec<(&[u8], usize, u32)> = held
                .iter()
                .zip(&self.weights)
                .map(|((_, picture), &weight)| (picture.data(plane), picture.stride(plane), weight))
                .collect();
            #[cfg(target_arch = "x86_64")]
            if simd {
                budget::install(|| {
                    rows.for_each(|(y, row)| unsafe {
                        blend_plane_row_avx2(&mut row[..width], &lines, y);
                    })
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
        if all_nv12 {
            let (cw, ch) = (width as usize >> 1, height as usize >> 1);
            let (u_stride, v_stride) = (output.stride(1), output.stride(2));
            let lines: Vec<(&[u8], usize, u32)> = held
                .iter()
                .zip(&self.weights)
                .map(|((_, picture), &weight)| (picture.data(1), picture.stride(1), weight))
                .collect();
            // ffmpeg allocates planes 1 and 2 as disjoint buffers in YUV420P
            let raw = unsafe { &*output.as_ptr() };
            let (u_out, v_out) = unsafe {
                (
                    std::slice::from_raw_parts_mut(raw.data[1], u_stride * ch),
                    std::slice::from_raw_parts_mut(raw.data[2], v_stride * ch),
                )
            };
            let rows = u_out
                .par_chunks_mut(u_stride)
                .take(ch)
                .zip(v_out.par_chunks_mut(v_stride).take(ch))
                .enumerate()
                .with_min_len(budget::piece(ch));
            #[cfg(target_arch = "x86_64")]
            if simd {
                budget::install(|| {
                    rows.for_each(|(y, (u_row, v_row))| unsafe {
                        blend_nv12_uv_row_avx2(&mut u_row[..cw], &mut v_row[..cw], &lines, y);
                    })
                });
            } else {
                budget::install(|| {
                    rows.for_each_init(
                        || (vec![0u32; cw], vec![0u32; cw]),
                        |(su, sv), (y, (u_row, v_row))| {
                            su.fill(1 << (SHIFT - 1));
                            sv.fill(1 << (SHIFT - 1));
                            for &(data, stride, weight) in &lines {
                                let (pairs, _) = data[y * stride..][..cw * 2].as_chunks::<2>();
                                for ((u, v), pair) in su.iter_mut().zip(sv.iter_mut()).zip(pairs) {
                                    *u += u32::from(pair[0]) * weight;
                                    *v += u32::from(pair[1]) * weight;
                                }
                            }
                            for ((u, &sum_u), (v, &sum_v)) in u_row
                                .iter_mut()
                                .zip(su.iter())
                                .zip(v_row.iter_mut().zip(sv.iter()))
                            {
                                *u = (sum_u >> SHIFT).min(255) as u8;
                                *v = (sum_v >> SHIFT).min(255) as u8;
                            }
                        },
                    )
                });
            }
            #[cfg(not(target_arch = "x86_64"))]
            budget::install(|| {
                rows.for_each_init(
                    || (vec![0u32; cw], vec![0u32; cw]),
                    |(su, sv), (y, (u_row, v_row))| {
                        su.fill(1 << (SHIFT - 1));
                        sv.fill(1 << (SHIFT - 1));
                        for &(data, stride, weight) in &lines {
                            let (pairs, _) = data[y * stride..][..cw * 2].as_chunks::<2>();
                            for ((u, v), pair) in su.iter_mut().zip(sv.iter_mut()).zip(pairs) {
                                *u += u32::from(pair[0]) * weight;
                                *v += u32::from(pair[1]) * weight;
                            }
                        }
                        for ((u, &sum_u), (v, &sum_v)) in u_row
                            .iter_mut()
                            .zip(su.iter())
                            .zip(v_row.iter_mut().zip(sv.iter()))
                        {
                            *u = (sum_u >> SHIFT).min(255) as u8;
                            *v = (sum_v >> SHIFT).min(255) as u8;
                        }
                    },
                )
            });
        }
        Ok(output)
    }
}
