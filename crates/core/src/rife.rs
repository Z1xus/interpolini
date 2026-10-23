use std::ffi::{CStr, CString, c_char};
use std::mem::ManuallyDrop;
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};

use libloading::Library;

use crate::config::{self, Backend};
use crate::decode::Meta;
use crate::svp::{Clip, Frame, Map, Raw, Shape, Svp};
use crate::tensorrt::{Support, support};
use crate::work::Cancel;
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
    prepare: b"rife_prepare\0" fn(Raw, i32, i32, i32, i32, i32, Report, Raw) -> i32;
    process: b"rife_process\0" fn(Raw, *const Picture, *const Picture, f32, *const Picture) -> i32;
    destroy: b"rife_destroy\0" fn(Raw);
    error: b"rife_error\0" fn() -> *const c_char;
}

type Report = unsafe extern "C" fn(Raw, f32);

// the plugin tells how far the build of a tensorrt engine is, and the sink shows it
unsafe extern "C" fn report(sink: Raw, done: f32) {
    {
        let mut build = locked(&PREPARE.0);
        build.done = done;
    }
    PREPARE.1.notify_all();
    let sink = unsafe { *sink.cast::<Sink>() };
    sink(Event::Preparing(done));
}

// the libraries keep gpu state, so they stay loaded
static VULKAN: OnceLock<std::result::Result<Api, String>> = OnceLock::new();
static TENSORRT: OnceLock<std::result::Result<Api, String>> = OnceLock::new();

struct Build {
    busy: bool,
    // below 0 when no engine builds
    done: f32,
}

// one tensorrt engine builds at a time, and the graphs that wait for it show how far it is
static PREPARE: (Mutex<Build>, Condvar) = (
    Mutex::new(Build {
        busy: false,
        done: -1.0,
    }),
    Condvar::new(),
);

fn locked(build: &Mutex<Build>) -> MutexGuard<'_, Build> {
    build.lock().unwrap_or_else(|poison| poison.into_inner())
}

struct Building;

impl Drop for Building {
    fn drop(&mut self) {
        *locked(&PREPARE.0) = Build {
            busy: false,
            done: -1.0,
        };
        PREPARE.1.notify_all();
    }
}

fn wait(sink: Sink, cancel: &Cancel) -> Result<Building> {
    let _wake = cancel.wake(|| {
        let _build = locked(&PREPARE.0);
        PREPARE.1.notify_all();
    });
    let mut build = locked(&PREPARE.0);
    loop {
        cancel.check()?;
        if !build.busy {
            build.busy = true;
            return Ok(Building);
        }
        let done = build.done;
        drop(build);
        if done >= 0.0 {
            sink(Event::Preparing(done));
        }
        build = locked(&PREPARE.0);
        if build.busy && build.done == done {
            build = PREPARE
                .1
                .wait(build)
                .unwrap_or_else(|poison| poison.into_inner());
        }
    }
}

fn last_error(api: &Api) -> String {
    unsafe { CStr::from_ptr((api.error)()) }
        .to_string_lossy()
        .into_owned()
}

struct Engine {
    api: &'static Api,
    raw: Raw,
    models: PathBuf,
}

unsafe impl Send for Engine {}

impl Drop for Engine {
    fn drop(&mut self) {
        unsafe { (self.api.destroy)(self.raw) };
    }
}

// engines of closed clips, for the next clip with the same model
static IDLE: Mutex<Vec<Engine>> = Mutex::new(Vec::new());

fn idle() -> MutexGuard<'static, Vec<Engine>> {
    IDLE.lock().unwrap_or_else(|poison| poison.into_inner())
}

pub fn shutdown() {
    idle().clear();
    for api in [&VULKAN, &TENSORRT] {
        if let Some(Ok(api)) = api.get()
            && let Ok(call) = unsafe {
                api._library
                    .get::<unsafe extern "C" fn()>(b"rife_shutdown\0")
            }
        {
            unsafe { call() };
        }
    }
}

pub struct Rife {
    engine: ManuallyDrop<Engine>,
    step: (i64, i64),
    // the radius of the blend that reads this clip, and the frame rates around it
    blend: Option<(i64, i64, i64)>,
    last: i32,
    width: usize,
    height: usize,
    chroma: usize,
}

// the plugin is thread safe
unsafe impl Send for Rife {}
unsafe impl Sync for Rife {}

impl Drop for Rife {
    fn drop(&mut self) {
        idle().push(unsafe { ManuallyDrop::take(&mut self.engine) });
    }
}

impl Rife {
    pub fn open(
        svp: &Arc<Svp>,
        source: Arc<Clip>,
        meta: &Meta,
        (fps, rife, blend): (u32, &config::Rife, Option<(i64, i64, i64)>),
        sink: crate::Sink,
        cancel: &Cancel,
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
            let library = unsafe { Library::new(&path) };
            let library = library.map_err(|error| format!("{}: {error}", path.display()))?;
            Api::load(library).map_err(|error| format!("{}: {error}", path.display()))
        });
        let api = api
            .as_ref()
            .map_err(|error| format!("cannot load rife, {error}"))?;
        let mut idle = idle();
        let kept = idle
            .iter()
            .position(|engine| std::ptr::eq(engine.api, api) && engine.models == models);
        let kept = kept.map(|index| idle.swap_remove(index));
        // another model frees the gpu first
        if kept.is_none() {
            idle.clear();
        }
        drop(idle);
        let engine = match kept {
            Some(engine) => engine,
            None => {
                let path = CString::new(models.to_string_lossy().as_bytes())
                    .map_err(|error| error.to_string())?;
                let raw = unsafe { (api.create)(path.as_ptr()) };
                if raw.is_null() {
                    return Err(last_error(api).into());
                }
                Engine { api, raw, models }
            }
        };

        let Meta { width, height, .. } = *meta;
        // its a looooong one
        let user = std::ptr::from_ref(&sink).cast_mut().cast();
        let [bt601, full, yuv444] = [meta.bt601(), meta.full(), meta.yuv444].map(i32::from);
        let (width, height) = (width as i32, height as i32);
        let guard = tensorrt.then(|| wait(sink, cancel)).transpose()?;
        let raw = engine.raw;
        let ready = unsafe { (api.prepare)(raw, width, height, bt601, full, yuv444, report, user) };
        drop(guard);
        if ready != 0 {
            drop(engine);
            let error = last_error(api);
            return Err(format!("tensorrt could not prepare the model, {error}").into());
        }
        let from = source.shape();
        let step = (from.fps.0, from.fps.1 * i64::from(fps));
        let shape = Shape {
            deep: false,
            yuv444: meta.yuv444,
            width: width as u32,
            height: height as u32,
            fps: (i64::from(fps), 1),
            frames: (i64::from(from.frames) * step.1 / step.0) as i32,
        };
        let rife = Self {
            engine: ManuallyDrop::new(engine),
            step,
            blend,
            last: from.frames - 1,
            width: width as usize,
            height: height as usize,
            chroma: meta.shift(),
        };
        svp.map(&shape, source, rife)
    }

    fn place(&self, n: i32) -> (i32, f32) {
        let position = i64::from(n) * self.step.0;
        let index = ((position / self.step.1) as i32).min(self.last);
        let time = (position % self.step.1) as f32 / self.step.1 as f32;
        (index, time)
    }

    fn made(&self, n: i32) -> bool {
        let (index, time) = self.place(n);
        let Some((radius, from, to)) = self.blend else {
            return time != 0.0 && index != self.last;
        };
        let n = i64::from(n);
        let nearest = n * to / from;
        let used =
            (nearest - 1..=nearest + 1).any(|output| (n - output * from / to).abs() <= radius);
        time != 0.0 && index != self.last && used
    }
}

impl Map for Rife {
    fn need(&self, n: i32) -> Vec<i32> {
        let index = self.place(n).0;
        match self.made(n) {
            true => vec![index, index + 1],
            false => vec![index],
        }
    }

    fn render(
        &self,
        n: i32,
        inputs: &[&Frame],
        planes: [*mut u8; 3],
        strides: [isize; 3],
    ) -> Result<()> {
        let (index, time) = self.place(n);
        let [first, second] = inputs else {
            let frame = inputs.first().ok_or("no source frame")?;
            frame.copy(planes, strides, self.width, self.height, self.chroma);
            return Ok(());
        };
        let first = Picture::of(first, index);
        let second = Picture::of(second, index + 1);
        let output = Picture {
            data: planes,
            stride: strides,
            id: -1,
        };
        let Engine { api, raw, .. } = *self.engine;
        let code = unsafe { (api.process)(raw, &first, &second, time, &output) };
        if code != 0 {
            return Err(last_error(api).into());
        }
        Ok(())
    }
}
