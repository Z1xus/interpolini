use std::ffi::{CStr, CString, c_char};
use std::sync::{Arc, OnceLock};

use libloading::Library;

use crate::config::{self, Backend};
use crate::decode::Meta;
use crate::svp::{Clip, Frame, Raw, Reader, Shape, Svp};
use crate::tensorrt::{Support, support};
use crate::{Event, Result, Sink};

#[repr(C)]
struct Picture {
    data: [*mut u8; 3],
    stride: [isize; 3],
    id: i32,
}

impl Picture {
    fn of(frame: &Frame, id: i32) -> Self {
        Self {
            data: std::array::from_fn(|plane| frame.data(plane).cast_mut()),
            stride: std::array::from_fn(|plane| frame.stride(plane) as isize),
            id,
        }
    }
}

api! {
    create: b"rife_create\0" fn(*const c_char) -> Raw;
    prepare: b"rife_prepare\0" fn(Raw, i32, i32, i32, i32, Report, Raw) -> i32;
    process: b"rife_process\0" fn(Raw, *const Picture, *const Picture, f32, *const Picture) -> i32;
    destroy: b"rife_destroy\0" fn(Raw);
    error: b"rife_error\0" fn() -> *const c_char;
}

type Report = unsafe extern "C" fn(Raw, f32);

// the plugin tells how far the build of a tensorrt engine is, and the sink shows it
unsafe extern "C" fn report(sink: Raw, done: f32) {
    let sink = unsafe { *sink.cast::<Sink>() };
    sink(Event::Preparing(done));
}

// the libraries keep gpu state, so they stay loaded
static VULKAN: OnceLock<Option<Api>> = OnceLock::new();
static TENSORRT: OnceLock<Option<Api>> = OnceLock::new();

pub struct Rife {
    api: &'static Api,
    raw: Raw,
    source: Arc<Clip>,
    step: (i64, i64),
    // the radius of the blend that reads this clip, and the frame rates around it
    blend: Option<(i64, i64, i64)>,
    last: i32,
    width: usize,
    height: usize,
}

// the plugin is thread safe
unsafe impl Send for Rife {}
unsafe impl Sync for Rife {}

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
        blend: Option<(i64, i64, i64)>,
        sink: crate::Sink,
    ) -> Result<Clip> {
        let directory = config::app_dir();
        let mut backend = rife.backend;
        let support = support();
        if backend == Backend::Tensorrt && support != Support::Ready {
            sink(Event::Warning(format!("{} Using vulkan.", support.need())));
            backend = Backend::Vulkan;
        }
        let (model, tensorrt) = (&rife.model, backend == Backend::Tensorrt);
        let models = directory.join("rife").join(model);
        if !models.join(backend.model()).exists() {
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
        let (bt601, full) = (i32::from(meta.bt601()), i32::from(meta.full()));
        let ready =
            unsafe { (api.prepare)(raw, width as i32, height as i32, bt601, full, report, user) };
        if ready != 0 {
            unsafe { (api.destroy)(raw) };
            let error = unsafe { CStr::from_ptr((api.error)()) }.to_string_lossy();
            if !error.is_empty() {
                sink(Event::Warning(format!("tensorrt: {error}")));
            }
            return Err("tensorrt could not prepare the model".into());
        }
        let from = source.shape();
        let step = (from.fps.0, from.fps.1 * i64::from(fps));
        let shape = Shape {
            deep: false,
            width,
            height,
            fps: (i64::from(fps), 1),
            frames: (i64::from(from.frames) * step.1 / step.0) as i32,
        };
        let rife = Self {
            api,
            raw,
            source,
            step,
            blend,
            last: from.frames - 1,
            width: width as usize,
            height: height as usize,
        };
        svp.source(&shape, rife)
    }

    fn used(&self, n: i32) -> bool {
        let Some((radius, from, to)) = self.blend else {
            return true;
        };
        let n = i64::from(n);
        let nearest = n * to / from;
        (nearest - 1..=nearest + 1).any(|output| (n - output * from / to).abs() <= radius)
    }
}

impl Reader for Rife {
    const PARALLEL: bool = true;

    fn read(&self, n: i32, planes: [*mut u8; 3], strides: [isize; 3]) -> Result<()> {
        let position = i64::from(n) * self.step.0;
        let index = ((position / self.step.1) as i32).min(self.last);
        let time = (position % self.step.1) as f32 / self.step.1 as f32;
        if time == 0.0 || index == self.last || !self.used(n) {
            // a source frame goes through as it is
            let frame = self.source.frame(index)?;
            frame.copy(planes, strides, self.width, self.height);
            return Ok(());
        }
        let frames = (self.source.frame(index)?, self.source.frame(index + 1)?);
        let first = Picture::of(&frames.0, index);
        let second = Picture::of(&frames.1, index + 1);
        let output = Picture {
            data: planes,
            stride: strides,
            id: -1,
        };
        let code = unsafe { (self.api.process)(self.raw, &first, &second, time, &output) };
        if code != 0 {
            return Err("rife could not render the frame".into());
        }
        Ok(())
    }
}
