use std::ffi::{CString, c_char};
use std::sync::{Arc, OnceLock};

use ffmpeg_next::ffi;
use ffmpeg_next::format::Pixel;
use ffmpeg_next::frame;
use libloading::Library;

use crate::compose::Scaler;
use crate::decode::Meta;
use crate::svp::{Clip, Raw, Reader, Shape, Svp};
use crate::{Result, Sink, config};

api! {
    create: b"rife_create\0" fn(*const c_char) -> Raw;
    prepare: b"rife_prepare\0" fn(Raw, i32, i32, Report, Raw) -> i32;
    process: b"rife_process\0" fn(Raw, *const u8, *const u8, i32, i32, f32, *mut u8) -> i32;
    destroy: b"rife_destroy\0" fn(Raw);
}

type Report = unsafe extern "C" fn(Raw, f32);

// the plugin tells how far the build of a tensorrt engine is, and the sink shows it
unsafe extern "C" fn report(sink: Raw, done: f32) {
    let sink = unsafe { *sink.cast::<Sink>() };
    let text = format!("Preparing TensorRT, {:.0}%", done * 100.0);
    sink(crate::Event::Warning(text));
}

// the libraries keep gpu state, so they stay loaded
static VULKAN: OnceLock<Option<Api>> = OnceLock::new();
static TENSORRT: OnceLock<Option<Api>> = OnceLock::new();

pub struct Rife {
    api: &'static Api,
    raw: Raw,
    source: Arc<Clip>,
    meta: Meta,
    to_rgb: Scaler,
    to_yuv: Scaler,
    frames: [(i32, Vec<u8>); 2],
    output: Vec<u8>,
    step: (i64, i64),
    last: i32,
    width: usize,
    height: usize,
}

unsafe impl Send for Rife {}

impl Drop for Rife {
    fn drop(&mut self) {
        unsafe { (self.api.destroy)(self.raw) };
    }
}

impl Rife {
    pub fn open(
        svp: &Arc<Svp>,
        source: Arc<Clip>,
        meta: &Meta,
        fps: u32,
        rife: &config::Rife,
        sink: crate::Sink,
    ) -> Result<Clip> {
        let directory = config::app_dir();
        let (model, tensorrt) = (&rife.model, rife.backend == config::Backend::Tensorrt);
        let models = directory.join("rife").join(model);
        if !models.join(rife.backend.model()).exists() {
            return Err(format!("there is no rife model \"{model}\"").into());
        }
        let (api, name) = match tensorrt {
            true => (&TENSORRT, "interpolini_rife_trt"),
            false => (&VULKAN, "interpolini_rife"),
        };
        // macos has no vulkan of its own, so ncnn gets the driver that comes with the app
        let driver = directory.join("libMoltenVK.dylib");
        if cfg!(target_os = "macos") && driver.exists() {
            unsafe { std::env::set_var("NCNN_VULKAN_DRIVER", driver) };
        }
        let api = api.get_or_init(|| {
            let path = directory.join(libloading::library_filename(name));
            Api::load(unsafe { Library::new(path) }.ok()?).ok()
        });
        let api = api.as_ref().ok_or("rife is not installed")?;
        let path =
            CString::new(models.to_string_lossy().as_bytes()).map_err(|error| error.to_string())?;
        let raw = unsafe { (api.create)(path.as_ptr()) };
        if raw.is_null() {
            return Err(match tensorrt {
                true => "tensorrt could not start",
                false => "rife needs a vulkan gpu",
            }
            .into());
        }

        let Meta { width, height, .. } = *meta;
        // its a looooong one
        let user = std::ptr::from_ref(&sink).cast_mut().cast();
        let ready = unsafe { (api.prepare)(raw, width as i32, height as i32, report, user) };
        if ready != 0 {
            unsafe { (api.destroy)(raw) };
            return Err("tensorrt could not make an engine for this size".into());
        }
        let size = width as usize * height as usize * 3;
        let from = source.shape();
        let step = (from.fps.0, from.fps.1 * i64::from(fps));
        let shape = Shape {
            width,
            height,
            fps: (i64::from(fps), 1),
            frames: (i64::from(from.frames) * step.1 / step.0) as i32,
        };
        let rife = Self {
            api,
            raw,
            source,
            meta: *meta,
            to_rgb: Scaler::new(),
            to_yuv: Scaler::new(),
            frames: [(-1, vec![0; size]), (-1, vec![0; size])],
            output: vec![0; size],
            step,
            last: from.frames - 1,
            width: width as usize,
            height: height as usize,
        };
        svp.source(&shape, rife)
    }

    fn wrap(&self, format: Pixel, planes: [*mut u8; 3], strides: [i32; 3]) -> frame::Video {
        let mut wrapped = frame::Video::empty();
        let raw = unsafe { &mut *wrapped.as_mut_ptr() };
        raw.format = ffi::AVPixelFormat::from(format) as i32;
        (raw.width, raw.height) = (self.width as i32, self.height as i32);
        raw.data[..3].copy_from_slice(&planes);
        raw.linesize[..3].copy_from_slice(&strides);
        match format {
            Pixel::RGB24 => {
                raw.colorspace = ffi::AVColorSpace::AVCOL_SPC_RGB;
                raw.color_range = ffi::AVColorRange::AVCOL_RANGE_JPEG;
            }
            _ => self.meta.tag(&mut wrapped),
        }
        wrapped
    }

    fn rgb(&self, pixels: *const u8) -> frame::Video {
        let planes = [
            pixels.cast_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        ];
        self.wrap(Pixel::RGB24, planes, [self.width as i32 * 3, 0, 0])
    }

    fn load(&mut self, index: i32) -> Result<()> {
        if self.frames[index as usize % 2].0 == index {
            return Ok(());
        }
        let frame = self.source.frame(index)?;
        let planes = std::array::from_fn(|plane| frame.data(plane).cast_mut());
        let strides = std::array::from_fn(|plane| frame.stride(plane) as i32);
        let from = self.wrap(Pixel::YUV420P, planes, strides);
        let slot = &mut self.frames[index as usize % 2];
        slot.0 = index;
        let mut to = self.rgb(self.frames[index as usize % 2].1.as_ptr());
        self.to_rgb.run(&mut to, &from)
    }
}

impl Reader for Rife {
    fn read(&mut self, n: i32, planes: [*mut u8; 3], strides: [isize; 3]) -> Result<()> {
        let position = i64::from(n) * self.step.0;
        let index = ((position / self.step.1) as i32).min(self.last);
        let time = (position % self.step.1) as f32 / self.step.1 as f32;
        if time == 0.0 || index == self.last {
            // a source frame goes through as it is
            let frame = self.source.frame(index)?;
            frame.copy(planes, strides, self.width, self.height);
            return Ok(());
        }
        self.load(index)?;
        self.load(index + 1)?;
        let [first, second] = &self.frames;
        let (first, second) = if index % 2 == 0 {
            (first, second)
        } else {
            (second, first)
        };
        let (width, height) = (self.width as i32, self.height as i32);
        let output = self.output.as_mut_ptr();
        let code = unsafe {
            (self.api.process)(
                self.raw,
                first.1.as_ptr(),
                second.1.as_ptr(),
                width,
                height,
                time,
                output,
            )
        };
        if code != 0 {
            return Err("rife could not render the frame".into());
        }
        let strides = strides.map(|stride| stride as i32);
        let mut to = self.wrap(Pixel::YUV420P, planes, strides);
        self.to_yuv.run(&mut to, &self.rgb(self.output.as_ptr()))
    }
}
