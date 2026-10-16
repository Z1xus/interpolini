use std::ffi::c_void;
use std::path::Path;
use std::sync::{Arc, Mutex};

use ffmpeg_next::format::Pixel;
use ffmpeg_next::{ffi, frame};

use crate::blend::Blend;
use crate::color::Grade;
use crate::compose::Scaler;
use crate::config::{self, Config, Engine, Masking};
use crate::decode::{Decoder, Meta};
use crate::dedup::Pick;
use crate::encode::Spare;
use crate::rife::Rife;
use crate::segments::{Made, Segments};
use crate::svp::{Clip, Frame, Options, Shape, Svp};
use crate::{Event, Result, Sink, weights};

#[derive(Clone, Copy)]
pub struct Info {
    pub width: u32,
    pub height: u32,
    pub fps: (i64, i64),
    pub frames: i32,
}

enum Feed {
    File(Box<Mutex<Segments>>),
    Host {
        derived: Option<Clip>,
        input: Arc<Clip>,
    },
}

pub struct Graph {
    feed: Feed,
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
    let output = &config.output;
    let plain = output.options.is_empty() && output.audio == config::Audio::Separate;
    let plain = plain && output.target == config::Target::Quality;
    untouched && plain && !config.dedup.enabled && !config.color.enabled && !config.upscale.enabled
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
            codec,
            ..
        } = meta;
        let source_fps = fps.0 as f64 / fps.1 as f64;
        sink(Event::Info(format!(
            "{codec}  |  {width}x{height}  |  {source_fps:.2} fps  |  {frames} frames"
        )));

        let plan = Plan::new(source_fps, config);
        let plain = !config.dedup.enabled && plan.pre.is_none() && !plan.interpolate;
        let Plan {
            blended, weights, ..
        } = &plan;
        let blending = &config.blending;
        let (feed, fused) = match *blended && plain {
            true => {
                let segments = Segments::new(clip, decoder, weights, blending);
                (Feed::File(Box::new(Mutex::new(segments))), false)
            }
            false => Self::host(decoder, config, &plan, sink)?,
        };
        let blend = match &feed {
            Feed::Host { derived, input } if *blended && !fused => {
                let shape = derived.as_ref().unwrap_or(input).shape();
                Some(Blend::new(weights, shape.fps, blending, shape.frames))
            }
            _ => None,
        };
        if *blended {
            let fused = if fused { " on the gpu" } else { "" };
            sink(Event::Info(format!(
                "blending {} frames to {} fps{fused}",
                weights.len(),
                blending.fps
            )));
        }
        let (fps, frames) = match (&feed, &blend) {
            (Feed::File(segments), _) => {
                let segments = segments.lock().unwrap_or_else(|poison| poison.into_inner());
                ((i64::from(blending.fps), 1), segments.frames)
            }
            (_, Some(blend)) => ((i64::from(blending.fps), 1), blend.frames),
            (Feed::Host { derived, input }, None) => {
                let shape = derived.as_ref().unwrap_or(input).shape();
                (shape.fps, shape.frames)
            }
        };
        let info = Info {
            width,
            height,
            fps,
            frames,
        };
        let grade = Grade::new(&config.color, &meta)?;
        Ok(Self {
            feed,
            blend,
            grade,
            meta,
            info,
        })
    }

    fn host(decoder: Decoder, config: &Config, plan: &Plan, sink: Sink) -> Result<(Feed, bool)> {
        let meta = decoder.meta();
        let Meta {
            width,
            height,
            fps,
            frames,
            ..
        } = meta;
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
            deep: false,
            width,
            height,
            fps,
            frames,
        };
        let mut input = Arc::new(svp.source(&shape, Mutex::new(decoder))?);

        let Plan {
            pre,
            fps: target,
            interpolate,
            blended,
            weights,
        } = plan;
        let (pre, target, interpolate, blended) = (*pre, *target, *interpolate, *blended);
        let skipped = interpolate && interpolation.engine == Engine::Svp && pre.is_none();
        let limit = dedup.enabled.then_some(dedup.threshold);
        let mut fused = false;
        if dedup.enabled && dedup.threshold > 0.0 && !skipped {
            let rate = format!("num:{},den:{},abs:true", fps.0, fps.1);
            let options = |gpu| Options::new(interpolation, &rate, mask, limit, gpu, half);
            let last = !interpolate && pre.is_none();
            let made = match blended && last {
                true => svp.smooth_blend(&input, &options(true), weights, blending),
                false => svp.smooth(&input, &options(true)),
            };
            fused = blended && last && made.is_ok();
            let clip = match made {
                Ok(clip) => clip,
                Err(_) => svp.smooth(&input, &options(false))?,
            };
            input = Arc::new(clip.keep(input));
        }
        if dedup.enabled {
            sink(Event::Info("replacing repeated frames".into()));
        }

        // the layer of peel is a 16-bit clip of double height
        let relay = |svp: &Arc<Svp>, source: &Arc<Clip>, scale: usize| {
            let relay = Pick {
                source: Arc::clone(source),
                step: 1,
                offset: 0,
                width: size.0 * scale,
                height: size.1 * scale,
            };
            svp.source(&source.shape(), relay)
        };
        let pass = |svp: &mut Arc<Svp>, input: &Arc<Clip>, fps, blend| -> Result<Clip> {
            let open = |svp: &Arc<Svp>, source: &Arc<Clip>| {
                Rife::open(svp, Arc::clone(source), &meta, fps, rife, blend, sink)
            };
            match mask {
                Some(mask) if mask.method == Masking::Peel => {
                    let options = |gpu| Options::new(interpolation, "", None, None, gpu, half);
                    let layer = svp.layer(input, &options(true), mask.peel);
                    let layer = layer.or_else(|_| svp.layer(input, &options(false), mask.peel))?;
                    let layer = Arc::new(layer);
                    let bare = Arc::new(svp.peel(input, &layer)?);
                    *svp = host()?;
                    let clip = open(svp, &bare)?;
                    svp.unpeel(clip, relay(svp, input, 1)?, relay(svp, &layer, 2)?)
                }
                Some(mask) => {
                    *svp = host()?;
                    let relay = relay(svp, input, 1)?;
                    Ok(svp.still(open(svp, input)?, &relay, mask)?.keep(relay))
                }
                None => {
                    *svp = host()?;
                    open(svp, input)
                }
            }
        };
        if let Some(fps) = pre {
            input = Arc::new(pass(&mut svp, &input, fps, None)?);
            sink(Event::Info(format!("rife to {fps} fps first")));
        }
        let derived = match interpolation.engine {
            _ if !interpolate => None,
            Engine::Rife => {
                let radius = (weights.len() / 2) as i64;
                let blend = blended.then_some((radius, i64::from(target), i64::from(blending.fps)));
                let clip = pass(&mut svp, &input, target, blend)?;
                sink(Event::Info(format!("rife to {target} fps")));
                Some(clip)
            }
            Engine::Svp => {
                let rate = format!("num:{target},den:1,abs:true");
                let limit = limit.filter(|_| skipped);
                let options = |gpu| Options::new(interpolation, &rate, mask, limit, gpu, half);
                let made = match blended {
                    true => svp.smooth_blend(&input, &options(true), weights, blending),
                    false => svp.smooth(&input, &options(true)),
                };
                fused = blended && made.is_ok();
                let clip = match made {
                    Ok(clip) => clip,
                    Err(_) => {
                        sink(Event::Warning("no opencl gpu, rendering on the cpu".into()));
                        svp.smooth(&input, &options(false))?
                    }
                };
                let half = if half { ", half-size analysis" } else { "" };
                sink(Event::Info(format!("svp to {target} fps{half}")));
                Some(clip)
            }
        };
        Ok((Feed::Host { derived, input }, fused))
    }

    pub(crate) fn frame(&self, n: i32) -> Result<frame::Video> {
        let Info { width, height, .. } = self.info;
        let picture = match &self.feed {
            Feed::File(segments) => {
                let mut segments = segments.lock().unwrap_or_else(|poison| poison.into_inner());
                segments.frame(n)?
            }
            Feed::Host { derived, input } => {
                let clip = derived.as_ref().unwrap_or(input);
                let source = |n| Ok(wrap(clip.frame(n)?, width, height));
                match &self.blend {
                    Some(blend) => blend.frame(source, n, width, height)?,
                    None => source(n)?,
                }
            }
        };
        let mut picture = match &self.grade {
            Some(grade) => grade.apply(&picture),
            None => picture,
        };
        self.meta.tag(&mut picture);
        Ok(picture)
    }

    pub(crate) fn encode(&self, spare: Spare, from: i32, end: i32) -> bool {
        let Feed::File(segments) = &self.feed else {
            return false;
        };
        if self.grade.is_some() {
            return false;
        }
        let mut segments = segments.lock().unwrap_or_else(|poison| poison.into_inner());
        segments.encode(spare, from, end);
        true
    }

    pub(crate) fn made(&self, n: i32) -> Result<Made> {
        let Feed::File(segments) = &self.feed else {
            return self.frame(n).map(Made::Picture);
        };
        let mut segments = segments.lock().unwrap_or_else(|poison| poison.into_inner());
        let mut made = segments.take(n)?;
        if let Made::Picture(picture) = &mut made {
            self.meta.tag(picture);
        }
        Ok(made)
    }

    pub(crate) fn lanes(&self) -> Option<(usize, usize)> {
        match &self.feed {
            Feed::File(segments) => {
                let segments = segments.lock().unwrap_or_else(|poison| poison.into_inner());
                Some(segments.lanes())
            }
            Feed::Host { .. } => None,
        }
    }

    pub fn source_fps(&self) -> f64 {
        self.meta.fps.0 as f64 / self.meta.fps.1 as f64
    }

    pub fn preview(&self, n: i32, width: u32) -> Result<Image> {
        let picture = self.frame(n.clamp(0, self.info.frames - 1))?;
        image(&Scaler::new(), &picture, width)
    }
}

pub fn image(scaler: &Scaler, picture: &frame::Video, width: u32) -> Result<Image> {
    let width = width.min(picture.width()) & !1;
    let height =
        (u64::from(width) * u64::from(picture.height()) / u64::from(picture.width())) as u32;
    // the scaler writes past the end of a row, and a frame has the space for that
    let mut scaled = frame::Video::new(Pixel::RGBA, width, height);
    scaler.run(&mut scaled, picture)?;
    let mut rgba = Vec::with_capacity(width as usize * height as usize * 4);
    for row in scaled.data(0).chunks(scaled.stride(0)) {
        rgba.extend_from_slice(&row[..width as usize * 4]);
    }
    Ok(Image {
        width,
        height,
        rgba,
    })
}
