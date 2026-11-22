use std::collections::VecDeque;
use std::sync::Mutex;

use ffmpeg_next::ffi;
use ffmpeg_next::format::Pixel;
use ffmpeg_next::frame;
use rayon::prelude::*;

use crate::Result;
use crate::budget;
use crate::compose::share;
use crate::config::Blending;
use crate::decode;

const SHIFT: u32 = 15;
const INEXACT: u32 = 13;
const BLOCK_WINDOW: usize = 12;
const BLOCK_FRAMES: usize = 8;
const TILE: usize = 256;

#[cfg(target_arch = "x86_64")]
#[repr(C, align(32))]
struct AccU16([u16; TILE]);

#[repr(C, align(32))]
struct AccU32([u32; TILE]);

#[repr(C, align(32))]
struct AccF32([f32; TILE]);

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
unsafe fn blend_plane_row_blocked_avx2(out: &mut [u8], lines: &[(&[u8], usize, u32)], y: usize) {
    use std::arch::x86_64::{
        _mm_loadu_si128, _mm_storeu_si128, _mm256_add_epi32, _mm256_castsi256_si128,
        _mm256_cvtepu8_epi16, _mm256_load_si256, _mm256_madd_epi16, _mm256_packus_epi16,
        _mm256_packus_epi32, _mm256_permute4x64_epi64, _mm256_set1_epi32, _mm256_setzero_si256,
        _mm256_srli_epi32, _mm256_store_si256, _mm256_unpackhi_epi16, _mm256_unpacklo_epi16,
    };
    let width = out.len();
    let zero = _mm256_setzero_si256();
    let mut acc = AccU32([0; TILE]);
    let mut x0 = 0;
    while x0 < width {
        let len = (width - x0).min(TILE);
        let vec_len = len & !15;
        acc.0[..len].fill(1 << (SHIFT - 1));
        for chunk in lines.chunks(BLOCK_FRAMES) {
            let pairs = chunk.len() / 2;
            let odd = chunk.len() % 2 == 1;
            let mut x = 0;
            while x < vec_len {
                unsafe {
                    let ptr = acc.0.as_mut_ptr().add(x).cast();
                    let mut acc_lo = _mm256_load_si256(ptr);
                    let mut acc_hi = _mm256_load_si256(ptr.add(1));
                    for pair in 0..pairs {
                        let (d0, s0, w0) = chunk[pair * 2];
                        let (d1, s1, w1) = chunk[pair * 2 + 1];
                        let w = _mm256_set1_epi32((w0 as i32) | ((w1 as i32) << 16));
                        let a = _mm256_cvtepu8_epi16(_mm_loadu_si128(
                            d0.as_ptr().add(y * s0 + x0 + x).cast(),
                        ));
                        let b = _mm256_cvtepu8_epi16(_mm_loadu_si128(
                            d1.as_ptr().add(y * s1 + x0 + x).cast(),
                        ));
                        let lo = _mm256_unpacklo_epi16(a, b);
                        let hi = _mm256_unpackhi_epi16(a, b);
                        acc_lo = _mm256_add_epi32(acc_lo, _mm256_madd_epi16(lo, w));
                        acc_hi = _mm256_add_epi32(acc_hi, _mm256_madd_epi16(hi, w));
                    }
                    if odd {
                        let (d0, s0, w0) = chunk[chunk.len() - 1];
                        let w = _mm256_set1_epi32(w0 as i32);
                        let a = _mm256_cvtepu8_epi16(_mm_loadu_si128(
                            d0.as_ptr().add(y * s0 + x0 + x).cast(),
                        ));
                        let lo = _mm256_unpacklo_epi16(a, zero);
                        let hi = _mm256_unpackhi_epi16(a, zero);
                        acc_lo = _mm256_add_epi32(acc_lo, _mm256_madd_epi16(lo, w));
                        acc_hi = _mm256_add_epi32(acc_hi, _mm256_madd_epi16(hi, w));
                    }
                    _mm256_store_si256(ptr, acc_lo);
                    _mm256_store_si256(ptr.add(1), acc_hi);
                }
                x += 16;
            }
            for dx in vec_len..len {
                let col = x0 + dx;
                let mut sum = acc.0[dx];
                for &(data, stride, weight) in chunk {
                    sum += u32::from(data[y * stride + col]) * weight;
                }
                acc.0[dx] = sum;
            }
        }
        let mut x = 0;
        while x < vec_len {
            unsafe {
                let ptr = acc.0.as_ptr().add(x).cast();
                let s_lo = _mm256_srli_epi32::<{ SHIFT as i32 }>(_mm256_load_si256(ptr));
                let s_hi = _mm256_srli_epi32::<{ SHIFT as i32 }>(_mm256_load_si256(ptr.add(1)));
                let p16 = _mm256_packus_epi32(s_lo, s_hi);
                let p8 = _mm256_packus_epi16(p16, p16);
                let ord = _mm256_permute4x64_epi64::<0xD8>(p8);
                _mm_storeu_si128(
                    out.as_mut_ptr().add(x0 + x).cast(),
                    _mm256_castsi256_si128(ord),
                );
            }
            x += 16;
        }
        for dx in vec_len..len {
            out[x0 + dx] = (acc.0[dx] >> SHIFT).min(255) as u8;
        }
        x0 += len;
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
    for col in x..width {
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

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn blend_nv12_uv_row_blocked_avx2(
    out_u: &mut [u8],
    out_v: &mut [u8],
    lines: &[(&[u8], usize, u32)],
    y: usize,
) {
    use std::arch::x86_64::{
        _mm_storeu_si128, _mm256_add_epi32, _mm256_and_si256, _mm256_castsi256_si128,
        _mm256_load_si256, _mm256_loadu_si256, _mm256_madd_epi16, _mm256_packus_epi16,
        _mm256_packus_epi32, _mm256_permute4x64_epi64, _mm256_set1_epi16, _mm256_set1_epi32,
        _mm256_setzero_si256, _mm256_srli_epi16, _mm256_srli_epi32, _mm256_store_si256,
        _mm256_unpackhi_epi16, _mm256_unpacklo_epi16,
    };
    let width = out_u.len();
    let mask = _mm256_set1_epi16(0x00FF);
    let zero = _mm256_setzero_si256();
    let mut acc_u = AccU32([0; TILE]);
    let mut acc_v = AccU32([0; TILE]);
    let mut x0 = 0;
    while x0 < width {
        let len = (width - x0).min(TILE);
        let vec_len = len & !15;
        acc_u.0[..len].fill(1 << (SHIFT - 1));
        acc_v.0[..len].fill(1 << (SHIFT - 1));
        for chunk in lines.chunks(BLOCK_FRAMES) {
            let pairs = chunk.len() / 2;
            let odd = chunk.len() % 2 == 1;
            let mut x = 0;
            while x < vec_len {
                unsafe {
                    let pu = acc_u.0.as_mut_ptr().add(x).cast();
                    let pv = acc_v.0.as_mut_ptr().add(x).cast();
                    let mut u_lo = _mm256_load_si256(pu);
                    let mut u_hi = _mm256_load_si256(pu.add(1));
                    let mut v_lo = _mm256_load_si256(pv);
                    let mut v_hi = _mm256_load_si256(pv.add(1));
                    for pair in 0..pairs {
                        let (d0, s0, w0) = chunk[pair * 2];
                        let (d1, s1, w1) = chunk[pair * 2 + 1];
                        let w = _mm256_set1_epi32((w0 as i32) | ((w1 as i32) << 16));
                        let r0 = _mm256_loadu_si256(d0.as_ptr().add(y * s0 + (x0 + x) * 2).cast());
                        let r1 = _mm256_loadu_si256(d1.as_ptr().add(y * s1 + (x0 + x) * 2).cast());
                        let ua = _mm256_and_si256(r0, mask);
                        let ub = _mm256_and_si256(r1, mask);
                        u_lo = _mm256_add_epi32(
                            u_lo,
                            _mm256_madd_epi16(_mm256_unpacklo_epi16(ua, ub), w),
                        );
                        u_hi = _mm256_add_epi32(
                            u_hi,
                            _mm256_madd_epi16(_mm256_unpackhi_epi16(ua, ub), w),
                        );
                        let va = _mm256_srli_epi16::<8>(r0);
                        let vb = _mm256_srli_epi16::<8>(r1);
                        v_lo = _mm256_add_epi32(
                            v_lo,
                            _mm256_madd_epi16(_mm256_unpacklo_epi16(va, vb), w),
                        );
                        v_hi = _mm256_add_epi32(
                            v_hi,
                            _mm256_madd_epi16(_mm256_unpackhi_epi16(va, vb), w),
                        );
                    }
                    if odd {
                        let (d0, s0, w0) = chunk[chunk.len() - 1];
                        let w = _mm256_set1_epi32(w0 as i32);
                        let r0 = _mm256_loadu_si256(d0.as_ptr().add(y * s0 + (x0 + x) * 2).cast());
                        let ua = _mm256_and_si256(r0, mask);
                        u_lo = _mm256_add_epi32(
                            u_lo,
                            _mm256_madd_epi16(_mm256_unpacklo_epi16(ua, zero), w),
                        );
                        u_hi = _mm256_add_epi32(
                            u_hi,
                            _mm256_madd_epi16(_mm256_unpackhi_epi16(ua, zero), w),
                        );
                        let va = _mm256_srli_epi16::<8>(r0);
                        v_lo = _mm256_add_epi32(
                            v_lo,
                            _mm256_madd_epi16(_mm256_unpacklo_epi16(va, zero), w),
                        );
                        v_hi = _mm256_add_epi32(
                            v_hi,
                            _mm256_madd_epi16(_mm256_unpackhi_epi16(va, zero), w),
                        );
                    }
                    _mm256_store_si256(pu, u_lo);
                    _mm256_store_si256(pu.add(1), u_hi);
                    _mm256_store_si256(pv, v_lo);
                    _mm256_store_si256(pv.add(1), v_hi);
                }
                x += 16;
            }
            for dx in vec_len..len {
                let col = x0 + dx;
                let mut su = acc_u.0[dx];
                let mut sv = acc_v.0[dx];
                for &(data, stride, weight) in chunk {
                    let at = y * stride + col * 2;
                    su += u32::from(data[at]) * weight;
                    sv += u32::from(data[at + 1]) * weight;
                }
                acc_u.0[dx] = su;
                acc_v.0[dx] = sv;
            }
        }
        let mut x = 0;
        while x < vec_len {
            unsafe {
                let pu = acc_u.0.as_ptr().add(x).cast();
                let pv = acc_v.0.as_ptr().add(x).cast();
                let u16v = _mm256_packus_epi32(
                    _mm256_srli_epi32::<{ SHIFT as i32 }>(_mm256_load_si256(pu)),
                    _mm256_srli_epi32::<{ SHIFT as i32 }>(_mm256_load_si256(pu.add(1))),
                );
                let u8v = _mm256_permute4x64_epi64::<0xD8>(_mm256_packus_epi16(u16v, u16v));
                let v16v = _mm256_packus_epi32(
                    _mm256_srli_epi32::<{ SHIFT as i32 }>(_mm256_load_si256(pv)),
                    _mm256_srli_epi32::<{ SHIFT as i32 }>(_mm256_load_si256(pv.add(1))),
                );
                let v8v = _mm256_permute4x64_epi64::<0xD8>(_mm256_packus_epi16(v16v, v16v));
                _mm_storeu_si128(
                    out_u.as_mut_ptr().add(x0 + x).cast(),
                    _mm256_castsi256_si128(u8v),
                );
                _mm_storeu_si128(
                    out_v.as_mut_ptr().add(x0 + x).cast(),
                    _mm256_castsi256_si128(v8v),
                );
            }
            x += 16;
        }
        for dx in vec_len..len {
            out_u[x0 + dx] = (acc_u.0[dx] >> SHIFT).min(255) as u8;
            out_v[x0 + dx] = (acc_v.0[dx] >> SHIFT).min(255) as u8;
        }
        x0 += len;
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2", enable = "fma")]
unsafe fn blend_gamma_row_avx2(
    out: &mut [u8],
    lines: &[(&[u8], usize)],
    weights: &[f32],
    curve: &[f32; 256],
    inverse: &[u8],
    y: usize,
) {
    use std::arch::x86_64::{
        _mm256_fmadd_ps, _mm256_load_ps, _mm256_set1_ps, _mm256_setr_ps, _mm256_store_ps,
    };
    let width = out.len();
    let mut acc = AccF32([0.0; TILE]);
    let mut x0 = 0;
    while x0 < width {
        let len = (width - x0).min(TILE);
        let vec_len = len & !7;
        acc.0[..len].fill(0.0);
        for (&(data, stride), &weight) in lines.iter().zip(weights) {
            let src = &data[y * stride + x0..][..len];
            let w = _mm256_set1_ps(weight);
            let mut x = 0;
            while x < vec_len {
                unsafe {
                    let s = src.as_ptr().add(x);
                    let v = _mm256_setr_ps(
                        *curve.get_unchecked(usize::from(*s)),
                        *curve.get_unchecked(usize::from(*s.add(1))),
                        *curve.get_unchecked(usize::from(*s.add(2))),
                        *curve.get_unchecked(usize::from(*s.add(3))),
                        *curve.get_unchecked(usize::from(*s.add(4))),
                        *curve.get_unchecked(usize::from(*s.add(5))),
                        *curve.get_unchecked(usize::from(*s.add(6))),
                        *curve.get_unchecked(usize::from(*s.add(7))),
                    );
                    let ptr = acc.0.as_mut_ptr().add(x);
                    _mm256_store_ps(ptr, _mm256_fmadd_ps(v, w, _mm256_load_ps(ptr)));
                }
                x += 8;
            }
            for i in vec_len..len {
                acc.0[i] = curve[usize::from(src[i])].mul_add(weight, acc.0[i]);
            }
        }
        for i in 0..len {
            let index = acc.0[i].clamp(0.0, 1.0).to_bits() >> INEXACT;
            out[x0 + i] = inverse[index as usize];
        }
        x0 += len;
    }
}

#[cfg(target_arch = "aarch64")]
fn blend_plane_row_neon(out: &mut [u8], lines: &[(&[u8], usize, u32)], y: usize) {
    use std::arch::aarch64::{
        vcombine_u8, vcombine_u16, vdup_n_u16, vdupq_n_u16, vdupq_n_u32, vget_low_u8, vget_low_u16,
        vld1q_u8, vmlal_high_u16, vmlal_u16, vmovl_high_u8, vmovl_u8, vqmovn_u16, vqshrn_n_u32,
        vst1q_u8,
    };
    let width = out.len();
    let half = unsafe { vdupq_n_u32(1 << (SHIFT - 1)) };
    let mut x = 0;
    while x + 16 <= width {
        let (mut a0, mut a1, mut a2, mut a3) = (half, half, half, half);
        for &(data, stride, weight) in lines {
            unsafe {
                let w4 = vdup_n_u16(weight as u16);
                let w8 = vdupq_n_u16(weight as u16);
                let px = vld1q_u8(data.as_ptr().add(y * stride + x));
                let lo = vmovl_u8(vget_low_u8(px));
                let hi = vmovl_high_u8(px);
                a0 = vmlal_u16(a0, vget_low_u16(lo), w4);
                a1 = vmlal_high_u16(a1, lo, w8);
                a2 = vmlal_u16(a2, vget_low_u16(hi), w4);
                a3 = vmlal_high_u16(a3, hi, w8);
            }
        }
        unsafe {
            let p0 = vcombine_u16(
                vqshrn_n_u32::<{ SHIFT as i32 }>(a0),
                vqshrn_n_u32::<{ SHIFT as i32 }>(a1),
            );
            let p1 = vcombine_u16(
                vqshrn_n_u32::<{ SHIFT as i32 }>(a2),
                vqshrn_n_u32::<{ SHIFT as i32 }>(a3),
            );
            vst1q_u8(
                out.as_mut_ptr().add(x),
                vcombine_u8(vqmovn_u16(p0), vqmovn_u16(p1)),
            );
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

#[cfg(target_arch = "aarch64")]
fn blend_plane_row_blocked_neon(out: &mut [u8], lines: &[(&[u8], usize, u32)], y: usize) {
    use std::arch::aarch64::{
        vcombine_u8, vcombine_u16, vdup_n_u16, vdupq_n_u16, vget_low_u8, vget_low_u16, vld1q_u8,
        vld1q_u32, vmlal_high_u16, vmlal_u16, vmovl_high_u8, vmovl_u8, vqmovn_u16, vqshrn_n_u32,
        vst1q_u8, vst1q_u32,
    };
    let width = out.len();
    let mut acc = AccU32([0; TILE]);
    let mut x0 = 0;
    while x0 < width {
        let len = (width - x0).min(TILE);
        let vec_len = len & !15;
        acc.0[..len].fill(1 << (SHIFT - 1));
        for chunk in lines.chunks(BLOCK_FRAMES) {
            let mut x = 0;
            while x < vec_len {
                unsafe {
                    let ptr = acc.0.as_mut_ptr().add(x);
                    let mut a0 = vld1q_u32(ptr);
                    let mut a1 = vld1q_u32(ptr.add(4));
                    let mut a2 = vld1q_u32(ptr.add(8));
                    let mut a3 = vld1q_u32(ptr.add(12));
                    for &(data, stride, weight) in chunk {
                        let w4 = vdup_n_u16(weight as u16);
                        let w8 = vdupq_n_u16(weight as u16);
                        let px = vld1q_u8(data.as_ptr().add(y * stride + x0 + x));
                        let lo = vmovl_u8(vget_low_u8(px));
                        let hi = vmovl_high_u8(px);
                        a0 = vmlal_u16(a0, vget_low_u16(lo), w4);
                        a1 = vmlal_high_u16(a1, lo, w8);
                        a2 = vmlal_u16(a2, vget_low_u16(hi), w4);
                        a3 = vmlal_high_u16(a3, hi, w8);
                    }
                    vst1q_u32(ptr, a0);
                    vst1q_u32(ptr.add(4), a1);
                    vst1q_u32(ptr.add(8), a2);
                    vst1q_u32(ptr.add(12), a3);
                }
                x += 16;
            }
            for dx in vec_len..len {
                let col = x0 + dx;
                let mut sum = acc.0[dx];
                for &(data, stride, weight) in chunk {
                    sum += u32::from(data[y * stride + col]) * weight;
                }
                acc.0[dx] = sum;
            }
        }
        let mut x = 0;
        while x < vec_len {
            unsafe {
                let ptr = acc.0.as_ptr().add(x);
                let p0 = vcombine_u16(
                    vqshrn_n_u32::<{ SHIFT as i32 }>(vld1q_u32(ptr)),
                    vqshrn_n_u32::<{ SHIFT as i32 }>(vld1q_u32(ptr.add(4))),
                );
                let p1 = vcombine_u16(
                    vqshrn_n_u32::<{ SHIFT as i32 }>(vld1q_u32(ptr.add(8))),
                    vqshrn_n_u32::<{ SHIFT as i32 }>(vld1q_u32(ptr.add(12))),
                );
                vst1q_u8(
                    out.as_mut_ptr().add(x0 + x),
                    vcombine_u8(vqmovn_u16(p0), vqmovn_u16(p1)),
                );
            }
            x += 16;
        }
        for dx in vec_len..len {
            out[x0 + dx] = (acc.0[dx] >> SHIFT).min(255) as u8;
        }
        x0 += len;
    }
}

#[cfg(target_arch = "aarch64")]
fn blend_nv12_uv_row_neon(
    out_u: &mut [u8],
    out_v: &mut [u8],
    lines: &[(&[u8], usize, u32)],
    y: usize,
) {
    use std::arch::aarch64::{
        vcombine_u8, vcombine_u16, vdup_n_u16, vdupq_n_u16, vdupq_n_u32, vget_low_u8, vget_low_u16,
        vld2q_u8, vmlal_high_u16, vmlal_u16, vmovl_high_u8, vmovl_u8, vqmovn_u16, vqshrn_n_u32,
        vst1q_u8,
    };
    let width = out_u.len();
    let half = unsafe { vdupq_n_u32(1 << (SHIFT - 1)) };
    let mut x = 0;
    while x + 16 <= width {
        let (mut u0, mut u1, mut u2, mut u3) = (half, half, half, half);
        let (mut v0, mut v1, mut v2, mut v3) = (half, half, half, half);
        for &(data, stride, weight) in lines {
            unsafe {
                let w4 = vdup_n_u16(weight as u16);
                let w8 = vdupq_n_u16(weight as u16);
                let pair = vld2q_u8(data.as_ptr().add(y * stride + x * 2));
                let ulo = vmovl_u8(vget_low_u8(pair.0));
                let uhi = vmovl_high_u8(pair.0);
                u0 = vmlal_u16(u0, vget_low_u16(ulo), w4);
                u1 = vmlal_high_u16(u1, ulo, w8);
                u2 = vmlal_u16(u2, vget_low_u16(uhi), w4);
                u3 = vmlal_high_u16(u3, uhi, w8);
                let vlo = vmovl_u8(vget_low_u8(pair.1));
                let vhi = vmovl_high_u8(pair.1);
                v0 = vmlal_u16(v0, vget_low_u16(vlo), w4);
                v1 = vmlal_high_u16(v1, vlo, w8);
                v2 = vmlal_u16(v2, vget_low_u16(vhi), w4);
                v3 = vmlal_high_u16(v3, vhi, w8);
            }
        }
        unsafe {
            let pu0 = vcombine_u16(
                vqshrn_n_u32::<{ SHIFT as i32 }>(u0),
                vqshrn_n_u32::<{ SHIFT as i32 }>(u1),
            );
            let pu1 = vcombine_u16(
                vqshrn_n_u32::<{ SHIFT as i32 }>(u2),
                vqshrn_n_u32::<{ SHIFT as i32 }>(u3),
            );
            vst1q_u8(
                out_u.as_mut_ptr().add(x),
                vcombine_u8(vqmovn_u16(pu0), vqmovn_u16(pu1)),
            );
            let pv0 = vcombine_u16(
                vqshrn_n_u32::<{ SHIFT as i32 }>(v0),
                vqshrn_n_u32::<{ SHIFT as i32 }>(v1),
            );
            let pv1 = vcombine_u16(
                vqshrn_n_u32::<{ SHIFT as i32 }>(v2),
                vqshrn_n_u32::<{ SHIFT as i32 }>(v3),
            );
            vst1q_u8(
                out_v.as_mut_ptr().add(x),
                vcombine_u8(vqmovn_u16(pv0), vqmovn_u16(pv1)),
            );
        }
        x += 16;
    }
    for col in x..width {
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

#[cfg(target_arch = "aarch64")]
fn blend_nv12_uv_row_blocked_neon(
    out_u: &mut [u8],
    out_v: &mut [u8],
    lines: &[(&[u8], usize, u32)],
    y: usize,
) {
    use std::arch::aarch64::{
        vcombine_u8, vcombine_u16, vdup_n_u16, vdupq_n_u16, vget_low_u8, vget_low_u16, vld1q_u32,
        vld2q_u8, vmlal_high_u16, vmlal_u16, vmovl_high_u8, vmovl_u8, vqmovn_u16, vqshrn_n_u32,
        vst1q_u8, vst1q_u32,
    };
    let width = out_u.len();
    let mut acc_u = AccU32([0; TILE]);
    let mut acc_v = AccU32([0; TILE]);
    let mut x0 = 0;
    while x0 < width {
        let len = (width - x0).min(TILE);
        let vec_len = len & !15;
        acc_u.0[..len].fill(1 << (SHIFT - 1));
        acc_v.0[..len].fill(1 << (SHIFT - 1));
        for chunk in lines.chunks(BLOCK_FRAMES) {
            let mut x = 0;
            while x < vec_len {
                unsafe {
                    let pu = acc_u.0.as_mut_ptr().add(x);
                    let pv = acc_v.0.as_mut_ptr().add(x);
                    let mut u0 = vld1q_u32(pu);
                    let mut u1 = vld1q_u32(pu.add(4));
                    let mut u2 = vld1q_u32(pu.add(8));
                    let mut u3 = vld1q_u32(pu.add(12));
                    let mut v0 = vld1q_u32(pv);
                    let mut v1 = vld1q_u32(pv.add(4));
                    let mut v2 = vld1q_u32(pv.add(8));
                    let mut v3 = vld1q_u32(pv.add(12));
                    for &(data, stride, weight) in chunk {
                        let w4 = vdup_n_u16(weight as u16);
                        let w8 = vdupq_n_u16(weight as u16);
                        let pair = vld2q_u8(data.as_ptr().add(y * stride + (x0 + x) * 2));
                        let ulo = vmovl_u8(vget_low_u8(pair.0));
                        let uhi = vmovl_high_u8(pair.0);
                        u0 = vmlal_u16(u0, vget_low_u16(ulo), w4);
                        u1 = vmlal_high_u16(u1, ulo, w8);
                        u2 = vmlal_u16(u2, vget_low_u16(uhi), w4);
                        u3 = vmlal_high_u16(u3, uhi, w8);
                        let vlo = vmovl_u8(vget_low_u8(pair.1));
                        let vhi = vmovl_high_u8(pair.1);
                        v0 = vmlal_u16(v0, vget_low_u16(vlo), w4);
                        v1 = vmlal_high_u16(v1, vlo, w8);
                        v2 = vmlal_u16(v2, vget_low_u16(vhi), w4);
                        v3 = vmlal_high_u16(v3, vhi, w8);
                    }
                    vst1q_u32(pu, u0);
                    vst1q_u32(pu.add(4), u1);
                    vst1q_u32(pu.add(8), u2);
                    vst1q_u32(pu.add(12), u3);
                    vst1q_u32(pv, v0);
                    vst1q_u32(pv.add(4), v1);
                    vst1q_u32(pv.add(8), v2);
                    vst1q_u32(pv.add(12), v3);
                }
                x += 16;
            }
            for dx in vec_len..len {
                let col = x0 + dx;
                let mut su = acc_u.0[dx];
                let mut sv = acc_v.0[dx];
                for &(data, stride, weight) in chunk {
                    let at = y * stride + col * 2;
                    su += u32::from(data[at]) * weight;
                    sv += u32::from(data[at + 1]) * weight;
                }
                acc_u.0[dx] = su;
                acc_v.0[dx] = sv;
            }
        }
        let mut x = 0;
        while x < vec_len {
            unsafe {
                let pu = acc_u.0.as_ptr().add(x);
                let pv = acc_v.0.as_ptr().add(x);
                let pu0 = vcombine_u16(
                    vqshrn_n_u32::<{ SHIFT as i32 }>(vld1q_u32(pu)),
                    vqshrn_n_u32::<{ SHIFT as i32 }>(vld1q_u32(pu.add(4))),
                );
                let pu1 = vcombine_u16(
                    vqshrn_n_u32::<{ SHIFT as i32 }>(vld1q_u32(pu.add(8))),
                    vqshrn_n_u32::<{ SHIFT as i32 }>(vld1q_u32(pu.add(12))),
                );
                vst1q_u8(
                    out_u.as_mut_ptr().add(x0 + x),
                    vcombine_u8(vqmovn_u16(pu0), vqmovn_u16(pu1)),
                );
                let pv0 = vcombine_u16(
                    vqshrn_n_u32::<{ SHIFT as i32 }>(vld1q_u32(pv)),
                    vqshrn_n_u32::<{ SHIFT as i32 }>(vld1q_u32(pv.add(4))),
                );
                let pv1 = vcombine_u16(
                    vqshrn_n_u32::<{ SHIFT as i32 }>(vld1q_u32(pv.add(8))),
                    vqshrn_n_u32::<{ SHIFT as i32 }>(vld1q_u32(pv.add(12))),
                );
                vst1q_u8(
                    out_v.as_mut_ptr().add(x0 + x),
                    vcombine_u8(vqmovn_u16(pv0), vqmovn_u16(pv1)),
                );
            }
            x += 16;
        }
        for dx in vec_len..len {
            out_u[x0 + dx] = (acc_u.0[dx] >> SHIFT).min(255) as u8;
            out_v[x0 + dx] = (acc_v.0[dx] >> SHIFT).min(255) as u8;
        }
        x0 += len;
    }
}

#[cfg(target_arch = "aarch64")]
fn blend_gamma_row_neon(
    out: &mut [u8],
    lines: &[(&[u8], usize)],
    weights: &[f32],
    curve: &[f32; 256],
    inverse: &[u8],
    y: usize,
) {
    use std::arch::aarch64::{vdupq_n_f32, vfmaq_f32, vld1q_f32, vst1q_f32};
    let width = out.len();
    let mut acc = AccF32([0.0; TILE]);
    let mut x0 = 0;
    while x0 < width {
        let len = (width - x0).min(TILE);
        let vec_len = len & !3;
        acc.0[..len].fill(0.0);
        for (&(data, stride), &weight) in lines.iter().zip(weights) {
            let src = &data[y * stride + x0..][..len];
            let w = unsafe { vdupq_n_f32(weight) };
            let mut x = 0;
            while x < vec_len {
                unsafe {
                    let s = src.as_ptr().add(x);
                    let vals = [
                        *curve.get_unchecked(usize::from(*s)),
                        *curve.get_unchecked(usize::from(*s.add(1))),
                        *curve.get_unchecked(usize::from(*s.add(2))),
                        *curve.get_unchecked(usize::from(*s.add(3))),
                    ];
                    let ptr = acc.0.as_mut_ptr().add(x);
                    let v = vld1q_f32(vals.as_ptr());
                    let cur = vld1q_f32(ptr);
                    vst1q_f32(ptr, vfmaq_f32(cur, v, w));
                }
                x += 4;
            }
            for i in vec_len..len {
                acc.0[i] = curve[usize::from(src[i])].mul_add(weight, acc.0[i]);
            }
        }
        for i in 0..len {
            let index = acc.0[i].clamp(0.0, 1.0).to_bits() >> INEXACT;
            out[x0 + i] = inverse[index as usize];
        }
        x0 += len;
    }
}

fn blend_plane_row_scalar(out: &mut [u8], lines: &[(&[u8], usize, u32)], y: usize) {
    let width = out.len();
    let mut acc = AccU32([0; TILE]);
    let mut x0 = 0;
    while x0 < width {
        let len = (width - x0).min(TILE);
        acc.0[..len].fill(1 << (SHIFT - 1));
        for chunk in lines.chunks(BLOCK_FRAMES) {
            for &(data, stride, weight) in chunk {
                for (sum, &sample) in acc.0[..len].iter_mut().zip(&data[y * stride + x0..]) {
                    *sum += u32::from(sample) * weight;
                }
            }
        }
        for (sample, &sum) in out[x0..x0 + len].iter_mut().zip(&acc.0[..len]) {
            *sample = (sum >> SHIFT).min(255) as u8;
        }
        x0 += len;
    }
}

fn blend_nv12_uv_row_scalar(
    out_u: &mut [u8],
    out_v: &mut [u8],
    lines: &[(&[u8], usize, u32)],
    y: usize,
) {
    let width = out_u.len();
    let mut acc_u = AccU32([0; TILE]);
    let mut acc_v = AccU32([0; TILE]);
    let mut x0 = 0;
    while x0 < width {
        let len = (width - x0).min(TILE);
        acc_u.0[..len].fill(1 << (SHIFT - 1));
        acc_v.0[..len].fill(1 << (SHIFT - 1));
        for chunk in lines.chunks(BLOCK_FRAMES) {
            for &(data, stride, weight) in chunk {
                let (pairs, _) = data[y * stride + x0 * 2..][..len * 2].as_chunks::<2>();
                for ((u, v), pair) in acc_u.0[..len]
                    .iter_mut()
                    .zip(&mut acc_v.0[..len])
                    .zip(pairs)
                {
                    *u += u32::from(pair[0]) * weight;
                    *v += u32::from(pair[1]) * weight;
                }
            }
        }
        for ((u, &su), (v, &sv)) in out_u[x0..x0 + len]
            .iter_mut()
            .zip(&acc_u.0[..len])
            .zip(out_v[x0..x0 + len].iter_mut().zip(&acc_v.0[..len]))
        {
            *u = (su >> SHIFT).min(255) as u8;
            *v = (sv >> SHIFT).min(255) as u8;
        }
        x0 += len;
    }
}

fn blend_gamma_row_scalar(
    out: &mut [u8],
    lines: &[(&[u8], usize)],
    weights: &[f32],
    curve: &[f32; 256],
    inverse: &[u8],
    y: usize,
) {
    let width = out.len();
    let mut acc = AccF32([0.0; TILE]);
    let mut x0 = 0;
    while x0 < width {
        let len = (width - x0).min(TILE);
        acc.0[..len].fill(0.0);
        for (&(data, stride), &weight) in lines.iter().zip(weights) {
            for (sum, &sample) in acc.0[..len].iter_mut().zip(&data[y * stride + x0..]) {
                *sum = curve[usize::from(sample)].mul_add(weight, *sum);
            }
        }
        for (sample, &sum) in out[x0..x0 + len].iter_mut().zip(&acc.0[..len]) {
            let index = sum.clamp(0.0, 1.0).to_bits() >> INEXACT;
            *sample = inverse[index as usize];
        }
        x0 += len;
    }
}

struct MixedLine<'a> {
    u: &'a [u8],
    u_stride: usize,
    v: &'a [u8],
    v_stride: usize,
    weight: u32,
    nv12: bool,
}

fn blend_mixed_uv_row(out_u: &mut [u8], out_v: &mut [u8], lines: &[MixedLine<'_>], y: usize) {
    let width = out_u.len();
    let mut acc_u = AccU32([0; TILE]);
    let mut acc_v = AccU32([0; TILE]);
    let mut x0 = 0;
    while x0 < width {
        let len = (width - x0).min(TILE);
        acc_u.0[..len].fill(1 << (SHIFT - 1));
        acc_v.0[..len].fill(1 << (SHIFT - 1));
        for chunk in lines.chunks(BLOCK_FRAMES) {
            for line in chunk {
                if line.nv12 {
                    let (pairs, _) = line.u[y * line.u_stride + x0 * 2..][..len * 2].as_chunks::<2>();
                    for ((u, v), pair) in acc_u.0[..len]
                        .iter_mut()
                        .zip(&mut acc_v.0[..len])
                        .zip(pairs)
                    {
                        *u += u32::from(pair[0]) * line.weight;
                        *v += u32::from(pair[1]) * line.weight;
                    }
                } else {
                    for (u, &sample) in acc_u.0[..len]
                        .iter_mut()
                        .zip(&line.u[y * line.u_stride + x0..])
                    {
                        *u += u32::from(sample) * line.weight;
                    }
                    for (v, &sample) in acc_v.0[..len]
                        .iter_mut()
                        .zip(&line.v[y * line.v_stride + x0..])
                    {
                        *v += u32::from(sample) * line.weight;
                    }
                }
            }
        }
        for ((u, &su), (v, &sv)) in out_u[x0..x0 + len]
            .iter_mut()
            .zip(&acc_u.0[..len])
            .zip(out_v[x0..x0 + len].iter_mut().zip(&acc_v.0[..len]))
        {
            *u = (su >> SHIFT).min(255) as u8;
            *v = (sv >> SHIFT).min(255) as u8;
        }
        x0 += len;
    }
}

fn blend_plane_row_equal_scalar(out: &mut [u8], lines: &[(&[u8], usize, u32)], y: usize) {
    let n = lines.len() as u32;
    let half = n / 2;
    let recip = (1u32 << 24).div_ceil(n);
    for (col, sample) in out.iter_mut().enumerate() {
        let mut sum = half;
        for &(data, stride, _) in lines {
            sum += u32::from(data[y * stride + col]);
        }
        *sample = ((sum * recip) >> 24).min(255) as u8;
    }
}

fn blend_nv12_uv_row_equal_scalar(
    out_u: &mut [u8],
    out_v: &mut [u8],
    lines: &[(&[u8], usize, u32)],
    y: usize,
) {
    let n = lines.len() as u32;
    let half = n / 2;
    let recip = (1u32 << 24).div_ceil(n);
    for col in 0..out_u.len() {
        let mut su = half;
        let mut sv = half;
        for &(data, stride, _) in lines {
            let at = y * stride + col * 2;
            su += u32::from(data[at]);
            sv += u32::from(data[at + 1]);
        }
        out_u[col] = ((su * recip) >> 24).min(255) as u8;
        out_v[col] = ((sv * recip) >> 24).min(255) as u8;
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn resolve_u16_32_avx2(
    s0: std::arch::x86_64::__m256i,
    s1: std::arch::x86_64::__m256i,
    ok16: bool,
    vrecip16: std::arch::x86_64::__m256i,
    vrecip24: std::arch::x86_64::__m256i,
) -> std::arch::x86_64::__m256i {
    use std::arch::x86_64::{
        _mm256_castsi256_si128, _mm256_cvtepu16_epi32, _mm256_extracti128_si256,
        _mm256_mulhi_epu16, _mm256_mullo_epi32, _mm256_packus_epi16, _mm256_packus_epi32,
        _mm256_permute4x64_epi64, _mm256_srli_epi32,
    };
    let (r0, r1) = if ok16 {
        (
            _mm256_mulhi_epu16(s0, vrecip16),
            _mm256_mulhi_epu16(s1, vrecip16),
        )
    } else {
        let q0 = _mm256_srli_epi32::<24>(_mm256_mullo_epi32(
            _mm256_cvtepu16_epi32(_mm256_castsi256_si128(s0)),
            vrecip24,
        ));
        let q1 = _mm256_srli_epi32::<24>(_mm256_mullo_epi32(
            _mm256_cvtepu16_epi32(_mm256_extracti128_si256::<1>(s0)),
            vrecip24,
        ));
        let q2 = _mm256_srli_epi32::<24>(_mm256_mullo_epi32(
            _mm256_cvtepu16_epi32(_mm256_castsi256_si128(s1)),
            vrecip24,
        ));
        let q3 = _mm256_srli_epi32::<24>(_mm256_mullo_epi32(
            _mm256_cvtepu16_epi32(_mm256_extracti128_si256::<1>(s1)),
            vrecip24,
        ));
        (
            _mm256_permute4x64_epi64::<0xD8>(_mm256_packus_epi32(q0, q1)),
            _mm256_permute4x64_epi64::<0xD8>(_mm256_packus_epi32(q2, q3)),
        )
    };
    _mm256_permute4x64_epi64::<0xD8>(_mm256_packus_epi16(r0, r1))
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn blend_plane_row_equal_avx2(out: &mut [u8], lines: &[(&[u8], usize, u32)], y: usize) {
    use std::arch::x86_64::{
        _mm_loadu_si128, _mm256_add_epi16, _mm256_cvtepu8_epi16, _mm256_set1_epi16,
        _mm256_set1_epi32, _mm256_storeu_si256,
    };
    let width = out.len();
    let n = lines.len() as u32;
    let half = (n / 2) as u16;
    let ok16 = n <= 16;
    let recip16 = ((1u32 << 16).div_ceil(n)) as u16;
    let recip24 = (1u32 << 24).div_ceil(n);
    let vhalf = _mm256_set1_epi16(half as i16);
    let vrecip16 = _mm256_set1_epi16(recip16 as i16);
    let vrecip24 = _mm256_set1_epi32(recip24 as i32);
    let mut x = 0;
    while x + 32 <= width {
        let mut s0 = vhalf;
        let mut s1 = vhalf;
        for &(data, stride, _) in lines {
            unsafe {
                let p = data.as_ptr().add(y * stride + x);
                s0 = _mm256_add_epi16(s0, _mm256_cvtepu8_epi16(_mm_loadu_si128(p.cast())));
                s1 = _mm256_add_epi16(s1, _mm256_cvtepu8_epi16(_mm_loadu_si128(p.add(16).cast())));
            }
        }
        let packed = unsafe { resolve_u16_32_avx2(s0, s1, ok16, vrecip16, vrecip24) };
        unsafe { _mm256_storeu_si256(out.as_mut_ptr().add(x).cast(), packed) };
        x += 32;
    }
    for (dx, sample) in out[x..].iter_mut().enumerate() {
        let col = x + dx;
        let mut sum = u32::from(half);
        for &(data, stride, _) in lines {
            sum += u32::from(data[y * stride + col]);
        }
        *sample = ((sum * recip24) >> 24).min(255) as u8;
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn blend_plane_row_equal_blocked_avx2(
    out: &mut [u8],
    lines: &[(&[u8], usize, u32)],
    y: usize,
) {
    use std::arch::x86_64::{
        _mm_loadu_si128, _mm256_add_epi16, _mm256_cvtepu8_epi16, _mm256_load_si256,
        _mm256_set1_epi16, _mm256_set1_epi32, _mm256_store_si256, _mm256_storeu_si256,
    };
    let width = out.len();
    let n = lines.len() as u32;
    let half = (n / 2) as u16;
    let ok16 = n <= 16;
    let recip16 = ((1u32 << 16).div_ceil(n)) as u16;
    let recip24 = (1u32 << 24).div_ceil(n);
    let vrecip16 = _mm256_set1_epi16(recip16 as i16);
    let vrecip24 = _mm256_set1_epi32(recip24 as i32);
    let mut acc = AccU16([0; TILE]);
    let mut x0 = 0;
    while x0 < width {
        let len = (width - x0).min(TILE);
        let vec_len = len & !31;
        acc.0[..len].fill(half);
        for chunk in lines.chunks(BLOCK_FRAMES) {
            let mut x = 0;
            while x < vec_len {
                unsafe {
                    let ptr = acc.0.as_mut_ptr().add(x).cast();
                    let mut s0 = _mm256_load_si256(ptr);
                    let mut s1 = _mm256_load_si256(ptr.add(1));
                    for &(data, stride, _) in chunk {
                        let p = data.as_ptr().add(y * stride + x0 + x);
                        s0 = _mm256_add_epi16(s0, _mm256_cvtepu8_epi16(_mm_loadu_si128(p.cast())));
                        s1 = _mm256_add_epi16(
                            s1,
                            _mm256_cvtepu8_epi16(_mm_loadu_si128(p.add(16).cast())),
                        );
                    }
                    _mm256_store_si256(ptr, s0);
                    _mm256_store_si256(ptr.add(1), s1);
                }
                x += 32;
            }
            for dx in vec_len..len {
                let col = x0 + dx;
                let mut sum = acc.0[dx];
                for &(data, stride, _) in chunk {
                    sum += u16::from(data[y * stride + col]);
                }
                acc.0[dx] = sum;
            }
        }
        let mut x = 0;
        while x < vec_len {
            unsafe {
                let ptr = acc.0.as_ptr().add(x).cast();
                let packed = resolve_u16_32_avx2(
                    _mm256_load_si256(ptr),
                    _mm256_load_si256(ptr.add(1)),
                    ok16,
                    vrecip16,
                    vrecip24,
                );
                _mm256_storeu_si256(out.as_mut_ptr().add(x0 + x).cast(), packed);
            }
            x += 32;
        }
        for dx in vec_len..len {
            out[x0 + dx] = ((u32::from(acc.0[dx]) * recip24) >> 24).min(255) as u8;
        }
        x0 += len;
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn blend_nv12_uv_row_equal_avx2(
    out_u: &mut [u8],
    out_v: &mut [u8],
    lines: &[(&[u8], usize, u32)],
    y: usize,
) {
    use std::arch::x86_64::{
        _mm_loadu_si128, _mm_storeu_si128, _mm256_add_epi16, _mm256_castsi256_si128,
        _mm256_cvtepu8_epi16, _mm256_extracti128_si256, _mm256_load_si256,
        _mm256_permute4x64_epi64, _mm256_set1_epi16, _mm256_set1_epi32, _mm256_setr_epi8,
        _mm256_shuffle_epi8, _mm256_store_si256,
    };
    let width = out_u.len();
    let n = lines.len() as u32;
    let half = (n / 2) as u16;
    let ok16 = n <= 16;
    let recip16 = ((1u32 << 16).div_ceil(n)) as u16;
    let recip24 = (1u32 << 24).div_ceil(n);
    let vhalf = _mm256_set1_epi16(half as i16);
    let vrecip16 = _mm256_set1_epi16(recip16 as i16);
    let vrecip24 = _mm256_set1_epi32(recip24 as i32);
    let shuf = _mm256_setr_epi8(
        0, 2, 4, 6, 8, 10, 12, 14, 1, 3, 5, 7, 9, 11, 13, 15, 0, 2, 4, 6, 8, 10, 12, 14, 1, 3, 5,
        7, 9, 11, 13, 15,
    );
    if lines.len() >= BLOCK_WINDOW {
        let mut acc = AccU16([0; TILE]);
        let tile_pairs = TILE / 2;
        let mut x0 = 0;
        while x0 < width {
            let pairs = (width - x0).min(tile_pairs);
            let bytes = pairs * 2;
            let vec_bytes = bytes & !31;
            acc.0[..bytes].fill(half);
            for chunk in lines.chunks(BLOCK_FRAMES) {
                let mut b = 0;
                while b < vec_bytes {
                    unsafe {
                        let ptr = acc.0.as_mut_ptr().add(b).cast();
                        let mut s0 = _mm256_load_si256(ptr);
                        let mut s1 = _mm256_load_si256(ptr.add(1));
                        for &(data, stride, _) in chunk {
                            let p = data.as_ptr().add(y * stride + x0 * 2 + b);
                            s0 = _mm256_add_epi16(
                                s0,
                                _mm256_cvtepu8_epi16(_mm_loadu_si128(p.cast())),
                            );
                            s1 = _mm256_add_epi16(
                                s1,
                                _mm256_cvtepu8_epi16(_mm_loadu_si128(p.add(16).cast())),
                            );
                        }
                        _mm256_store_si256(ptr, s0);
                        _mm256_store_si256(ptr.add(1), s1);
                    }
                    b += 32;
                }
                for db in vec_bytes..bytes {
                    let mut sum = acc.0[db];
                    for &(data, stride, _) in chunk {
                        sum += u16::from(data[y * stride + x0 * 2 + db]);
                    }
                    acc.0[db] = sum;
                }
            }
            let mut b = 0;
            while b < vec_bytes {
                unsafe {
                    let ptr = acc.0.as_ptr().add(b).cast();
                    let packed = resolve_u16_32_avx2(
                        _mm256_load_si256(ptr),
                        _mm256_load_si256(ptr.add(1)),
                        ok16,
                        vrecip16,
                        vrecip24,
                    );
                    let ord = _mm256_permute4x64_epi64::<0xD8>(_mm256_shuffle_epi8(packed, shuf));
                    let col = x0 + b / 2;
                    _mm_storeu_si128(
                        out_u.as_mut_ptr().add(col).cast(),
                        _mm256_castsi256_si128(ord),
                    );
                    _mm_storeu_si128(
                        out_v.as_mut_ptr().add(col).cast(),
                        _mm256_extracti128_si256::<1>(ord),
                    );
                }
                b += 32;
            }
            for col in x0 + vec_bytes / 2..x0 + pairs {
                let db = (col - x0) * 2;
                out_u[col] = ((u32::from(acc.0[db]) * recip24) >> 24).min(255) as u8;
                out_v[col] = ((u32::from(acc.0[db + 1]) * recip24) >> 24).min(255) as u8;
            }
            x0 += pairs;
        }
        return;
    }
    let mut x = 0;
    while x + 16 <= width {
        let mut s0 = vhalf;
        let mut s1 = vhalf;
        for &(data, stride, _) in lines {
            unsafe {
                let p = data.as_ptr().add(y * stride + x * 2);
                s0 = _mm256_add_epi16(s0, _mm256_cvtepu8_epi16(_mm_loadu_si128(p.cast())));
                s1 = _mm256_add_epi16(s1, _mm256_cvtepu8_epi16(_mm_loadu_si128(p.add(16).cast())));
            }
        }
        unsafe {
            let packed = resolve_u16_32_avx2(s0, s1, ok16, vrecip16, vrecip24);
            let ord = _mm256_permute4x64_epi64::<0xD8>(_mm256_shuffle_epi8(packed, shuf));
            _mm_storeu_si128(out_u.as_mut_ptr().add(x).cast(), _mm256_castsi256_si128(ord));
            _mm_storeu_si128(
                out_v.as_mut_ptr().add(x).cast(),
                _mm256_extracti128_si256::<1>(ord),
            );
        }
        x += 16;
    }
    for col in x..width {
        let mut su = u32::from(half);
        let mut sv = u32::from(half);
        for &(data, stride, _) in lines {
            let at = y * stride + col * 2;
            su += u32::from(data[at]);
            sv += u32::from(data[at + 1]);
        }
        out_u[col] = ((su * recip24) >> 24).min(255) as u8;
        out_v[col] = ((sv * recip24) >> 24).min(255) as u8;
    }
}

#[cfg(target_arch = "aarch64")]
fn resolve_u16_16_neon(
    s0: std::arch::aarch64::uint16x8_t,
    s1: std::arch::aarch64::uint16x8_t,
    vrecip: std::arch::aarch64::uint32x4_t,
) -> std::arch::aarch64::uint8x16_t {
    use std::arch::aarch64::{
        vcombine_u8, vcombine_u16, vget_low_u16, vmovl_high_u16, vmovl_u16, vmulq_u32,
        vqshrn_n_u16, vshrn_n_u32,
    };
    unsafe {
        let q0 = vshrn_n_u32::<16>(vmulq_u32(vmovl_u16(vget_low_u16(s0)), vrecip));
        let q1 = vshrn_n_u32::<16>(vmulq_u32(vmovl_high_u16(s0), vrecip));
        let q2 = vshrn_n_u32::<16>(vmulq_u32(vmovl_u16(vget_low_u16(s1)), vrecip));
        let q3 = vshrn_n_u32::<16>(vmulq_u32(vmovl_high_u16(s1), vrecip));
        vcombine_u8(
            vqshrn_n_u16::<8>(vcombine_u16(q0, q1)),
            vqshrn_n_u16::<8>(vcombine_u16(q2, q3)),
        )
    }
}

#[cfg(target_arch = "aarch64")]
fn blend_plane_row_equal_neon(out: &mut [u8], lines: &[(&[u8], usize, u32)], y: usize) {
    use std::arch::aarch64::{
        vaddw_high_u8, vaddw_u8, vdupq_n_u16, vdupq_n_u32, vget_low_u8, vld1q_u8, vst1q_u8,
    };
    let width = out.len();
    let n = lines.len() as u32;
    let half = (n / 2) as u16;
    let recip = (1u32 << 24).div_ceil(n);
    let vhalf = unsafe { vdupq_n_u16(half) };
    let vrecip = unsafe { vdupq_n_u32(recip) };
    let mut x = 0;
    while x + 16 <= width {
        let mut s0 = vhalf;
        let mut s1 = vhalf;
        for &(data, stride, _) in lines {
            unsafe {
                let px = vld1q_u8(data.as_ptr().add(y * stride + x));
                s0 = vaddw_u8(s0, vget_low_u8(px));
                s1 = vaddw_high_u8(s1, px);
            }
        }
        unsafe { vst1q_u8(out.as_mut_ptr().add(x), resolve_u16_16_neon(s0, s1, vrecip)) };
        x += 16;
    }
    for (dx, sample) in out[x..].iter_mut().enumerate() {
        let col = x + dx;
        let mut sum = u32::from(half);
        for &(data, stride, _) in lines {
            sum += u32::from(data[y * stride + col]);
        }
        *sample = ((sum * recip) >> 24).min(255) as u8;
    }
}

#[cfg(target_arch = "aarch64")]
fn blend_nv12_uv_row_equal_neon(
    out_u: &mut [u8],
    out_v: &mut [u8],
    lines: &[(&[u8], usize, u32)],
    y: usize,
) {
    use std::arch::aarch64::{
        vaddw_high_u8, vaddw_u8, vdupq_n_u16, vdupq_n_u32, vget_low_u8, vld1q_u8, vst1q_u8,
        vuzp1q_u8, vuzp2q_u8,
    };
    let width = out_u.len();
    let n = lines.len() as u32;
    let half = (n / 2) as u16;
    let recip = (1u32 << 24).div_ceil(n);
    let vhalf = unsafe { vdupq_n_u16(half) };
    let vrecip = unsafe { vdupq_n_u32(recip) };
    let mut x = 0;
    while x + 16 <= width {
        let (mut a0, mut a1, mut b0, mut b1) = (vhalf, vhalf, vhalf, vhalf);
        for &(data, stride, _) in lines {
            unsafe {
                let p = data.as_ptr().add(y * stride + x * 2);
                let p0 = vld1q_u8(p);
                let p1 = vld1q_u8(p.add(16));
                a0 = vaddw_u8(a0, vget_low_u8(p0));
                a1 = vaddw_high_u8(a1, p0);
                b0 = vaddw_u8(b0, vget_low_u8(p1));
                b1 = vaddw_high_u8(b1, p1);
            }
        }
        unsafe {
            let r0 = resolve_u16_16_neon(a0, a1, vrecip);
            let r1 = resolve_u16_16_neon(b0, b1, vrecip);
            vst1q_u8(out_u.as_mut_ptr().add(x), vuzp1q_u8(r0, r1));
            vst1q_u8(out_v.as_mut_ptr().add(x), vuzp2q_u8(r0, r1));
        }
        x += 16;
    }
    for col in x..width {
        let mut su = u32::from(half);
        let mut sv = u32::from(half);
        for &(data, stride, _) in lines {
            let at = y * stride + col * 2;
            su += u32::from(data[at]);
            sv += u32::from(data[at + 1]);
        }
        out_u[col] = ((su * recip) >> 24).min(255) as u8;
        out_v[col] = ((sv * recip) >> 24).min(255) as u8;
    }
}

fn blend_plane_row(
    out: &mut [u8],
    lines: &[(&[u8], usize, u32)],
    y: usize,
    equal: bool,
    #[cfg(target_arch = "x86_64")] simd: bool,
) {
    let blocked = lines.len() >= BLOCK_WINDOW;
    if equal {
        #[cfg(target_arch = "x86_64")]
        if simd {
            unsafe {
                if blocked {
                    blend_plane_row_equal_blocked_avx2(out, lines, y);
                } else {
                    blend_plane_row_equal_avx2(out, lines, y);
                }
            }
            return;
        }
        #[cfg(target_arch = "aarch64")]
        {
            blend_plane_row_equal_neon(out, lines, y);
            return;
        }
        #[allow(unreachable_code)]
        {
            blend_plane_row_equal_scalar(out, lines, y);
            return;
        }
    }
    #[cfg(target_arch = "x86_64")]
    if simd {
        unsafe {
            if blocked {
                blend_plane_row_blocked_avx2(out, lines, y);
            } else {
                blend_plane_row_avx2(out, lines, y);
            }
        }
        return;
    }
    #[cfg(target_arch = "aarch64")]
    {
        if blocked {
            blend_plane_row_blocked_neon(out, lines, y);
        } else {
            blend_plane_row_neon(out, lines, y);
        }
        return;
    }
    #[allow(unreachable_code)]
    {
        let _ = blocked;
        blend_plane_row_scalar(out, lines, y);
    }
}

fn blend_nv12_uv_row(
    out_u: &mut [u8],
    out_v: &mut [u8],
    lines: &[(&[u8], usize, u32)],
    y: usize,
    equal: bool,
    #[cfg(target_arch = "x86_64")] simd: bool,
) {
    let blocked = lines.len() >= BLOCK_WINDOW;
    if equal {
        #[cfg(target_arch = "x86_64")]
        if simd {
            unsafe { blend_nv12_uv_row_equal_avx2(out_u, out_v, lines, y) };
            return;
        }
        #[cfg(target_arch = "aarch64")]
        {
            blend_nv12_uv_row_equal_neon(out_u, out_v, lines, y);
            return;
        }
        #[allow(unreachable_code)]
        {
            blend_nv12_uv_row_equal_scalar(out_u, out_v, lines, y);
            return;
        }
    }
    #[cfg(target_arch = "x86_64")]
    if simd {
        unsafe {
            if blocked {
                blend_nv12_uv_row_blocked_avx2(out_u, out_v, lines, y);
            } else {
                blend_nv12_uv_row_avx2(out_u, out_v, lines, y);
            }
        }
        return;
    }
    #[cfg(target_arch = "aarch64")]
    {
        if blocked {
            blend_nv12_uv_row_blocked_neon(out_u, out_v, lines, y);
        } else {
            blend_nv12_uv_row_neon(out_u, out_v, lines, y);
        }
        return;
    }
    #[allow(unreachable_code)]
    {
        let _ = blocked;
        blend_nv12_uv_row_scalar(out_u, out_v, lines, y);
    }
}

fn blend_gamma_row(
    out: &mut [u8],
    lines: &[(&[u8], usize)],
    weights: &[f32],
    curve: &[f32; 256],
    inverse: &[u8],
    y: usize,
    #[cfg(target_arch = "x86_64")] fma: bool,
) {
    #[cfg(target_arch = "x86_64")]
    if fma {
        unsafe { blend_gamma_row_avx2(out, lines, weights, curve, inverse, y) };
        return;
    }
    #[cfg(target_arch = "aarch64")]
    {
        blend_gamma_row_neon(out, lines, weights, curve, inverse, y);
        return;
    }
    #[allow(unreachable_code)]
    blend_gamma_row_scalar(out, lines, weights, curve, inverse, y);
}

enum Chroma<'a> {
    Planar(Vec<(&'a [u8], usize, u32)>, Vec<(&'a [u8], usize, u32)>),
    Nv12(Vec<(&'a [u8], usize, u32)>),
    Mixed(Vec<MixedLine<'a>>),
}

pub struct Blend {
    weights: Vec<u32>,
    equal: bool,
    exact: Vec<f32>,
    curve: Option<[f32; 256]>,
    inverse: Vec<u8>,
    step: (i64, i64),
    last: i64,
    held: Mutex<Vec<(i64, frame::Video)>>,
    pool: Mutex<VecDeque<frame::Video>>,
    pub frames: i32,
}

impl Blend {
    pub fn new(weights: &[f64], from: (i64, i64), blending: &Blending, frames: i32) -> Self {
        let (scale, sum) = (f64::from(1 << SHIFT), weights.iter().sum::<f64>());
        let equal = weights.len() >= 2
            && weights.len() <= 255
            && weights.windows(2).all(|w| (w[0] - w[1]).abs() <= 1e-12 * sum);
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
            equal,
            exact: weights.iter().map(|weight| (weight / sum) as f32).collect(),
            curve: (gamma != 1.0).then(|| std::array::from_fn(curve)),
            inverse,
            step,
            last: i64::from(frames - 1),
            held: Mutex::new(Vec::new()),
            pool: Mutex::new(VecDeque::new()),
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
        let output = {
            let mut pool = self
                .pool
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            let found = pool.iter().position(|frame| {
                frame.format() == format
                    && frame.width() == width
                    && frame.height() == height
                    && unsafe { ffi::av_frame_is_writable(frame.as_ptr().cast_mut()) != 0 }
            });
            match found.and_then(|index| pool.remove(index)) {
                Some(frame) => frame,
                None => frame::Video::new(format, width, height),
            }
        };
        #[cfg(target_arch = "x86_64")]
        let simd = std::arch::is_x86_feature_detected!("avx2")
            && self.weights.len() >= 2
            && self.weights.iter().all(|&w| w <= 32767);
        #[cfg(target_arch = "x86_64")]
        let fma =
            std::arch::is_x86_feature_detected!("avx2") && std::arch::is_x86_feature_detected!("fma");
        let all_nv12 = !yuv444 && held.iter().all(|(_, p)| p.format() == Pixel::NV12);
        let any_nv12 = !yuv444 && held.iter().any(|(_, p)| p.format() == Pixel::NV12);

        let (w, h) = (width as usize, height as usize);
        let shift = usize::from(!yuv444);
        let (cw, ch) = (w >> shift, h >> shift);
        let y_raw: Vec<(&[u8], usize)> = held
            .iter()
            .map(|(_, picture)| (picture.data(0), picture.stride(0)))
            .collect();
        let y_lines: Vec<(&[u8], usize, u32)> = held
            .iter()
            .zip(&self.weights)
            .map(|((_, picture), &weight)| (picture.data(0), picture.stride(0), weight))
            .collect();
        let chroma = if all_nv12 {
            Chroma::Nv12(
                held.iter()
                    .zip(&self.weights)
                    .map(|((_, picture), &weight)| (picture.data(1), picture.stride(1), weight))
                    .collect(),
            )
        } else if any_nv12 {
            Chroma::Mixed(
                held.iter()
                    .zip(&self.weights)
                    .map(|((_, picture), &weight)| {
                        let nv12 = picture.format() == Pixel::NV12;
                        if nv12 {
                            MixedLine {
                                u: picture.data(1),
                                u_stride: picture.stride(1),
                                v: &[],
                                v_stride: 0,
                                weight,
                                nv12: true,
                            }
                        } else {
                            MixedLine {
                                u: picture.data(1),
                                u_stride: picture.stride(1),
                                v: picture.data(2),
                                v_stride: picture.stride(2),
                                weight,
                                nv12: false,
                            }
                        }
                    })
                    .collect(),
            )
        } else {
            let u = held
                .iter()
                .zip(&self.weights)
                .map(|((_, picture), &weight)| (picture.data(1), picture.stride(1), weight))
                .collect();
            let v = held
                .iter()
                .zip(&self.weights)
                .map(|((_, picture), &weight)| (picture.data(2), picture.stride(2), weight))
                .collect();
            Chroma::Planar(u, v)
        };

        let (y_stride, u_stride, v_stride) = (output.stride(0), output.stride(1), output.stride(2));
        let raw = unsafe { &*output.as_ptr() };
        let (y_out, u_out, v_out) = unsafe {
            (
                std::slice::from_raw_parts_mut(raw.data[0], y_stride * h),
                std::slice::from_raw_parts_mut(raw.data[1], u_stride * ch),
                std::slice::from_raw_parts_mut(raw.data[2], v_stride * ch),
            )
        };
        let equal = self.equal;
        let blend_y = |row: &mut [u8], y: usize| {
            if let Some(curve) = &self.curve {
                blend_gamma_row(
                    row,
                    &y_raw,
                    &self.exact,
                    curve,
                    &self.inverse,
                    y,
                    #[cfg(target_arch = "x86_64")]
                    fma,
                );
            } else {
                blend_plane_row(
                    row,
                    &y_lines,
                    y,
                    equal,
                    #[cfg(target_arch = "x86_64")]
                    simd,
                );
            }
        };
        let blend_uv = |u_row: &mut [u8], v_row: &mut [u8], cy: usize| match &chroma {
            Chroma::Nv12(lines) => blend_nv12_uv_row(
                u_row,
                v_row,
                lines,
                cy,
                equal,
                #[cfg(target_arch = "x86_64")]
                simd,
            ),
            Chroma::Mixed(lines) => blend_mixed_uv_row(u_row, v_row, lines, cy),
            Chroma::Planar(u_lines, v_lines) => {
                blend_plane_row(
                    u_row,
                    u_lines,
                    cy,
                    equal,
                    #[cfg(target_arch = "x86_64")]
                    simd,
                );
                blend_plane_row(
                    v_row,
                    v_lines,
                    cy,
                    equal,
                    #[cfg(target_arch = "x86_64")]
                    simd,
                );
            }
        };

        if yuv444 {
            let piece = budget::piece(h);
            budget::install(|| {
                y_out
                    .par_chunks_mut(y_stride)
                    .take(h)
                    .zip(u_out.par_chunks_mut(u_stride).take(h))
                    .zip(v_out.par_chunks_mut(v_stride).take(h))
                    .enumerate()
                    .with_min_len(piece)
                    .for_each(|(y, ((y_row, u_row), v_row))| {
                        blend_y(&mut y_row[..w], y);
                        blend_uv(&mut u_row[..cw], &mut v_row[..cw], y);
                    });
            });
        } else {
            let piece = budget::piece(ch);
            budget::install(|| {
                y_out
                    .par_chunks_mut(y_stride * 2)
                    .take(ch)
                    .zip(u_out.par_chunks_mut(u_stride).take(ch))
                    .zip(v_out.par_chunks_mut(v_stride).take(ch))
                    .enumerate()
                    .with_min_len(piece)
                    .for_each(|(cy, ((y_pair, u_row), v_row))| {
                        let (y0, y1) = y_pair.split_at_mut(y_stride);
                        blend_y(&mut y0[..w], cy * 2);
                        blend_y(&mut y1[..w], cy * 2 + 1);
                        blend_uv(&mut u_row[..cw], &mut v_row[..cw], cy);
                    });
            });
            if h % 2 == 1 {
                blend_y(&mut y_out[(h - 1) * y_stride..][..w], h - 1);
            }
        }
        {
            let mut pool = self
                .pool
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            if pool.len() >= 4 {
                pool.pop_front();
            }
            pool.push_back(share(&output));
        }
        Ok(output)
    }
}
