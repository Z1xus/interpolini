use std::ffi::c_void;
use std::path::Path;
use std::sync::{Arc, Mutex};

use ffmpeg_next::format::Pixel;
use ffmpeg_next::{ffi, frame};

use crate::blend::Blend;
use crate::budget;
use crate::color::Grade;
use crate::compose::Scaler;
use crate::config::{self, Config, Engine, Masking};
use crate::decode::{Ahead, Decoder, Meta};
use crate::encode::Spare;
use crate::rife::Rife;
use crate::segments::{Made, Segments};
use crate::svp::{Clip, Frame, Options, Shape, Svp};
use crate::work::{Cancel, Follow};
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
    Plain(Box<Ahead>),
    Host {
        derived: Option<Clip>,
        input: Arc<Clip>,
    },
}

pub struct Graph {
    feed: Feed,
    blend: Option<Blend>,
    grade: Option<Grade>,
    scaler: Scaler,
    follow: Follow,
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
        let pre = Some(pre.fps.at(source_fps)).filter(|fps| {
            pre.enabled
                && f64::from(*fps) > source_fps
                && !(interpolation.enabled && interpolation.engine == Engine::Rife)
        });
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

    fn plain(&self, config: &Config) -> bool {
        !config.dedup.enabled && self.pre.is_none() && !self.interpolate
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

fn wrap(source: Frame, width: u32, height: u32, format: Pixel) -> frame::Video {
    let mut picture = frame::Video::empty();
    unsafe {
        let raw = &mut *picture.as_mut_ptr();
        raw.format = ffi::AVPixelFormat::from(format) as i32;
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

pub fn covering(frames: i32, from: (i64, i64), to: (i64, i64)) -> i32 {
    let (num, den) = (
        i128::from(frames) * i128::from(from.1) * i128::from(to.0),
        i128::from(from.0) * i128::from(to.1),
    );
    ((num + den - 1) / den) as i32
}

impl Graph {
    pub fn open(clip: &Path, config: &Config, sink: Sink, cancel: &Cancel) -> Result<Self> {
        // a blend reads the clip in its own decoders, this one only seeks
        let threads = |fps, height| {
            let plan = Plan::new(fps, config);
            match plan.plain(config) && plan.blended {
                true => 1,
                false => budget::decode(height),
            }
        };
        let follow = Follow::new(cancel);
        let decoder = Decoder::chosen(clip, threads)?
            .following(&follow)
            .shrink(config.shrink);
        cancel.check()?;
        let meta = decoder.meta();
        let Meta {
            width,
            height,
            fps,
            frames,
            codec,
            cropped,
            ..
        } = meta;
        if cropped {
            sink(Event::Warning(format!(
                "the picture has an odd size, it is cropped to {width}x{height}"
            )));
        }
        let source_fps = fps.0 as f64 / fps.1 as f64;
        sink(Event::Info(format!(
            "{codec}  |  {width}x{height}  |  {source_fps:.2} fps  |  {frames} frames"
        )));

        let plan = Plan::new(source_fps, config);
        let plain = plan.plain(config);
        let Plan {
            blended, weights, ..
        } = &plan;
        let blending = &config.blending;
        let (feed, fused) = match (plain, *blended) {
            (true, true) => {
                let segments = Segments::new(clip, decoder, weights, blending)?;
                (Feed::File(Box::new(Mutex::new(segments))), false)
            }
            (true, false) => (Feed::Plain(Box::new(Ahead::new(decoder))), false),
            (false, _) => {
                let decoder = match Decoder::gpu(clip) {
                    Some(gpu) => {
                        sink(Event::Info("decoding on the gpu".into()));
                        gpu.following(&follow).shrink(config.shrink)
                    }
                    None => decoder,
                };
                Self::host(decoder, config, &plan, sink, cancel)?
            }
        };
        let blend = match &feed {
            Feed::Host { derived, input } if *blended && !fused => {
                let shape = derived.as_ref().unwrap_or(input).shape();
                let frames = covering(frames, fps, shape.fps);
                Some(Blend::new(weights, shape.fps, blending, frames))
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
                (shape.fps, covering(frames, fps, shape.fps))
            }
            (Feed::Plain(_), None) => (fps, frames),
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
            scaler: Scaler::new(),
            follow,
            meta,
            info,
        })
    }

    fn host(
        decoder: Decoder,
        config: &Config,
        plan: &Plan,
        sink: Sink,
        cancel: &Cancel,
    ) -> Result<(Feed, bool)> {
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

        cancel.check()?;
        let svp = Svp::load(&config::app_dir())?;
        let shape = Shape {
            deep: false,
            yuv444: meta.yuv444,
            width,
            height,
            fps,
            frames,
        };
        let mut input = Arc::new(svp.source(&shape, Ahead::new(decoder))?);

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
                false => Err("no blend".into()),
            };
            fused = made.is_ok();
            // the blend on the gpu takes fewer formats than the gpu itself
            let made = made.or_else(|_| svp.smooth(&input, &options(true)));
            let clip = match made {
                Ok(clip) => clip,
                Err(_) => svp.smooth(&input, &options(false))?,
            };
            input = Arc::new(clip.keep(input));
        }
        if dedup.enabled {
            sink(Event::Info("replacing repeated frames".into()));
        }

        let pass = |input: &Arc<Clip>, fps, blend| -> Result<Clip> {
            let open = |source: &Arc<Clip>| {
                Rife::open(
                    &svp,
                    Arc::clone(source),
                    &meta,
                    (fps, rife, blend),
                    sink,
                    cancel,
                )
            };
            match mask {
                Some(mask) if mask.method == Masking::Peel => {
                    let options = |gpu| Options::new(interpolation, "", None, None, gpu, half);
                    let layer = svp.layer(input, &options(true), mask.peel);
                    let layer = layer.or_else(|_| svp.layer(input, &options(false), mask.peel))?;
                    let layer = Arc::new(layer);
                    let bare = Arc::new(svp.peel(input, &layer)?);
                    svp.unpeel(open(&bare)?, input, &layer)
                }
                Some(mask) => svp.still(open(input)?, input, mask),
                None => open(input),
            }
        };
        if let Some(fps) = pre {
            cancel.check()?;
            input = Arc::new(pass(&input, fps, None)?);
            sink(Event::Info(format!("rife to {fps} fps first")));
        }
        cancel.check()?;
        let derived = match interpolation.engine {
            _ if !interpolate => None,
            Engine::Rife => {
                let radius = (weights.len() / 2) as i64;
                let blend = blended.then_some((radius, i64::from(target), i64::from(blending.fps)));
                let clip = pass(&input, target, blend)?;
                sink(Event::Info(format!("rife to {target} fps")));
                Some(clip)
            }
            Engine::Svp => {
                let rate = format!("num:{target},den:1,abs:true");
                let limit = limit.filter(|_| skipped);
                let options = |gpu| Options::new(interpolation, &rate, mask, limit, gpu, half);
                let made = match blended {
                    true => svp.smooth_blend(&input, &options(true), weights, blending),
                    false => Err("no blend".into()),
                };
                fused = made.is_ok();
                let made = made.or_else(|_| svp.smooth(&input, &options(true)));
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

    pub fn stop(&self, seconds: f64) {
        if let Feed::Host { derived, input } = &self.feed {
            let clip = derived.as_ref().unwrap_or(input);
            let (num, den) = clip.shape().fps;
            clip.end((seconds * num as f64 / den as f64).ceil() as i32);
        }
    }

    pub(crate) fn frame(&self, n: i32, cancel: &Cancel) -> Result<frame::Video> {
        self.follow.set(cancel);
        let Info { width, height, .. } = self.info;
        let picture = match &self.feed {
            Feed::File(segments) => {
                let mut segments = segments.lock().unwrap_or_else(|poison| poison.into_inner());
                segments.frame(n, cancel)?
            }
            Feed::Plain(decoder) => decoder.picture(n)?,
            Feed::Host { derived, input } => {
                let clip = derived.as_ref().unwrap_or(input);
                let last = clip.shape().frames - 1;
                let source = |n: i32| {
                    Ok(wrap(
                        clip.frame(n.min(last))?,
                        width,
                        height,
                        self.meta.pixel(),
                    ))
                };
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

    pub(crate) fn packs(&self) -> bool {
        matches!(self.feed, Feed::File(_)) && self.grade.is_none()
    }

    pub(crate) fn encode(&self, spare: Spare, from: i32, end: i32) -> bool {
        let Feed::File(segments) = &self.feed else {
            return false;
        };
        let mut segments = segments.lock().unwrap_or_else(|poison| poison.into_inner());
        segments.encode(spare, from, end);
        true
    }

    pub(crate) fn made(&self, n: i32, cancel: &Cancel) -> Result<Made> {
        let Feed::File(segments) = &self.feed else {
            return self.frame(n, cancel).map(Made::Picture);
        };
        let mut segments = segments.lock().unwrap_or_else(|poison| poison.into_inner());
        let mut made = segments.take(n, cancel)?;
        if let Made::Picture(picture) = &mut made {
            self.meta.tag(picture);
        }
        Ok(made)
    }

    pub(crate) fn lanes(&self) -> Option<usize> {
        match &self.feed {
            Feed::File(segments) => {
                let segments = segments.lock().unwrap_or_else(|poison| poison.into_inner());
                Some(segments.lanes())
            }
            Feed::Plain(_) | Feed::Host { .. } => None,
        }
    }

    pub fn source_fps(&self) -> f64 {
        self.meta.fps.0 as f64 / self.meta.fps.1 as f64
    }

    pub fn preview(&self, n: i32, width: u32, cancel: &Cancel) -> Result<Image> {
        let picture = self.frame(n.clamp(0, self.info.frames - 1), cancel)?;
        image(&self.scaler, &picture, width)
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
