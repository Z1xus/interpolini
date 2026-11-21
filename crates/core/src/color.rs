use std::path::Path;

use ffmpeg_next::format::Pixel;
use ffmpeg_next::frame;
use rayon::prelude::*;

use crate::budget;
use crate::decode::{self, Meta};
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

#[repr(C, align(16))]
#[derive(Clone, Copy)]
struct Entry([f32; 4]);

#[inline(always)]
fn blend4(entries: [&Entry; 4], weights: [f32; 4]) -> [f32; 4] {
    #[cfg(target_arch = "x86_64")]
    {
        use std::arch::x86_64::{_mm_add_ps, _mm_load_ps, _mm_mul_ps, _mm_set1_ps, _mm_storeu_ps};
        let mut out = [0.0f32; 4];
        unsafe {
            let v0 = _mm_mul_ps(_mm_load_ps(entries[0].0.as_ptr()), _mm_set1_ps(weights[0]));
            let va = _mm_mul_ps(_mm_load_ps(entries[1].0.as_ptr()), _mm_set1_ps(weights[1]));
            let vb = _mm_mul_ps(_mm_load_ps(entries[2].0.as_ptr()), _mm_set1_ps(weights[2]));
            let v1 = _mm_mul_ps(_mm_load_ps(entries[3].0.as_ptr()), _mm_set1_ps(weights[3]));
            let sum = _mm_add_ps(_mm_add_ps(v0, va), _mm_add_ps(vb, v1));
            _mm_storeu_ps(out.as_mut_ptr(), sum);
        }
        out
    }
    #[cfg(target_arch = "aarch64")]
    {
        use std::arch::aarch64::{vfmaq_n_f32, vld1q_f32, vmulq_n_f32, vst1q_f32};
        let mut out = [0.0f32; 4];
        unsafe {
            let mut acc = vmulq_n_f32(vld1q_f32(entries[0].0.as_ptr()), weights[0]);
            acc = vfmaq_n_f32(acc, vld1q_f32(entries[1].0.as_ptr()), weights[1]);
            acc = vfmaq_n_f32(acc, vld1q_f32(entries[2].0.as_ptr()), weights[2]);
            acc = vfmaq_n_f32(acc, vld1q_f32(entries[3].0.as_ptr()), weights[3]);
            vst1q_f32(out.as_mut_ptr(), acc);
        }
        out
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        [
            weights[0] * entries[0].0[0]
                + weights[1] * entries[1].0[0]
                + weights[2] * entries[2].0[0]
                + weights[3] * entries[3].0[0],
            weights[0] * entries[0].0[1]
                + weights[1] * entries[1].0[1]
                + weights[2] * entries[2].0[1]
                + weights[3] * entries[3].0[1],
            weights[0] * entries[0].0[2]
                + weights[1] * entries[1].0[2]
                + weights[2] * entries[2].0[2]
                + weights[3] * entries[3].0[2],
            0.0,
        ]
    }
}

struct BakedLut {
    size: usize,
    data: Vec<Entry>,
    coord: [(usize, f32); 256],
}

impl BakedLut {
    fn new(lut: &Lut, color: &config::Color, matrix: &Matrix) -> Self {
        let size = lut.size().clamp(33, 49);
        let (sin, cos) = color.hue.to_radians().sin_cos();
        let inv = 255.0 / (size - 1) as f32;
        let mut data = vec![Entry([0.0; 4]); size * size * size];
        let plane_len = size * size;
        budget::install(|| {
            data.par_chunks_mut(plane_len)
                .enumerate()
                .for_each(|(iv, slice)| {
                    let cr = iv as f32 * inv - 128.0;
                    for iu in 0..size {
                        let cb = iu as f32 * inv - 128.0;
                        let cb1 = (cb * cos - cr * sin) * color.saturation;
                        let cr1 = (cb * sin + cr * cos) * color.saturation;
                        let row = &mut slice[iu * size..(iu + 1) * size];
                        for (iy, slot) in row.iter_mut().enumerate() {
                            let yf = iy as f32 * inv;
                            let y1 = (((yf - 126.0) * color.contrast + 126.0) * color.brightness)
                                .clamp(0.0, 255.0);
                            let [yo, cbo, cro] = matrix.yuv(lut.sample(matrix.rgb(y1, cb1, cr1)));
                            *slot = Entry([yo, cbo + 128.0, cro + 128.0, 0.0]);
                        }
                    }
                });
        });
        let scale = (size - 1) as f32 / 255.0;
        let coord = std::array::from_fn(|i| {
            let pos = i as f32 * scale;
            let idx = (pos as usize).min(size - 2);
            (idx, pos - idx as f32)
        });
        Self { size, data, coord }
    }

    #[inline(always)]
    fn sample_uv(&self, y: u8, du: f32, dv: f32, base_uv: usize) -> [f32; 4] {
        let (iy, dy) = self.coord[usize::from(y)];
        let su = self.size;
        let sv = su * su;
        let base = base_uv + iy;
        let (w0, wa, wb, w1, ia, ib) = if dy >= du {
            if du >= dv {
                (1.0 - dy, dy - du, du - dv, dv, 1, su + 1)
            } else if dy >= dv {
                (1.0 - dy, dy - dv, dv - du, du, 1, sv + 1)
            } else {
                (1.0 - dv, dv - dy, dy - du, du, sv, sv + 1)
            }
        } else if dv >= du {
            (1.0 - dv, dv - du, du - dy, dy, sv, sv + su)
        } else if dv >= dy {
            (1.0 - du, du - dv, dv - dy, dy, su, sv + su)
        } else {
            (1.0 - du, du - dy, dy - dv, dv, su, su + 1)
        };
        unsafe {
            blend4(
                [
                    self.data.get_unchecked(base),
                    self.data.get_unchecked(base + ia),
                    self.data.get_unchecked(base + ib),
                    self.data.get_unchecked(base + sv + su + 1),
                ],
                [w0, wa, wb, w1],
            )
        }
    }

    #[inline(always)]
    fn sample(&self, y: u8, u: u8, v: u8) -> [f32; 4] {
        let (iu, du) = self.coord[usize::from(u)];
        let (iv, dv) = self.coord[usize::from(v)];
        let base_uv = (iv * self.size + iu) * self.size;
        self.sample_uv(y, du, dv, base_uv)
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn turn_chroma_row_avx2(
    u_in: &[u8],
    v_in: &[u8],
    u_out: &mut [u8],
    v_out: &mut [u8],
    cos: i32,
    sin: i32,
) {
    use std::arch::x86_64::{
        _mm_loadl_epi64, _mm_storeu_si128, _mm256_add_epi32, _mm256_castsi256_si128,
        _mm256_cvtepu8_epi32, _mm256_max_epi32, _mm256_min_epi32, _mm256_mullo_epi32,
        _mm256_packus_epi16, _mm256_packus_epi32, _mm256_permute4x64_epi64, _mm256_set1_epi32,
        _mm256_setzero_si256, _mm256_srai_epi32, _mm256_sub_epi32,
    };
    let len = u_in.len();
    let c128 = _mm256_set1_epi32(128);
    let c255 = _mm256_set1_epi32(255);
    let zero = _mm256_setzero_si256();
    let vcos = _mm256_set1_epi32(cos);
    let vsin = _mm256_set1_epi32(sin);
    let mut x = 0;
    while x + 16 <= len {
        unsafe {
            let cb0 = _mm256_sub_epi32(
                _mm256_cvtepu8_epi32(_mm_loadl_epi64(u_in.as_ptr().add(x).cast())),
                c128,
            );
            let cr0 = _mm256_sub_epi32(
                _mm256_cvtepu8_epi32(_mm_loadl_epi64(v_in.as_ptr().add(x).cast())),
                c128,
            );
            let cb1 = _mm256_sub_epi32(
                _mm256_cvtepu8_epi32(_mm_loadl_epi64(u_in.as_ptr().add(x + 8).cast())),
                c128,
            );
            let cr1 = _mm256_sub_epi32(
                _mm256_cvtepu8_epi32(_mm_loadl_epi64(v_in.as_ptr().add(x + 8).cast())),
                c128,
            );
            let nu0 = _mm256_min_epi32(
                _mm256_max_epi32(
                    _mm256_add_epi32(
                        _mm256_srai_epi32::<16>(_mm256_sub_epi32(
                            _mm256_mullo_epi32(cb0, vcos),
                            _mm256_mullo_epi32(cr0, vsin),
                        )),
                        c128,
                    ),
                    zero,
                ),
                c255,
            );
            let nu1 = _mm256_min_epi32(
                _mm256_max_epi32(
                    _mm256_add_epi32(
                        _mm256_srai_epi32::<16>(_mm256_sub_epi32(
                            _mm256_mullo_epi32(cb1, vcos),
                            _mm256_mullo_epi32(cr1, vsin),
                        )),
                        c128,
                    ),
                    zero,
                ),
                c255,
            );
            let nv0 = _mm256_min_epi32(
                _mm256_max_epi32(
                    _mm256_add_epi32(
                        _mm256_srai_epi32::<16>(_mm256_add_epi32(
                            _mm256_mullo_epi32(cb0, vsin),
                            _mm256_mullo_epi32(cr0, vcos),
                        )),
                        c128,
                    ),
                    zero,
                ),
                c255,
            );
            let nv1 = _mm256_min_epi32(
                _mm256_max_epi32(
                    _mm256_add_epi32(
                        _mm256_srai_epi32::<16>(_mm256_add_epi32(
                            _mm256_mullo_epi32(cb1, vsin),
                            _mm256_mullo_epi32(cr1, vcos),
                        )),
                        c128,
                    ),
                    zero,
                ),
                c255,
            );
            let u16 = _mm256_permute4x64_epi64::<0xD8>(_mm256_packus_epi32(nu0, nu1));
            let u8 = _mm256_permute4x64_epi64::<0xD8>(_mm256_packus_epi16(u16, u16));
            let v16 = _mm256_permute4x64_epi64::<0xD8>(_mm256_packus_epi32(nv0, nv1));
            let v8 = _mm256_permute4x64_epi64::<0xD8>(_mm256_packus_epi16(v16, v16));
            _mm_storeu_si128(
                u_out.as_mut_ptr().add(x).cast(),
                _mm256_castsi256_si128(u8),
            );
            _mm_storeu_si128(
                v_out.as_mut_ptr().add(x).cast(),
                _mm256_castsi256_si128(v8),
            );
        }
        x += 16;
    }
    for i in x..len {
        let cb = i32::from(u_in[i]) - 128;
        let cr = i32::from(v_in[i]) - 128;
        u_out[i] = (((cb * cos - cr * sin) >> 16) + 128).clamp(0, 255) as u8;
        v_out[i] = (((cb * sin + cr * cos) >> 16) + 128).clamp(0, 255) as u8;
    }
}

#[cfg(target_arch = "aarch64")]
fn turn_chroma_row_neon(
    u_in: &[u8],
    v_in: &[u8],
    u_out: &mut [u8],
    v_out: &mut [u8],
    cos: i32,
    sin: i32,
) {
    use std::arch::aarch64::{
        vaddq_s32, vcombine_u16, vdupq_n_s32, vget_low_u16, vld1_u8, vmovl_high_u16, vmovl_u8,
        vmovl_u16, vmulq_s32, vqmovn_u16, vqmovun_s32, vreinterpretq_s32_u32, vshrq_n_s32,
        vst1_u8, vsubq_s32,
    };
    let len = u_in.len();
    let (c128, vcos, vsin) = unsafe { (vdupq_n_s32(128), vdupq_n_s32(cos), vdupq_n_s32(sin)) };
    let mut x = 0;
    while x + 8 <= len {
        unsafe {
            let pu = vmovl_u8(vld1_u8(u_in.as_ptr().add(x)));
            let pv = vmovl_u8(vld1_u8(v_in.as_ptr().add(x)));
            let cb0 = vsubq_s32(vreinterpretq_s32_u32(vmovl_u16(vget_low_u16(pu))), c128);
            let cb1 = vsubq_s32(vreinterpretq_s32_u32(vmovl_high_u16(pu)), c128);
            let cr0 = vsubq_s32(vreinterpretq_s32_u32(vmovl_u16(vget_low_u16(pv))), c128);
            let cr1 = vsubq_s32(vreinterpretq_s32_u32(vmovl_high_u16(pv)), c128);
            let nu0 = vaddq_s32(
                vshrq_n_s32::<16>(vsubq_s32(vmulq_s32(cb0, vcos), vmulq_s32(cr0, vsin))),
                c128,
            );
            let nu1 = vaddq_s32(
                vshrq_n_s32::<16>(vsubq_s32(vmulq_s32(cb1, vcos), vmulq_s32(cr1, vsin))),
                c128,
            );
            let nv0 = vaddq_s32(
                vshrq_n_s32::<16>(vaddq_s32(vmulq_s32(cb0, vsin), vmulq_s32(cr0, vcos))),
                c128,
            );
            let nv1 = vaddq_s32(
                vshrq_n_s32::<16>(vaddq_s32(vmulq_s32(cb1, vsin), vmulq_s32(cr1, vcos))),
                c128,
            );
            vst1_u8(
                u_out.as_mut_ptr().add(x),
                vqmovn_u16(vcombine_u16(vqmovun_s32(nu0), vqmovun_s32(nu1))),
            );
            vst1_u8(
                v_out.as_mut_ptr().add(x),
                vqmovn_u16(vcombine_u16(vqmovun_s32(nv0), vqmovun_s32(nv1))),
            );
        }
        x += 8;
    }
    for i in x..len {
        let cb = i32::from(u_in[i]) - 128;
        let cr = i32::from(v_in[i]) - 128;
        u_out[i] = (((cb * cos - cr * sin) >> 16) + 128).clamp(0, 255) as u8;
        v_out[i] = (((cb * sin + cr * cos) >> 16) + 128).clamp(0, 255) as u8;
    }
}

fn turn_chroma_row(u_in: &[u8], v_in: &[u8], u_out: &mut [u8], v_out: &mut [u8], cos: i32, sin: i32) {
    if cos == FIXED && sin == 0 {
        u_out.copy_from_slice(u_in);
        v_out.copy_from_slice(v_in);
        return;
    }
    #[cfg(target_arch = "x86_64")]
    if std::arch::is_x86_feature_detected!("avx2") {
        unsafe { turn_chroma_row_avx2(u_in, v_in, u_out, v_out, cos, sin) };
        return;
    }
    #[cfg(target_arch = "aarch64")]
    {
        turn_chroma_row_neon(u_in, v_in, u_out, v_out, cos, sin);
        return;
    }
    #[allow(unreachable_code)]
    for i in 0..u_in.len() {
        let cb = i32::from(u_in[i]) - 128;
        let cr = i32::from(v_in[i]) - 128;
        u_out[i] = (((cb * cos - cr * sin) >> 16) + 128).clamp(0, 255) as u8;
        v_out[i] = (((cb * sin + cr * cos) >> 16) + 128).clamp(0, 255) as u8;
    }
}

pub struct Grade {
    luma: [u8; 256],
    luma_identity: bool,
    turn: (i32, i32),
    baked: Option<BakedLut>,
}

#[inline(always)]
fn byte(value: f32) -> u8 {
    value.round().clamp(0.0, 255.0) as u8
}

impl Grade {
    pub fn new(color: &config::Color, meta: &Meta) -> Result<Option<Self>> {
        if !color.enabled {
            return Ok(None);
        }
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
        let matrix = Matrix {
            kr,
            kb,
            offset,
            luma: luma_scale,
            chroma,
        };
        let baked = if color.lut.is_empty() {
            None
        } else {
            let path = Path::new(&color.lut);
            let local = config::app_dir().join(path);
            let lut = Lut::load(if path.exists() { path } else { &local })?;
            Some(BakedLut::new(&lut, color, &matrix))
        };
        let luma: [u8; 256] = std::array::from_fn(|value| {
            byte(((value as f32 - 126.0) * color.contrast + 126.0) * color.brightness)
        });
        let luma_identity = luma.iter().enumerate().all(|(i, &v)| usize::from(v) == i);
        let (sin, cos) = color.hue.to_radians().sin_cos();
        let scale = color.saturation * FIXED as f32;
        Ok(Some(Self {
            luma,
            luma_identity,
            turn: ((cos * scale).round() as i32, (sin * scale).round() as i32),
            baked,
        }))
    }

    pub fn apply(&self, source: &frame::Video) -> frame::Video {
        let (width, height) = (source.width() as usize, source.height() as usize);
        let yuv444 = decode::yuv444(source.format());
        let format = if yuv444 {
            Pixel::YUV444P
        } else {
            Pixel::YUV420P
        };
        let output = frame::Video::new(format, source.width(), source.height());
        let from: [(&[u8], usize); 3] =
            std::array::from_fn(|plane| (source.data(plane), source.stride(plane)));
        let strides: [usize; 3] = std::array::from_fn(|plane| output.stride(plane));
        let raw = unsafe { &*output.as_ptr() };
        let [y, u, v] = std::array::from_fn(|plane| {
            let rows = if plane == 0 || yuv444 {
                height
            } else {
                height / 2
            };
            unsafe { std::slice::from_raw_parts_mut(raw.data[plane], strides[plane] * rows) }
        });
        let piece = budget::piece(height);
        if yuv444 {
            budget::install(|| {
                y.par_chunks_mut(strides[0])
                    .zip(u.par_chunks_mut(strides[1]))
                    .zip(v.par_chunks_mut(strides[2]))
                    .enumerate()
                    .with_min_len(piece)
                    .for_each(|(row, ((y, u), v))| {
                        let line = |plane: usize| &from[plane].0[row * from[plane].1..][..width];
                        self.pixels(
                            (line(0), &mut y[..width]),
                            (line(1), &mut u[..width]),
                            (line(2), &mut v[..width]),
                        );
                    })
            });
            return output;
        }
        budget::install(|| {
            y.par_chunks_mut(strides[0] * 2)
                .zip(u.par_chunks_mut(strides[1]))
                .zip(v.par_chunks_mut(strides[2]))
                .enumerate()
                .with_min_len(piece.div_ceil(2))
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
                })
        });
        output
    }

    fn map_luma(&self, source: &[u8], target: &mut [u8]) {
        if self.luma_identity {
            target.copy_from_slice(source);
            return;
        }
        for (target, &source) in target.iter_mut().zip(source) {
            *target = self.luma[usize::from(source)];
        }
    }

    fn pixels(&self, luma: (&[u8], &mut [u8]), u: (&[u8], &mut [u8]), v: (&[u8], &mut [u8])) {
        let Some(baked) = &self.baked else {
            self.map_luma(luma.0, luma.1);
            turn_chroma_row(u.0, v.0, u.1, v.1, self.turn.0, self.turn.1);
            return;
        };
        for x in 0..luma.0.len() {
            let out = baked.sample(luma.0[x], u.0[x], v.0[x]);
            luma.1[x] = byte(out[0]);
            u.1[x] = byte(out[1]);
            v.1[x] = byte(out[2]);
        }
    }

    fn rows(&self, luma: [(&[u8], &mut [u8]); 2], u: (&[u8], &mut [u8]), v: (&[u8], &mut [u8])) {
        let Some(baked) = &self.baked else {
            for (source, target) in luma {
                self.map_luma(source, target);
            }
            turn_chroma_row(u.0, v.0, u.1, v.1, self.turn.0, self.turn.1);
            return;
        };
        let [(top, top_out), (bottom, bottom_out)] = luma;
        let size = baked.size;
        for x in 0..u.0.len() {
            let (iu, du) = baked.coord[usize::from(u.0[x])];
            let (iv, dv) = baked.coord[usize::from(v.0[x])];
            let base_uv = (iv * size + iu) * size;
            let p0 = baked.sample_uv(top[x * 2], du, dv, base_uv);
            let p1 = baked.sample_uv(top[x * 2 + 1], du, dv, base_uv);
            let p2 = baked.sample_uv(bottom[x * 2], du, dv, base_uv);
            let p3 = baked.sample_uv(bottom[x * 2 + 1], du, dv, base_uv);
            top_out[x * 2] = byte(p0[0]);
            top_out[x * 2 + 1] = byte(p1[0]);
            bottom_out[x * 2] = byte(p2[0]);
            bottom_out[x * 2 + 1] = byte(p3[0]);
            u.1[x] = byte((p0[1] + p1[1] + p2[1] + p3[1]) * 0.25);
            v.1[x] = byte((p0[2] + p1[2] + p2[2] + p3[2]) * 0.25);
        }
    }
}
