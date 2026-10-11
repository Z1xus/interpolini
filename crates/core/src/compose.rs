use std::path::Path;

use ffmpeg_next::format::Pixel;
use ffmpeg_next::{color, ffi, frame};

use crate::config::Config;
use crate::decode::{Decoder, Meta};
use crate::graph::{Graph, Image, image};
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
            (*scaler).threads = 0;
            (*scaler).flags = flags as u32;
        }
        Self(scaler)
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

#[derive(Clone, Copy, Debug, PartialEq)]
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

pub struct Picture(pub(crate) frame::Video);

// a second handle to the same pixels
fn share(picture: &frame::Video) -> frame::Video {
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
}

impl Source {
    pub fn open(clip: &Path, config: &Config, still: bool, sink: Sink) -> Result<Self> {
        let kind = match still {
            true => Kind::Still(image_file(clip)?),
            false => Kind::Clip(Box::new(Graph::open(clip, config, sink)?)),
        };
        Ok(Self { kind, held: None })
    }

    pub fn graph(&self) -> Option<&Graph> {
        match &self.kind {
            Kind::Clip(graph) => Some(graph),
            Kind::Still(_) => None,
        }
    }

    pub fn at(&mut self, seconds: f64) -> Result<Picture> {
        let graph = match &self.kind {
            Kind::Clip(graph) => graph,
            Kind::Still(picture) => return Ok(Picture(share(picture))),
        };
        let fps = graph.info.fps.0 as f64 / graph.info.fps.1 as f64;
        let n = ((seconds * fps).round() as i32).clamp(0, graph.info.frames - 1);
        if !matches!(&self.held, Some(held) if held.0 == n) {
            self.held = Some((n, graph.frame(n)?));
        }
        Ok(Picture(share(&self.held.as_ref().ok_or("no picture")?.1)))
    }
}

fn image_file(path: &Path) -> Result<frame::Video> {
    let decoder = Decoder::open(path)?;
    let meta = decoder.meta();
    let source = decoder.first()?;
    let descriptor = unsafe { ffi::av_pix_fmt_desc_get(source.format().into()).as_ref() };
    let clear =
        descriptor.is_some_and(|format| format.flags & ffi::AV_PIX_FMT_FLAG_ALPHA as u64 != 0);
    let format = if clear {
        Pixel::YUVA420P
    } else {
        Pixel::YUV420P
    };
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

// a frame header for a part of a picture: left, top, right, bottom
fn window(picture: &frame::Video, part: [u32; 4]) -> frame::Video {
    let mut view = frame::Video::empty();
    unsafe {
        let (from, to) = (&*picture.as_ptr(), &mut *view.as_mut_ptr());
        (to.format, to.colorspace, to.color_range) =
            (from.format, from.colorspace, from.color_range);
        (to.width, to.height) = ((part[2] - part[0]) as i32, (part[3] - part[1]) as i32);
        for plane in (0..4).filter(|plane| !from.data[*plane].is_null()) {
            let shift = u32::from(plane == 1 || plane == 2);
            let stride = from.linesize[plane];
            let start = (part[1] >> shift) as isize * stride as isize + (part[0] >> shift) as isize;
            to.linesize[plane] = stride;
            to.data[plane] = from.data[plane].offset(start);
        }
    }
    view
}

fn paste(canvas: &mut frame::Video, picture: &frame::Video, left: u32, top: u32, opacity: f32) {
    let clear = picture.format() == Pixel::YUVA420P;
    let level = (opacity * 255.0).round() as u32;
    for plane in 0..3 {
        let shift = usize::from(plane > 0);
        let (width, height) = (
            picture.width() as usize >> shift,
            picture.height() as usize >> shift,
        );
        let (left, top) = (left as usize >> shift, top as usize >> shift);
        let (from, wide) = (picture.data(plane), picture.stride(plane));
        let cover = clear.then(|| (picture.data(3), picture.stride(3)));
        let stride = canvas.stride(plane);
        let to = canvas.data_mut(plane);
        for row in 0..height {
            let target = &mut to[(top + row) * stride + left..][..width];
            let source = &from[row * wide..][..width];
            if cover.is_none() && level == 255 {
                target.copy_from_slice(source);
                continue;
            }
            let cover = cover.map(|(cover, wide)| &cover[(row << shift) * wide..]);
            for (index, (to, from)) in target.iter_mut().zip(source).enumerate() {
                let cover = cover.map_or(255, |cover| u32::from(cover[index << shift]));
                let cover = cover * level / 255;
                *to =
                    ((u32::from(*from) * cover + u32::from(*to) * (255 - cover) + 127) / 255) as u8;
            }
        }
    }
}

pub struct Canvas {
    meta: Meta,
    scalers: Vec<Scaler>,
    shown: Scaler,
}

impl Canvas {
    pub fn open(clip: &Path) -> Result<Self> {
        Ok(Self::new(Decoder::open(clip)?.meta()))
    }

    pub(crate) fn new(meta: Meta) -> Self {
        Self {
            meta,
            scalers: Vec::new(),
            shown: Scaler::new(),
        }
    }

    pub fn size(&self) -> (u32, u32) {
        (self.meta.width, self.meta.height)
    }

    fn black(&self) -> frame::Video {
        let mut picture = frame::Video::new(Pixel::YUV420P, self.meta.width, self.meta.height);
        // 16 is black in the limited range, and 128 is no color
        picture
            .data_mut(0)
            .fill(if self.meta.full() { 0 } else { 16 });
        picture.data_mut(1).fill(128);
        picture.data_mut(2).fill(128);
        self.meta.tag(&mut picture);
        picture
    }

    // the layers go on a black canvas, the lowest one first
    pub(crate) fn compose(
        &mut self,
        mut layers: Vec<(Picture, Place, f32)>,
    ) -> Result<frame::Video> {
        let size = self.size();
        let mut canvas = self.black();
        let like = |picture: &frame::Video| unsafe {
            let (one, other) = (&*picture.as_ptr(), &*canvas.as_ptr());
            picture.format() == Pixel::YUV420P
                && (picture.width(), picture.height()) == size
                && (one.colorspace, one.color_range) == (other.colorspace, other.color_range)
        };
        // a clip that is the canvas goes through as it is
        let whole = |layer: &(Picture, Place, f32)| {
            layer.1 == Place::default() && layer.2 >= 1.0 && like(&layer.0.0)
        };
        if matches!(&layers[..], [layer] if whole(layer)) {
            return Ok(layers.remove(0).0.0);
        }
        for (index, (Picture(picture), place, opacity)) in layers.iter().enumerate() {
            if self.scalers.len() <= index {
                self.scalers.push(Scaler::new());
            }
            let (width, height) = (picture.width(), picture.height());
            let (whole, kept) = place.edges(size, (width, height));
            let even = |value: f64, most: u32| {
                ((value / 2.0).round() * 2.0).clamp(0.0, f64::from(most)) as u32
            };
            // the part that is on the canvas, and the same part in the pixels of the clip
            let (left, top) = (even(kept[0], size.0), even(kept[1], size.1));
            let (right, bottom) = (even(kept[2], size.0), even(kept[3], size.1));
            if right <= left || bottom <= top {
                continue;
            }
            let from = |edge: u32, start: f64, end: f64, most: u32, least: u32| {
                let pixel = (f64::from(edge) - start) / (end - start) * f64::from(most);
                even(pixel, most).clamp(least, most - 2 + least)
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
            let mut scaled = frame::Video::new(picture.format(), right - left, bottom - top);
            self.meta.tag(&mut scaled);
            self.scalers[index].run(&mut scaled, &window(picture, part))?;
            paste(&mut canvas, &scaled, left, top, *opacity);
        }
        Ok(canvas)
    }

    pub fn preview(&mut self, layers: Vec<(Picture, Place, f32)>, width: u32) -> Result<Image> {
        let picture = self.compose(layers)?;
        image(&self.shown, &picture, width)
    }
}
