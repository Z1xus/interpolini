use std::path::{Path, PathBuf};

use ffmpeg_next::format::Pixel;
use ffmpeg_next::{color, ffi, frame};
use rayon::prelude::*;

use crate::budget;
use crate::cache::{Cache, Reader};
use crate::config::Config;
use crate::decode::{Decoder, Meta};
use crate::graph::{Graph, Image, Info, image_into, image_size};
use crate::mask::Gray;
use crate::munch::{Munch, Muncher};
use crate::segments::Made;
use crate::work::Cancel;
use crate::{Result, Sink};

pub struct Scaler(*mut ffi::SwsContext);

// the scaler has no thread affinity
unsafe impl Send for Scaler {}

impl Scaler {
    pub fn new() -> Self {
        Self::with(ffi::SwsFlags::SWS_BILINEAR)
    }

    pub fn with(flags: ffi::SwsFlags) -> Self {
        let scaler = unsafe { ffi::sws_alloc_context() };
        unsafe {
            (*scaler).threads = budget::own() as i32;
            (*scaler).flags = flags as u32;
        }
        Self(scaler)
    }

    pub fn single(self) -> Self {
        unsafe { (*self.0).threads = 1 };
        self
    }

    pub fn run(&self, to: &mut frame::Video, from: &frame::Video) -> Result<()> {
        match unsafe { ffi::sws_scale_frame(self.0, to.as_mut_ptr(), from.as_ptr()) } {
            code if code < 0 => Err(ffmpeg_next::Error::from(code).into()),
            _ => Ok(()),
        }
    }
}

impl Drop for Scaler {
    fn drop(&mut self) {
        unsafe { ffi::sws_free_context(&mut self.0) };
    }
}

#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Place {
    // the middle of the clip from the middle of the canvas, as a part of the canvas
    pub x: f32,
    pub y: f32,
    // the width and the height, at 1 and 1 the clip fills the canvas in one direction
    pub scale: [f32; 2],
    // the part that is cut from each edge: left, top, right, bottom
    pub crop: [f32; 4],
}

impl Default for Place {
    fn default() -> Self {
        Self {
            x: 0.0,
            y: 0.0,
            scale: [1.0; 2],
            crop: [0.0; 4],
        }
    }
}

impl Place {
    fn edges(&self, canvas: (u32, u32), size: (u32, u32)) -> ([f64; 4], [f64; 4]) {
        let (wide, high) = (f64::from(canvas.0), f64::from(canvas.1));
        let (width, height) = (f64::from(size.0), f64::from(size.1));
        let fit = (wide / width).min(high / height);
        let width = width * fit * f64::from(self.scale[0]);
        let height = height * fit * f64::from(self.scale[1]);
        let left = wide / 2.0 + f64::from(self.x) * wide - width / 2.0;
        let top = high / 2.0 + f64::from(self.y) * high - height / 2.0;
        let crop = self.crop.map(f64::from);
        let whole = [left, top, left + width, top + height];
        let kept = [
            left + crop[0] * width,
            top + crop[1] * height,
            left + width - crop[2] * width,
            top + height - crop[3] * height,
        ];
        (whole, kept)
    }

    pub fn covers(&self, canvas: (u32, u32), size: (u32, u32)) -> bool {
        let (_, kept) = self.edges(canvas, size);
        let (wide, high) = (f64::from(canvas.0), f64::from(canvas.1));
        kept[0] < 1.0 && kept[1] < 1.0 && kept[2] > wide - 1.0 && kept[3] > high - 1.0
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Setup {
    pub size: Option<(u32, u32)>,
    pub fps: Option<f64>,
    pub background: [u8; 3],
    pub munch: Option<Munch>,
}

pub struct Picture(pub(crate) frame::Video);

// a second handle to the same pixels
pub(crate) fn share(picture: &frame::Video) -> frame::Video {
    unsafe { frame::Video::wrap(ffi::av_frame_clone(picture.as_ptr())) }
}

enum Kind {
    Clip(Box<Graph>),
    Still(frame::Video),
}

pub struct Source {
    kind: Kind,
    // the last picture, a clip with a low frame rate gives it more than once
    held: Option<(i32, frame::Video)>,
    cache: Option<Cache>,
}

impl Source {
    pub fn open(
        clip: &Path,
        config: &Config,
        still: bool,
        sink: Sink,
        cancel: &Cancel,
    ) -> Result<Self> {
        let (kind, cache) = match still {
            true => (Kind::Still(image_file(clip)?), None),
            false => {
                let graph = Graph::open(clip, config, sink, cancel)?;
                let cache = Cache::new(clip, config, &graph);
                (Kind::Clip(Box::new(graph)), cache)
            }
        };
        Ok(Self {
            kind,
            held: None,
            cache,
        })
    }

    pub fn graph(&self) -> Option<&Graph> {
        match &self.kind {
            Kind::Clip(graph) => Some(graph),
            Kind::Still(_) => None,
        }
    }

    pub(crate) fn made(&mut self, n: i32, cancel: &Cancel) -> Result<Made> {
        match &self.kind {
            Kind::Clip(graph) => graph.made(n, cancel),
            Kind::Still(picture) => Ok(Made::Picture(share(picture))),
        }
    }

    pub fn flat(rgba: &[u8], from: (u32, u32), size: (u32, u32)) -> Result<Self> {
        let mut source = frame::Video::new(Pixel::RGBA, from.0, from.1);
        let stride = source.stride(0);
        for (row, line) in rgba.chunks_exact(from.0 as usize * 4).enumerate() {
            source.data_mut(0)[row * stride..][..line.len()].copy_from_slice(line);
        }
        let mut picture = frame::Video::new(Pixel::YUV420P, size.0 & !1, size.1 & !1);
        let raw = unsafe { &mut *picture.as_mut_ptr() };
        raw.colorspace = ffi::AVColorSpace::AVCOL_SPC_BT709;
        raw.color_range = ffi::AVColorRange::AVCOL_RANGE_MPEG;
        Scaler::new().run(&mut picture, &source)?;
        Ok(Self {
            kind: Kind::Still(picture),
            held: None,
            cache: None,
        })
    }

    pub fn at(&mut self, seconds: f64, cancel: &Cancel) -> Result<Picture> {
        let graph = match &self.kind {
            Kind::Clip(graph) => graph,
            Kind::Still(picture) => return Ok(Picture(share(picture))),
        };
        let n = graph.info.number(seconds);
        if !matches!(&self.held, Some(held) if held.0 == n) {
            self.held = Some((n, graph.frame(n, cancel)?));
        }
        Ok(Picture(share(&self.held.as_ref().ok_or("no picture")?.1)))
    }

    pub fn play(&mut self, seconds: f64, cancel: &Cancel) -> Result<Picture> {
        self.keep(seconds, false, cancel)
    }

    // the frame goes into the cache, which waits for room
    pub fn store(&mut self, seconds: f64, cancel: &Cancel) -> Result<()> {
        self.keep(seconds, true, cancel).map(drop)
    }

    fn keep(&mut self, seconds: f64, wait: bool, cancel: &Cancel) -> Result<Picture> {
        let (Kind::Clip(graph), Some(cache)) = (&self.kind, &mut self.cache) else {
            return self.at(seconds, cancel);
        };
        let index = cache.index(seconds);
        let n = index * cache.form.every;
        if !matches!(&self.held, Some(held) if held.0 == n) {
            if let Some(picture) = cache.read(index) {
                return Ok(Picture(picture));
            }
            self.held = Some((n, graph.frame(n, cancel)?));
        }
        let picture = &self.held.as_ref().ok_or("no picture")?.1;
        match cache.write(index, picture, wait.then_some(cancel)) || !wait {
            true => Ok(Picture(share(picture))),
            false => Err("the cache has no encoder".into()),
        }
    }

    pub fn cached(&self, seconds: f64) -> bool {
        let cache = self.cache.as_ref();
        cache.is_some_and(|cache| cache.has(cache.index(seconds)))
    }

    pub fn every(&self) -> usize {
        self.cache
            .as_ref()
            .map_or(1, |cache| cache.form.every as usize)
    }

    pub fn missing(&self, from: f64, to: f64) -> Option<(f64, f64)> {
        self.cache.as_ref()?.missing(from, to)
    }
}

#[derive(Default)]
pub struct Sources(
    Vec<(PathBuf, Config, Source)>,
    Vec<(PathBuf, Config, Reader)>,
);

impl Sources {
    fn find(&self, clip: &Path, config: &Config) -> Option<usize> {
        let held = |held: &(PathBuf, Config, Source)| held.0 == clip && held.1 == *config;
        self.0.iter().position(held)
    }

    // the cache of a clip that is not open needs no graph
    fn reader(&mut self, clip: &Path, config: &Config) -> Option<&mut Reader> {
        let held = |held: &(PathBuf, Config, Reader)| held.0 == clip && held.1 == *config;
        let index = match self.1.iter().position(held) {
            Some(index) => index,
            None => {
                let reader = Reader::stored(clip, config)?;
                self.1.push((clip.to_owned(), config.clone(), reader));
                self.1.len() - 1
            }
        };
        Some(&mut self.1[index].2)
    }

    pub fn cached(&mut self, clip: &Path, config: &Config, seconds: f64) -> bool {
        match self.find(clip, config) {
            Some(index) => self.0[index].2.cached(seconds),
            None => self
                .reader(clip, config)
                .is_some_and(|reader| reader.has(reader.index(seconds))),
        }
    }

    // a picture from the cache, none when it is not there
    pub fn play(
        &mut self,
        clip: &Path,
        config: &Config,
        seconds: f64,
        cancel: &Cancel,
    ) -> Option<Result<Picture>> {
        if let Some(source) = self.get_mut(clip, config) {
            return Some(source.play(seconds, cancel));
        }
        let reader = self.reader(clip, config)?;
        let picture = reader.read(reader.index(seconds))?;
        Some(Ok(Picture(picture)))
    }

    // the size, the frame rate and the source frame rate
    pub fn info(&mut self, clip: &Path, config: &Config) -> Option<(Info, f64)> {
        if let Some(graph) = self.get(clip, config).and_then(Source::graph) {
            return Some((graph.info, graph.source_fps()));
        }
        let form = self.reader(clip, config)?.form;
        Some((form.info, form.source_fps()))
    }

    pub fn get(&self, clip: &Path, config: &Config) -> Option<&Source> {
        Some(&self.0[self.find(clip, config)?].2)
    }

    pub fn get_mut(&mut self, clip: &Path, config: &Config) -> Option<&mut Source> {
        let index = self.find(clip, config)?;
        Some(&mut self.0[index].2)
    }

    pub fn open(
        &mut self,
        clip: &Path,
        config: &Config,
        still: bool,
        sink: Sink,
        cancel: &Cancel,
    ) -> Result<()> {
        if self.find(clip, config).is_none() {
            let source = Source::open(clip, config, still, sink, cancel)?;
            self.add(clip.to_owned(), config.clone(), source);
        }
        Ok(())
    }

    pub fn add(&mut self, clip: PathBuf, config: Config, source: Source) {
        self.1.retain(|held| held.0 != clip || held.1 != config);
        self.0.push((clip, config, source));
    }

    pub fn keys(&self) -> Vec<(PathBuf, Config)> {
        let key = |held: &(PathBuf, Config, Source)| (held.0.clone(), held.1.clone());
        self.0.iter().map(key).collect()
    }

    // the caller closes the sources that keep turns down
    pub fn split(&mut self, keep: impl Fn(&Path, &Config) -> bool) -> Sources {
        let held = std::mem::take(&mut self.0).into_iter();
        let (kept, gone) = held.partition(|held| keep(&held.0, &held.1));
        self.0 = kept;
        self.1.retain(|held| keep(&held.0, &held.1));
        Sources(gone, Vec::new())
    }

    pub fn remask(&mut self, clip: &Path, to: &Config, gray: &Gray, sink: Sink, cancel: &Cancel) {
        let like = |held: &(PathBuf, Config, Source)| {
            let mut config = held.1.clone();
            (config.mask.image, config.mask.soft) = (to.mask.image.clone(), to.mask.soft);
            held.0 == clip && config == *to
        };
        let found = self.find(clip, to);
        let Some(index) = found.or_else(|| self.0.iter().position(like)) else {
            return;
        };
        let (_, config, source) = &mut self.0[index];
        let Kind::Clip(graph) = &mut source.kind else {
            return;
        };
        *config = to.clone();
        source.held = None;
        source.cache = Cache::new(clip, to, graph);
        if graph.remask(to, gray, sink, cancel).is_err() {
            self.0.remove(index);
        }
    }

    // a clip that is not open opens only for the call
    pub fn with<R>(
        &mut self,
        clip: &Path,
        config: &Config,
        cancel: &Cancel,
        run: impl FnOnce(&mut Source) -> Result<R>,
    ) -> Result<R> {
        match self.find(clip, config) {
            Some(index) => run(&mut self.0[index].2),
            None => run(&mut Source::open(clip, config, false, &|_| {}, cancel)?),
        }
    }
}

fn image_file(path: &Path) -> Result<frame::Video> {
    let decoder = Decoder::open(path)?;
    let meta = decoder.meta();
    let source = decoder.first()?;
    let descriptor = unsafe { ffi::av_pix_fmt_desc_get(source.format().into()).as_ref() };
    let clear =
        descriptor.is_some_and(|format| format.flags & ffi::AV_PIX_FMT_FLAG_ALPHA as u64 != 0);
    let format = layout(meta.yuv444, clear);
    let mut picture = frame::Video::new(format, meta.width, meta.height);
    Meta {
        space: color::Space::BT709,
        range: color::Range::MPEG,
        ..meta
    }
    .tag(&mut picture);
    Scaler::new().run(&mut picture, &source)?;
    Ok(picture)
}

fn layout(yuv444: bool, clear: bool) -> Pixel {
    match (yuv444, clear) {
        (false, false) => Pixel::YUV420P,
        (false, true) => Pixel::YUVA420P,
        (true, false) => Pixel::YUV444P,
        (true, true) => Pixel::YUVA444P,
    }
}

fn clear(format: Pixel) -> bool {
    matches!(format, Pixel::YUVA420P | Pixel::YUVA444P)
}

// the step from a luma size to a color plane size
fn shift(format: Pixel) -> u32 {
    u32::from(!matches!(
        format,
        Pixel::YUV444P | Pixel::YUVJ444P | Pixel::YUVA444P
    ))
}

// a frame header for a part of a picture: left, top, right, bottom
fn window(picture: &frame::Video, part: [u32; 4]) -> frame::Video {
    let mut view = frame::Video::empty();
    let chroma = shift(picture.format());
    unsafe {
        let (from, to) = (&*picture.as_ptr(), &mut *view.as_mut_ptr());
        (to.format, to.colorspace, to.color_range) =
            (from.format, from.colorspace, from.color_range);
        (to.width, to.height) = ((part[2] - part[0]) as i32, (part[3] - part[1]) as i32);
        for plane in (0..4).filter(|plane| !from.data[*plane].is_null()) {
            let shift = if plane == 1 || plane == 2 { chroma } else { 0 };
            let stride = from.linesize[plane];
            let start = (part[1] >> shift) as isize * stride as isize + (part[0] >> shift) as isize;
            to.linesize[plane] = stride;
            to.data[plane] = from.data[plane].offset(start);
        }
    }
    view
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn paste_opaque_row_avx2(target: &mut [u8], source: &[u8], level: u32) {
    use std::arch::x86_64::{
        _mm_loadu_si128, _mm_storeu_si128, _mm256_add_epi16, _mm256_castsi256_si128,
        _mm256_cvtepu8_epi16, _mm256_mullo_epi16, _mm256_packus_epi16, _mm256_permute4x64_epi64,
        _mm256_set1_epi16, _mm256_srli_epi16,
    };
    let len = target.len();
    let vlevel = _mm256_set1_epi16(level as i16);
    let vinv = _mm256_set1_epi16((255 - level) as i16);
    let c127 = _mm256_set1_epi16(127);
    let one = _mm256_set1_epi16(1);
    let mut x = 0;
    while x + 16 <= len {
        unsafe {
            let vf = _mm256_cvtepu8_epi16(_mm_loadu_si128(source.as_ptr().add(x).cast()));
            let vt = _mm256_cvtepu8_epi16(_mm_loadu_si128(target.as_ptr().add(x).cast()));
            let sum = _mm256_add_epi16(
                _mm256_add_epi16(_mm256_mullo_epi16(vf, vlevel), _mm256_mullo_epi16(vt, vinv)),
                c127,
            );
            let res = _mm256_srli_epi16::<8>(_mm256_add_epi16(
                _mm256_add_epi16(sum, one),
                _mm256_srli_epi16::<8>(sum),
            ));
            let p8 = _mm256_permute4x64_epi64::<0xD8>(_mm256_packus_epi16(res, res));
            _mm_storeu_si128(target.as_mut_ptr().add(x).cast(), _mm256_castsi256_si128(p8));
        }
        x += 16;
    }
    let inv = 255 - level;
    for (to, &from) in target[x..].iter_mut().zip(&source[x..]) {
        let sum = u32::from(from) * level + u32::from(*to) * inv + 127;
        *to = ((sum + 1 + (sum >> 8)) >> 8) as u8;
    }
}

#[cfg(target_arch = "aarch64")]
fn paste_opaque_row_neon(target: &mut [u8], source: &[u8], level: u32) {
    use std::arch::aarch64::{
        vaddq_u16, vcombine_u8, vdup_n_u8, vdupq_n_u16, vget_high_u8, vget_low_u8, vld1q_u8,
        vmlal_u8, vmull_u8, vshrn_n_u16, vshrq_n_u16, vst1q_u8,
    };
    let len = target.len();
    let (vlevel, vinv, c127, one) = unsafe {
        (
            vdup_n_u8(level as u8),
            vdup_n_u8((255 - level) as u8),
            vdupq_n_u16(127),
            vdupq_n_u16(1),
        )
    };
    let mut x = 0;
    while x + 16 <= len {
        unsafe {
            let vf = vld1q_u8(source.as_ptr().add(x));
            let vt = vld1q_u8(target.as_ptr().add(x));
            let s0 = vaddq_u16(
                vmlal_u8(vmull_u8(vget_low_u8(vf), vlevel), vget_low_u8(vt), vinv),
                c127,
            );
            let s1 = vaddq_u16(
                vmlal_u8(vmull_u8(vget_high_u8(vf), vlevel), vget_high_u8(vt), vinv),
                c127,
            );
            let r0 = vshrn_n_u16::<8>(vaddq_u16(vaddq_u16(s0, one), vshrq_n_u16::<8>(s0)));
            let r1 = vshrn_n_u16::<8>(vaddq_u16(vaddq_u16(s1, one), vshrq_n_u16::<8>(s1)));
            vst1q_u8(target.as_mut_ptr().add(x), vcombine_u8(r0, r1));
        }
        x += 16;
    }
    let inv = 255 - level;
    for (to, &from) in target[x..].iter_mut().zip(&source[x..]) {
        let sum = u32::from(from) * level + u32::from(*to) * inv + 127;
        *to = ((sum + 1 + (sum >> 8)) >> 8) as u8;
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn paste_alpha_row_avx2(target: &mut [u8], source: &[u8], cover: &[u8], level: u32) {
    use std::arch::x86_64::{
        _mm_loadu_si128, _mm_storeu_si128, _mm256_add_epi16, _mm256_castsi256_si128,
        _mm256_cvtepu8_epi16, _mm256_mullo_epi16, _mm256_packus_epi16, _mm256_permute4x64_epi64,
        _mm256_set1_epi16, _mm256_srli_epi16, _mm256_sub_epi16,
    };
    let len = target.len();
    let vlevel = _mm256_set1_epi16(level as i16);
    let c255 = _mm256_set1_epi16(255);
    let c127 = _mm256_set1_epi16(127);
    let one = _mm256_set1_epi16(1);
    let mut x = 0;
    while x + 16 <= len {
        unsafe {
            let vc = _mm256_cvtepu8_epi16(_mm_loadu_si128(cover.as_ptr().add(x).cast()));
            let c = if level == 255 {
                vc
            } else {
                let prod = _mm256_mullo_epi16(vc, vlevel);
                _mm256_srli_epi16::<8>(_mm256_add_epi16(
                    _mm256_add_epi16(prod, one),
                    _mm256_srli_epi16::<8>(prod),
                ))
            };
            let inv = _mm256_sub_epi16(c255, c);
            let vf = _mm256_cvtepu8_epi16(_mm_loadu_si128(source.as_ptr().add(x).cast()));
            let vt = _mm256_cvtepu8_epi16(_mm_loadu_si128(target.as_ptr().add(x).cast()));
            let sum = _mm256_add_epi16(
                _mm256_add_epi16(_mm256_mullo_epi16(vf, c), _mm256_mullo_epi16(vt, inv)),
                c127,
            );
            let res = _mm256_srli_epi16::<8>(_mm256_add_epi16(
                _mm256_add_epi16(sum, one),
                _mm256_srli_epi16::<8>(sum),
            ));
            let p8 = _mm256_permute4x64_epi64::<0xD8>(_mm256_packus_epi16(res, res));
            _mm_storeu_si128(target.as_mut_ptr().add(x).cast(), _mm256_castsi256_si128(p8));
        }
        x += 16;
    }
    for i in x..len {
        let raw = u32::from(cover[i]) * level;
        let c = if level == 255 {
            u32::from(cover[i])
        } else {
            (raw + 1 + (raw >> 8)) >> 8
        };
        let sum = u32::from(source[i]) * c + u32::from(target[i]) * (255 - c) + 127;
        target[i] = ((sum + 1 + (sum >> 8)) >> 8) as u8;
    }
}

#[cfg(target_arch = "aarch64")]
fn paste_alpha_row_neon(target: &mut [u8], source: &[u8], cover: &[u8], level: u32) {
    use std::arch::aarch64::{
        vaddq_u16, vcombine_u8, vdupq_n_u16, vget_low_u8, vld1q_u8, vmlaq_u16, vmovl_high_u8,
        vmovl_u8, vmulq_u16, vshrn_n_u16, vshrq_n_u16, vst1q_u8, vsubq_u16,
    };
    let len = target.len();
    let (vlevel, c255, c127, one) = unsafe {
        (
            vdupq_n_u16(level as u16),
            vdupq_n_u16(255),
            vdupq_n_u16(127),
            vdupq_n_u16(1),
        )
    };
    let mut x = 0;
    while x + 16 <= len {
        unsafe {
            let vc = vld1q_u8(cover.as_ptr().add(x));
            let vf = vld1q_u8(source.as_ptr().add(x));
            let vt = vld1q_u8(target.as_ptr().add(x));
            let mut c0 = vmovl_u8(vget_low_u8(vc));
            let mut c1 = vmovl_high_u8(vc);
            if level < 255 {
                let p0 = vmulq_u16(c0, vlevel);
                let p1 = vmulq_u16(c1, vlevel);
                c0 = vshrq_n_u16::<8>(vaddq_u16(vaddq_u16(p0, one), vshrq_n_u16::<8>(p0)));
                c1 = vshrq_n_u16::<8>(vaddq_u16(vaddq_u16(p1, one), vshrq_n_u16::<8>(p1)));
            }
            let s0 = vaddq_u16(
                vmlaq_u16(
                    vmulq_u16(vmovl_u8(vget_low_u8(vf)), c0),
                    vmovl_u8(vget_low_u8(vt)),
                    vsubq_u16(c255, c0),
                ),
                c127,
            );
            let s1 = vaddq_u16(
                vmlaq_u16(
                    vmulq_u16(vmovl_high_u8(vf), c1),
                    vmovl_high_u8(vt),
                    vsubq_u16(c255, c1),
                ),
                c127,
            );
            let r0 = vshrn_n_u16::<8>(vaddq_u16(vaddq_u16(s0, one), vshrq_n_u16::<8>(s0)));
            let r1 = vshrn_n_u16::<8>(vaddq_u16(vaddq_u16(s1, one), vshrq_n_u16::<8>(s1)));
            vst1q_u8(target.as_mut_ptr().add(x), vcombine_u8(r0, r1));
        }
        x += 16;
    }
    for i in x..len {
        let raw = u32::from(cover[i]) * level;
        let c = if level == 255 {
            u32::from(cover[i])
        } else {
            (raw + 1 + (raw >> 8)) >> 8
        };
        let sum = u32::from(source[i]) * c + u32::from(target[i]) * (255 - c) + 127;
        target[i] = ((sum + 1 + (sum >> 8)) >> 8) as u8;
    }
}

fn paste_row(
    target: &mut [u8],
    source: &[u8],
    cover: Option<&[u8]>,
    shift: usize,
    level: u32,
    #[cfg(target_arch = "x86_64")] avx2: bool,
) {
    let Some(cover) = cover else {
        if level == 255 {
            target.copy_from_slice(source);
            return;
        }
        #[cfg(target_arch = "x86_64")]
        if avx2 {
            unsafe { paste_opaque_row_avx2(target, source, level) };
            return;
        }
        #[cfg(target_arch = "aarch64")]
        {
            paste_opaque_row_neon(target, source, level);
            return;
        }
        #[allow(unreachable_code)]
        {
            let inv = 255 - level;
            for (to, &from) in target.iter_mut().zip(source) {
                let sum = u32::from(from) * level + u32::from(*to) * inv + 127;
                *to = ((sum + 1 + (sum >> 8)) >> 8) as u8;
            }
            return;
        }
    };
    if shift == 0 {
        #[cfg(target_arch = "x86_64")]
        if avx2 {
            unsafe { paste_alpha_row_avx2(target, source, cover, level) };
            return;
        }
        #[cfg(target_arch = "aarch64")]
        {
            paste_alpha_row_neon(target, source, cover, level);
            return;
        }
    }
    for (index, (to, &from)) in target.iter_mut().zip(source).enumerate() {
        let a = u32::from(cover[index << shift]);
        let c = if level == 255 {
            a
        } else {
            let raw = a * level;
            (raw + 1 + (raw >> 8)) >> 8
        };
        let sum = u32::from(from) * c + u32::from(*to) * (255 - c) + 127;
        *to = ((sum + 1 + (sum >> 8)) >> 8) as u8;
    }
}

fn paste(canvas: &mut frame::Video, picture: &frame::Video, left: u32, top: u32, opacity: f32) {
    let clear = clear(picture.format());
    let level = (opacity.clamp(0.0, 1.0) * 255.0).round() as u32;
    let chroma = shift(picture.format()) as usize;
    let (w, h) = (picture.width() as usize, picture.height() as usize);
    let (cw, ch) = (w >> chroma, h >> chroma);
    let (left_y, top_y) = (left as usize, top as usize);
    let (left_c, top_c) = (left_y >> chroma, top_y >> chroma);
    let src_y = (picture.data(0), picture.stride(0));
    let src_u = (picture.data(1), picture.stride(1));
    let src_v = (picture.data(2), picture.stride(2));
    let cover = clear.then(|| (picture.data(3), picture.stride(3)));
    let (sy, su, sv) = (canvas.stride(0), canvas.stride(1), canvas.stride(2));
    let raw = unsafe { &*canvas.as_ptr() };
    let (dst_y, dst_u, dst_v) = unsafe {
        (
            std::slice::from_raw_parts_mut(raw.data[0].add(top_y * sy), h * sy),
            std::slice::from_raw_parts_mut(raw.data[1].add(top_c * su), ch * su),
            std::slice::from_raw_parts_mut(raw.data[2].add(top_c * sv), ch * sv),
        )
    };
    #[cfg(target_arch = "x86_64")]
    let avx2 = std::arch::is_x86_feature_detected!("avx2");
    if w * h >= 262_144 {
        if chroma == 0 {
            let piece = budget::piece(h);
            budget::install(|| {
                dst_y
                    .par_chunks_mut(sy)
                    .zip(dst_u.par_chunks_mut(su))
                    .zip(dst_v.par_chunks_mut(sv))
                    .enumerate()
                    .with_min_len(piece)
                    .for_each(|(row, ((y_row, u_row), v_row))| {
                        let cov = cover.map(|(data, wide)| &data[row * wide..]);
                        paste_row(
                            &mut y_row[left_y..left_y + w],
                            &src_y.0[row * src_y.1..][..w],
                            cov,
                            0,
                            level,
                            #[cfg(target_arch = "x86_64")]
                            avx2,
                        );
                        paste_row(
                            &mut u_row[left_c..left_c + cw],
                            &src_u.0[row * src_u.1..][..cw],
                            cov,
                            0,
                            level,
                            #[cfg(target_arch = "x86_64")]
                            avx2,
                        );
                        paste_row(
                            &mut v_row[left_c..left_c + cw],
                            &src_v.0[row * src_v.1..][..cw],
                            cov,
                            0,
                            level,
                            #[cfg(target_arch = "x86_64")]
                            avx2,
                        );
                    });
            });
        } else {
            let piece = budget::piece(ch);
            budget::install(|| {
                dst_y
                    .par_chunks_mut(sy * 2)
                    .take(ch)
                    .zip(dst_u.par_chunks_mut(su).take(ch))
                    .zip(dst_v.par_chunks_mut(sv).take(ch))
                    .enumerate()
                    .with_min_len(piece)
                    .for_each(|(cy, ((y_pair, u_row), v_row))| {
                        let (y0, y1) = y_pair.split_at_mut(sy);
                        let r0 = cy * 2;
                        let r1 = r0 + 1;
                        let cov0 = cover.map(|(data, wide)| &data[r0 * wide..]);
                        let cov1 = cover.map(|(data, wide)| &data[r1 * wide..]);
                        paste_row(
                            &mut y0[left_y..left_y + w],
                            &src_y.0[r0 * src_y.1..][..w],
                            cov0,
                            0,
                            level,
                            #[cfg(target_arch = "x86_64")]
                            avx2,
                        );
                        paste_row(
                            &mut y1[left_y..left_y + w],
                            &src_y.0[r1 * src_y.1..][..w],
                            cov1,
                            0,
                            level,
                            #[cfg(target_arch = "x86_64")]
                            avx2,
                        );
                        paste_row(
                            &mut u_row[left_c..left_c + cw],
                            &src_u.0[cy * src_u.1..][..cw],
                            cov0,
                            1,
                            level,
                            #[cfg(target_arch = "x86_64")]
                            avx2,
                        );
                        paste_row(
                            &mut v_row[left_c..left_c + cw],
                            &src_v.0[cy * src_v.1..][..cw],
                            cov0,
                            1,
                            level,
                            #[cfg(target_arch = "x86_64")]
                            avx2,
                        );
                    });
            });
        }
        return;
    }
    for row in 0..h {
        let cov = cover.map(|(data, wide)| &data[row * wide..]);
        paste_row(
            &mut dst_y[row * sy + left_y..][..w],
            &src_y.0[row * src_y.1..][..w],
            cov,
            0,
            level,
            #[cfg(target_arch = "x86_64")]
            avx2,
        );
    }
    for cy in 0..ch {
        let cov = cover.map(|(data, wide)| &data[(cy << chroma) * wide..]);
        paste_row(
            &mut dst_u[cy * su + left_c..][..cw],
            &src_u.0[cy * src_u.1..][..cw],
            cov,
            chroma,
            level,
            #[cfg(target_arch = "x86_64")]
            avx2,
        );
        paste_row(
            &mut dst_v[cy * sv + left_c..][..cw],
            &src_v.0[cy * src_v.1..][..cw],
            cov,
            chroma,
            level,
            #[cfg(target_arch = "x86_64")]
            avx2,
        );
    }
}

pub struct Canvas {
    meta: Meta,
    background: [u8; 3],
    scalers: Vec<Scaler>,
    scaled: Vec<Option<frame::Video>>,
    rgba: Option<frame::Video>,
    shown: Scaler,
    munch: Option<Muncher>,
}

impl Canvas {
    pub fn open(clip: &Path, setup: &Setup) -> Result<Self> {
        let mut meta = match Decoder::open(clip) {
            Ok(decoder) => decoder.meta(),
            Err(_) if !clip.exists() => Meta::standard(),
            Err(error) => return Err(error),
        };
        if let Some(size) = setup.size {
            (meta.width, meta.height) = size;
        }
        (meta.width, meta.height) = (meta.width.max(2), meta.height.max(2));
        let mut canvas = Self::new(meta, setup.background);
        if let Some(munch) = setup.munch {
            canvas.munch = Some(Muncher::new(&canvas.meta, &munch)?);
            // slice threads leave gray lines in a small picture
            canvas.shown = Scaler::new().single();
        }
        Ok(canvas)
    }

    pub(crate) fn new(meta: Meta, background: [u8; 3]) -> Self {
        Self {
            meta,
            background,
            scalers: Vec::new(),
            scaled: Vec::new(),
            rgba: None,
            shown: Scaler::new(),
            munch: None,
        }
    }

    pub fn size(&self) -> (u32, u32) {
        (self.meta.width, self.meta.height)
    }

    pub(crate) fn black(&self, size: (u32, u32)) -> frame::Video {
        let mut picture = frame::Video::new(self.meta.pixel(), size.0, size.1);
        let [y, u, v] = self.meta.yuv(self.background);
        picture.data_mut(0).fill(y);
        picture.data_mut(1).fill(u);
        picture.data_mut(2).fill(v);
        self.meta.tag(&mut picture);
        picture
    }

    // the layers go on a black canvas, the lowest one first
    pub(crate) fn compose(&mut self, layers: Vec<(Picture, Place, f32)>) -> Result<frame::Video> {
        self.draw(layers, self.size(), false)
    }

    fn draw(
        &mut self,
        mut layers: Vec<(Picture, Place, f32)>,
        size: (u32, u32),
        preview: bool,
    ) -> Result<frame::Video> {
        let full = self.size();
        let (space, range) = self.meta.tags();
        let tagged = |picture: &frame::Video| unsafe {
            let picture = &*picture.as_ptr();
            (picture.colorspace, picture.color_range) == (space, range)
        };
        let like = |picture: &frame::Video| {
            let same = (picture.width(), picture.height()) == full;
            picture.format() == self.meta.pixel() && same && tagged(picture)
        };
        // the planes of the canvas, with the transparency of the picture
        let fitted = |picture: &frame::Video| layout(self.meta.yuv444, clear(picture.format()));
        // a clip that is the canvas goes through as it is
        let whole = |layer: &(Picture, Place, f32)| {
            layer.1 == Place::default() && layer.2 >= 1.0 && like(&layer.0.0)
        };
        if matches!(&layers[..], [layer] if whole(layer)) {
            return Ok(layers.remove(0).0.0);
        }
        let mut canvas: Option<frame::Video> = None;
        for (index, (Picture(picture), place, opacity)) in layers.iter().enumerate() {
            if self.scalers.len() <= index {
                self.scalers.push(Scaler::new());
            }
            if self.scaled.len() <= index {
                self.scaled.push(None);
            }
            let (width, height) = (picture.width(), picture.height());
            let (whole, kept) = place.edges(size, (width, height));
            let even = |value: f64, most: u32| {
                ((value / 2.0).round() * 2.0).clamp(0.0, f64::from(most)) as u32
            };
            // the part that is on the canvas, and the same part in the pixels of the clip
            let (left, top) = (even(kept[0], size.0), even(kept[1], size.1));
            let (right, bottom) = (even(kept[2], size.0), even(kept[3], size.1));
            // shrunken clips are a few rows short
            let near = |edge: u32, to: u32| edge.abs_diff(to) <= 4;
            let short =
                near(left, 0) && near(top, 0) && near(right, size.0) && near(bottom, size.1);
            let (left, top, right, bottom) = match preview && short && *place == Place::default() {
                true => (0, 0, size.0, size.1),
                false => (left, top, right, bottom),
            };
            if right <= left || bottom <= top {
                continue;
            }
            let from = |edge: u32, start: f64, end: f64, most: u32, least: u32| {
                let pixel = (f64::from(edge) - start) / (end - start) * f64::from(most);
                even(pixel, most).clamp(least, most.saturating_sub(2) + least)
            };
            let first = (
                from(left, whole[0], whole[2], width, 0),
                from(top, whole[1], whole[3], height, 0),
            );
            let part = [
                first.0,
                first.1,
                from(right, whole[0], whole[2], width, 2).max(first.0 + 2),
                from(bottom, whole[1], whole[3], height, 2).max(first.1 + 2),
            ];
            let whole = part == [0, 0, width, height];
            let format = fitted(picture);
            let same = (right - left, bottom - top) == (width, height)
                && picture.format() == format;
            let covers = canvas.is_none()
                && left == 0
                && top == 0
                && right == size.0
                && bottom == size.1
                && (opacity.clamp(0.0, 1.0) * 255.0).round() as u32 == 255
                && !clear(picture.format());
            if covers {
                if whole && same && tagged(picture) {
                    if index + 1 == layers.len() {
                        return Ok(share(picture));
                    }
                    let mut frame = frame::Video::new(self.meta.pixel(), size.0, size.1);
                    self.meta.tag(&mut frame);
                    paste(&mut frame, picture, 0, 0, 1.0);
                    canvas = Some(frame);
                    continue;
                }
                let mut frame = frame::Video::new(self.meta.pixel(), size.0, size.1);
                self.meta.tag(&mut frame);
                self.scalers[index].run(&mut frame, &window(picture, part))?;
                canvas = Some(frame);
                continue;
            }
            let canvas = canvas.get_or_insert_with(|| self.black(size));
            if whole && same && tagged(picture) {
                paste(canvas, picture, left, top, *opacity);
                continue;
            }
            let (dst_w, dst_h) = (right - left, bottom - top);
            let slot = &mut self.scaled[index];
            let reuse = slot.as_ref().is_some_and(|frame| {
                frame.format() == format
                    && frame.width() == dst_w
                    && frame.height() == dst_h
                    && unsafe { ffi::av_frame_is_writable(frame.as_ptr().cast_mut()) != 0 }
            });
            if !reuse {
                *slot = Some(frame::Video::new(format, dst_w, dst_h));
            }
            let scaled = slot.as_mut().ok_or("no frame")?;
            self.meta.tag(scaled);
            self.scalers[index].run(scaled, &window(picture, part))?;
            paste(canvas, scaled, left, top, *opacity);
        }
        Ok(canvas.unwrap_or_else(|| self.black(size)))
    }

    pub fn preview_into<R>(
        &mut self,
        layers: Vec<(Picture, Place, f32)>,
        width: u32,
        munched: bool,
        write: impl FnOnce(u32, u32, &mut dyn FnMut(&mut [u8]) -> Result<()>) -> Result<R>,
    ) -> Result<R> {
        let (wide, high) = self.size();
        let width = (width.min(wide) & !1).max(2);
        let height = ((u64::from(width) * u64::from(high) / u64::from(wide)) as u32 & !1).max(2);
        // full-size pictures need no scaler, the rest is drawn at the width that shows
        let fits = |layer: &(Picture, Place, f32)| {
            layer.1 == Place::default() && (layer.0.0.width(), layer.0.0.height()) == (wide, high)
        };
        let size = match layers.iter().all(fits) || (munched && self.munch.is_some()) {
            true => (wide, high),
            false => (width, height),
        };
        let mut picture = self.draw(layers, size, true)?;
        if let Some(muncher) = self.munch.as_mut().filter(|_| munched) {
            picture = muncher.run(&picture)?;
        }
        let (out_w, out_h) = image_size(&picture, width);
        let shown = &self.shown;
        let rgba = &mut self.rgba;
        let mut fill = |dst: &mut [u8]| image_into(shown, &picture, out_w, out_h, dst, Some(rgba));
        write(out_w, out_h, &mut fill)
    }

    pub fn preview(
        &mut self,
        layers: Vec<(Picture, Place, f32)>,
        width: u32,
        munched: bool,
    ) -> Result<Image> {
        self.preview_into(layers, width, munched, |width, height, fill| {
            let mut rgba = vec![0u8; width as usize * height as usize * 4];
            fill(&mut rgba)?;
            Ok(Image {
                width,
                height,
                rgba,
            })
        })
    }
}
