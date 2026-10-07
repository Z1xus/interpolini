use std::ffi::c_void;
use std::path::Path;
use std::sync::Arc;

use ffmpeg_next::format::Pixel;
use ffmpeg_next::{ffi, frame};

use crate::blend::Blend;
use crate::color::Grade;
use crate::config::{self, Config, Engine};
use crate::decode::{Decoder, Meta};
use crate::dedup::{Dedup, Pick};
use crate::rife::Rife;
use crate::svp::{Clip, Frame, Options, Shape, Svp};
use crate::{Event, Result, Sink, weights};

#[derive(Clone, Copy)]
pub struct Info {
    pub width: u32,
    pub height: u32,
    pub fps: (i64, i64),
    pub frames: i32,
}

pub struct Graph {
    derived: Option<Clip>,
    input: Arc<Clip>,
    blend: Option<Blend>,
    grade: Option<Grade>,
    pub(crate) meta: Meta,
    pub info: Info,
}

pub struct Plan {
    pub pre: Option<u32>,
    pub fps: u32,
    pub interpolate: bool,
    pub blended: bool,
    pub weights: Vec<f64>,
}

impl Plan {
    pub fn new(source_fps: f64, config: &Config) -> Self {
        let Config {
            pre,
            interpolation,
            blending,
            ..
        } = config;
        let pre =
            Some(pre.fps.at(source_fps)).filter(|fps| pre.enabled && f64::from(*fps) > source_fps);
        let source_fps = pre.map_or(source_fps, f64::from);
        let fps = interpolation.fps.at(source_fps);
        let interpolate = interpolation.enabled && f64::from(fps) > source_fps;
        let middle_fps = if interpolate {
            f64::from(fps)
        } else {
            source_fps
        };
        let blended = blending.enabled && f64::from(blending.fps) < middle_fps;
        let weights = match blended {
            true => weights::plan(middle_fps, blending),
            false => vec![1.0],
        };
        Self {
            pre,
            fps,
            interpolate,
            blended,
            weights,
        }
    }
}

// a config that changes no frame lets the streams be copied
pub fn lossless(source_fps: f64, config: &Config) -> bool {
    let plan = Plan::new(source_fps, config);
    let untouched = !plan.interpolate && !plan.blended && plan.pre.is_none();
    let plain = config.output.options.is_empty() && config.output.audio == config::Audio::Separate;
    untouched && plain && !config.dedup.enabled && !config.color.enabled
}

pub struct Image {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

unsafe extern "C" fn release(holder: *mut c_void, _: *mut u8) {
    drop(unsafe { Box::from_raw(holder.cast::<Frame>()) });
}

fn wrap(source: Frame, width: u32, height: u32) -> frame::Video {
    let mut picture = frame::Video::empty();
    unsafe {
        let raw = &mut *picture.as_mut_ptr();
        raw.format = ffi::AVPixelFormat::from(Pixel::YUV420P) as i32;
        raw.width = width as i32;
        raw.height = height as i32;
        for plane in 0..3 {
            raw.data[plane] = source.data(plane).cast_mut();
            raw.linesize[plane] = source.stride(plane) as i32;
        }
        let size = source.stride(0) * height as usize;
        let holder = Box::into_raw(Box::new(source)).cast();
        let flags = ffi::AV_BUFFER_FLAG_READONLY;
        raw.buf[0] = ffi::av_buffer_create(raw.data[0], size, Some(release), holder, flags);
    }
    picture
}

impl Graph {
    pub fn open(clip: &Path, config: &Config, sink: Sink) -> Result<Self> {
        let decoder = Decoder::open(clip)?;
        let meta = decoder.meta();
        let Meta {
            width,
            height,
            fps,
            frames,
            ..
        } = meta;
        let source_fps = fps.0 as f64 / fps.1 as f64;
        sink(Event::Info(format!(
            "{width}x{height}  |  {source_fps:.2} fps  |  {frames} frames"
        )));

        let Config {
            dedup,
            interpolation,
            rife,
            blending,
            ..
        } = config;
        let mask = config.mask.enabled.then_some(&config.mask);
        let half = interpolation.half && width % 4 == 0 && height % 4 == 0;
        let size = (width as usize, height as usize);

        // a source that reads other clips starves the pool of their host, so it gets its own
        let host = || Svp::load(&config::app_dir());
        let mut svp = host()?;
        let shape = Shape {
            width,
            height,
            fps,
            frames,
        };
        let mut input = Arc::new(svp.source(&shape, decoder)?);

        if dedup.enabled {
            let mut fills = Vec::new();
            for offset in 0..2 {
                let shape = Shape {
                    fps: (fps.0, fps.1 * 2),
                    frames: ((frames - offset + 1) / 2).max(1),
                    ..shape
                };
                let pick = Pick {
                    source: Arc::clone(&input),
                    step: 2,
                    offset,
                    width: size.0,
                    height: size.1,
                };
                let pick = svp.source(&shape, pick)?;
                // without the still mask: over the double gap of these clips it leaves specks
                let options = |gpu| Options::new(interpolation, "num:2,den:1", None, gpu, half);
                let fill = svp.smooth(&pick, &options(true));
                let fill = fill.or_else(|_| svp.smooth(&pick, &options(false)))?;
                fills.push(fill.keep(pick));
            }
            let node = Dedup {
                source: input,
                fills: fills.try_into().map_err(|_| "dedup needs two clips")?,
                threshold: dedup.threshold,
                last: frames - 1,
                width: size.0,
                height: size.1,
            };
            svp = host()?;
            input = Arc::new(svp.source(&shape, node)?);
            sink(Event::Info("replacing repeated frames".into()));
        }

        let Plan {
            pre,
            fps: target,
            interpolate,
            blended,
            weights,
        } = Plan::new(source_fps, config);
        if let Some(fps) = pre {
            svp = host()?;
            let clip = Rife::open(&svp, Arc::clone(&input), &meta, fps, rife, sink)?;
            input = Arc::new(clip);
            sink(Event::Info(format!("rife to {fps} fps first")));
        }
        let mut fused = false;
        let derived = match interpolation.engine {
            _ if !interpolate => None,
            Engine::Rife => {
                svp = host()?;
                let clip = Rife::open(&svp, Arc::clone(&input), &meta, target, rife, sink)?;
                sink(Event::Info(format!("rife to {target} fps")));
                Some(match mask {
                    Some(mask) => {
                        let relay = Pick {
                            source: Arc::clone(&input),
                            step: 1,
                            offset: 0,
                            width: size.0,
                            height: size.1,
                        };
                        let relay = svp.source(&input.shape(), relay)?;
                        svp.still(clip, &relay, mask)?.keep(relay)
                    }
                    None => clip,
                })
            }
            Engine::Svp => {
                let rate = format!("num:{target},den:1,abs:true");
                let options = |gpu| Options::new(interpolation, &rate, mask, gpu, half);
                let made = match blended {
                    true => svp.smooth_blend(&input, &options(true), &weights, blending.fps),
                    false => svp.smooth(&input, &options(true)),
                };
                fused = blended && made.is_ok();
                let clip = match (made, mask) {
                    (Ok(clip), _) => clip,
                    (Err(_), mask) => {
                        sink(Event::Warning("no opencl gpu, rendering on the cpu".into()));
                        let clip = svp.smooth(&input, &options(false))?;
                        match mask {
                            Some(mask) => svp.still(clip, &input, mask)?,
                            None => clip,
                        }
                    }
                };
                let half = if half { ", half-size analysis" } else { "" };
                sink(Event::Info(format!("svp to {target} fps{half}")));
                Some(clip)
            }
        };

        let shape = derived.as_ref().unwrap_or(&*input).shape();
        let blend = (blended && !fused)
            .then(|| Blend::new(&weights, shape.fps, blending.fps, shape.frames));
        if blended {
            let fused = if fused { " on the gpu" } else { "" };
            sink(Event::Info(format!(
                "blending {} frames to {} fps{fused}",
                weights.len(),
                blending.fps
            )));
        }
        let info = Info {
            width,
            height,
            fps: if blend.is_some() {
                (i64::from(blending.fps), 1)
            } else {
                shape.fps
            },
            frames: blend.as_ref().map_or(shape.frames, |blend| blend.frames),
        };
        let grade = Grade::new(&config.color, &meta)?;
        Ok(Self {
            derived,
            input,
            blend,
            grade,
            meta,
            info,
        })
    }

    pub(crate) fn frame(&self, n: i32) -> Result<frame::Video> {
        let Info { width, height, .. } = self.info;
        let clip = self.derived.as_ref().unwrap_or(&*self.input);
        let picture = match &self.blend {
            Some(blend) => blend.frame(clip, n, width, height)?,
            None => wrap(clip.frame(n)?, width, height),
        };
        let mut picture = match &self.grade {
            Some(grade) => grade.apply(&picture),
            None => picture,
        };
        self.meta.tag(&mut picture);
        Ok(picture)
    }

    pub fn source_fps(&self) -> f64 {
        self.meta.fps.0 as f64 / self.meta.fps.1 as f64
    }

    pub fn preview(&self, n: i32, width: u32) -> Result<Image> {
        let picture = self.frame(n.clamp(0, self.info.frames - 1))?;
        image(&picture, &self.meta, width)
    }
}

pub fn image(picture: &frame::Video, meta: &Meta, width: u32) -> Result<Image> {
    let width = width.min(meta.width) & !1;
    let height = (u64::from(width) * u64::from(meta.height) / u64::from(meta.width)) as u32;
    let mut scaler = meta.scaler(Pixel::YUV420P, Pixel::RGBA, width, height)?;
    let mut rgba = vec![0u8; width as usize * height as usize * 4];
    unsafe {
        let source = &*picture.as_ptr();
        ffi::sws_scale(
            scaler.as_mut_ptr(),
            source.data.as_ptr().cast(),
            source.linesize.as_ptr(),
            0,
            meta.height as i32,
            [rgba.as_mut_ptr(), std::ptr::null_mut()].as_ptr(),
            [width as i32 * 4, 0].as_ptr(),
        );
    }
    Ok(Image {
        width,
        height,
        rgba,
    })
}
