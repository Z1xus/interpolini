use std::ffi::{CStr, CString, c_char, c_void};
use std::mem::ManuallyDrop;
use std::path::Path;
use std::sync::{Arc, OnceLock, RwLock};

use libloading::Library;

use crate::config::{Algorithm, Blending, Interpolation, Mask, Masking, Speed, Tuning};
use crate::{Error, Result};

const YUV420P8: i32 = 0;
const YUV444P8: i32 = 1;
const YUV420P16: i32 = 3;
const YUV444P16: i32 = 4;

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct VideoInfo {
    format: i32,
    width: i32,
    height: i32,
    fps_num: i64,
    fps_den: i64,
    num_frames: i32,
}

pub type Raw = *mut c_void;
type Text = *const c_char;
type ReadFn = unsafe extern "C" fn(Raw, i32, *const *mut u8, *const isize) -> i32;
type ReleaseFn = unsafe extern "C" fn(Raw);
type NeedFn = unsafe extern "C" fn(Raw, i32, *mut i32) -> i32;
type RenderFn =
    unsafe extern "C" fn(Raw, i32, *const Raw, i32, *const *mut u8, *const isize) -> i32;

// the most input frames that a map reads for one frame
const NEEDED: usize = 8;

api! {
    error: b"osvp_error\0" fn() -> Text;
    create: b"osvp_create\0" fn(Text) -> Raw;
    destroy: b"osvp_destroy\0" fn(Raw);
    source: b"osvp_source\0" fn(Raw, *const VideoInfo, ReadFn, ReleaseFn, Raw) -> Raw;
    map: b"osvp_map\0" fn(Raw, *const VideoInfo, Raw, NeedFn, RenderFn, ReleaseFn, Raw) -> Raw;
    smooth_fps: b"osvp_smooth_fps\0" fn(Raw, Raw, Text, Text, Text, i32) -> Raw;
    smooth_fps_blend: b"osvp_smooth_fps_blend\0"
        fn(Raw, Raw, Text, Text, Text, i32, *const f64, i32, i64, i64, f64) -> Raw;
    still: b"osvp_still\0" fn(Raw, Raw, Raw, f64, f64, f64) -> Raw;
    layer: b"osvp_layer\0" fn(Raw, Raw, Text, Text, i32, f64) -> Raw;
    peel: b"osvp_peel\0" fn(Raw, Raw, Raw) -> Raw;
    unpeel: b"osvp_unpeel\0" fn(Raw, Raw, Raw, Raw) -> Raw;
    clip_info: b"osvp_clip_info\0" fn(Raw, *mut VideoInfo);
    clip_free: b"osvp_clip_free\0" fn(Raw);
    clip_set_end: b"osvp_clip_set_end\0" fn(Raw, i32);
    get_frame: b"osvp_get_frame\0" fn(Raw, i32) -> Raw;
    frame_data: b"osvp_frame_data\0" fn(Raw, i32) -> *const u8;
    frame_stride: b"osvp_frame_stride\0" fn(Raw, i32) -> isize;
    frame_free: b"osvp_frame_free\0" fn(Raw);
}

pub struct Svp {
    api: &'static Api,
    context: Raw,
}

unsafe impl Send for Svp {}
unsafe impl Sync for Svp {}

impl Drop for Svp {
    fn drop(&mut self) {
        unsafe { (self.api.destroy)(self.context) };
    }
}

pub trait Reader: Send + Sync + 'static {
    fn read(&self, n: i32, planes: [*mut u8; 3], strides: [isize; 3]) -> Result<()>;
}

pub trait Map: Send + Sync + 'static {
    fn need(&self, n: i32) -> Vec<i32>;

    fn render(
        &self,
        n: i32,
        inputs: &[&Frame],
        planes: [*mut u8; 3],
        strides: [isize; 3],
    ) -> Result<()>;
}

type Cell<T> = RwLock<Option<(&'static Api, T)>>;

fn run<T>(user: Raw, call: impl FnOnce(&'static Api, &T) -> Result<()>) -> i32 {
    let cell = unsafe { &*user.cast::<Cell<T>>() };
    let value = cell.read().unwrap_or_else(|poison| poison.into_inner());
    let done = value
        .as_ref()
        .is_some_and(|(api, value)| call(api, value).is_ok());
    i32::from(!done)
}

unsafe extern "C" fn read<R: Reader>(
    user: Raw,
    n: i32,
    planes: *const *mut u8,
    strides: *const isize,
) -> i32 {
    let (planes, strides) = unsafe { (*planes.cast(), *strides.cast()) };
    run(user, |_, reader: &R| reader.read(n, planes, strides))
}

unsafe extern "C" fn need<M: Map>(user: Raw, n: i32, frames: *mut i32) -> i32 {
    let cell = unsafe { &*user.cast::<Cell<M>>() };
    let value = cell.read().unwrap_or_else(|poison| poison.into_inner());
    let needed = value
        .as_ref()
        .map(|(_, map)| map.need(n))
        .unwrap_or_default();
    let count = needed.len().min(NEEDED);
    unsafe { std::ptr::copy_nonoverlapping(needed.as_ptr(), frames, count) };
    count as i32
}

unsafe extern "C" fn render<M: Map>(
    user: Raw,
    n: i32,
    inputs: *const Raw,
    count: i32,
    planes: *const *mut u8,
    strides: *const isize,
) -> i32 {
    let inputs = unsafe { std::slice::from_raw_parts(inputs, count as usize) };
    let (planes, strides) = unsafe { (*planes.cast(), *strides.cast()) };
    run(user, |api, map: &M| {
        // lent by the host
        let inputs: Vec<_> = inputs
            .iter()
            .map(|&raw| ManuallyDrop::new(Frame { api, raw }))
            .collect();
        let inputs: Vec<&Frame> = inputs.iter().map(|frame| &**frame).collect();
        map.render(n, &inputs, planes, strides)
    })
}

unsafe extern "C" fn release<T>(user: Raw) {
    drop(unsafe { Box::from_raw(user.cast::<Cell<T>>()) });
}

pub struct Clip {
    svp: Arc<Svp>,
    raw: Raw,
    close: Option<Box<dyn Fn() + Send + Sync>>,
    parents: Vec<Arc<Clip>>,
}

unsafe impl Send for Clip {}
unsafe impl Sync for Clip {}

impl Drop for Clip {
    fn drop(&mut self) {
        // the reader goes now, and the host frees its cell when it is done with the clip
        if let Some(close) = &self.close {
            close();
        }
        unsafe { (self.svp.api.clip_free)(self.raw) };
    }
}

pub struct Frame {
    api: &'static Api,
    raw: Raw,
}

unsafe impl Send for Frame {}
unsafe impl Sync for Frame {}

impl Drop for Frame {
    fn drop(&mut self) {
        unsafe { (self.api.frame_free)(self.raw) };
    }
}

impl Frame {
    pub fn data(&self, plane: usize) -> *const u8 {
        unsafe { (self.api.frame_data)(self.raw, plane as i32) }
    }

    pub fn stride(&self, plane: usize) -> usize {
        unsafe { (self.api.frame_stride)(self.raw, plane as i32) as usize }
    }

    pub fn copy(
        &self,
        planes: [*mut u8; 3],
        strides: [isize; 3],
        width: usize,
        height: usize,
        chroma: usize,
    ) {
        for plane in 0..3 {
            let shift = if plane > 0 { chroma } else { 0 };
            for y in 0..height >> shift {
                unsafe {
                    std::ptr::copy_nonoverlapping(
                        self.data(plane).add(y * self.stride(plane)),
                        planes[plane].offset(y as isize * strides[plane]),
                        width >> shift,
                    );
                }
            }
        }
    }
}

pub struct Shape {
    pub deep: bool,
    pub yuv444: bool,
    pub width: u32,
    pub height: u32,
    pub fps: (i64, i64),
    pub frames: i32,
}

impl Shape {
    fn info(&self) -> VideoInfo {
        VideoInfo {
            format: match (self.deep, self.yuv444) {
                (false, false) => YUV420P8,
                (false, true) => YUV444P8,
                (true, false) => YUV420P16,
                (true, true) => YUV444P16,
            },
            width: self.width as i32,
            height: self.height as i32,
            fps_num: self.fps.0,
            fps_den: self.fps.1,
            num_frames: self.frames,
        }
    }
}

impl Svp {
    pub fn load(directory: &Path) -> Result<Arc<Self>> {
        // stays loaded, the threads of a host outlive it
        static API: OnceLock<std::result::Result<Api, String>> = OnceLock::new();
        let api = API.get_or_init(|| {
            let path = directory.join(libloading::library_filename("open_svpflow"));
            let library = unsafe { Library::new(&path) }
                .map_err(|error| format!("cannot load {}: {error}", path.display()))?;
            Api::load(library).map_err(|error| error.to_string())
        });
        let api = api.as_ref().map_err(Clone::clone)?;
        let directory = CString::new(directory.to_string_lossy().as_bytes())
            .map_err(|error| error.to_string())?;
        let context = unsafe { (api.create)(directory.as_ptr()) };
        if context.is_null() {
            return Err(unsafe { error(api) });
        }
        Ok(Arc::new(Self { api, context }))
    }

    fn clip(self: &Arc<Self>, raw: Raw) -> Result<Clip> {
        if raw.is_null() {
            return Err(unsafe { error(self.api) });
        }
        Ok(Clip {
            svp: Arc::clone(self),
            raw,
            close: None,
            parents: Vec::new(),
        })
    }

    // the host frees the cell with the clip, but not when it makes no clip
    fn attach<T: Send + Sync + 'static>(
        self: &Arc<Self>,
        value: T,
        make: impl FnOnce(Raw) -> Raw,
    ) -> Result<Clip> {
        let cell = Box::into_raw(Box::new(RwLock::new(Some((self.api, value))))) as usize;
        let raw = make(cell as Raw);
        let mut clip = self
            .clip(raw)
            .inspect_err(|_| unsafe { release::<T>(cell as Raw) })?;
        clip.close = Some(Box::new(move || {
            let cell = unsafe { &*(cell as *const Cell<T>) };
            drop(cell.write().map(|mut value| value.take()));
        }));
        Ok(clip)
    }

    pub fn source<R: Reader>(self: &Arc<Self>, shape: &Shape, reader: R) -> Result<Clip> {
        let info = shape.info();
        self.attach(reader, |user| unsafe {
            (self.api.source)(self.context, &raw const info, read::<R>, release::<R>, user)
        })
    }

    pub fn map<M: Map>(self: &Arc<Self>, shape: &Shape, input: Arc<Clip>, map: M) -> Result<Clip> {
        let info = shape.info();
        let (need, render, release) = (need::<M>, render::<M>, release::<M>);
        let clip = self.attach(map, |user| unsafe {
            (self.api.map)(
                self.context,
                &raw const info,
                input.raw,
                need,
                render,
                release,
                user,
            )
        })?;
        Ok(clip.keep(input))
    }

    pub fn smooth(self: &Arc<Self>, source: &Clip, options: &Options) -> Result<Clip> {
        let Options {
            supers,
            analyse,
            smooth,
            half,
        } = options;
        self.clip(unsafe {
            (self.api.smooth_fps)(
                self.context,
                source.raw,
                supers.as_ptr(),
                analyse.as_ptr(),
                smooth.as_ptr(),
                i32::from(*half),
            )
        })
    }

    pub fn smooth_blend(
        self: &Arc<Self>,
        source: &Clip,
        options: &Options,
        weights: &[f64],
        blending: &Blending,
    ) -> Result<Clip> {
        let Options {
            supers,
            analyse,
            smooth,
            half,
        } = options;
        self.clip(unsafe {
            (self.api.smooth_fps_blend)(
                self.context,
                source.raw,
                supers.as_ptr(),
                analyse.as_ptr(),
                smooth.as_ptr(),
                i32::from(*half),
                weights.as_ptr(),
                weights.len() as i32,
                i64::from(blending.fps),
                1,
                f64::from(blending.gamma),
            )
        })
    }

    pub fn still(self: &Arc<Self>, clip: Clip, source: &Arc<Clip>, mask: &Mask) -> Result<Clip> {
        let [limit, edge, tolerance] = [mask.limit, mask.edge, mask.tolerance].map(f64::from);
        let raw =
            unsafe { (self.api.still)(self.context, clip.raw, source.raw, limit, edge, tolerance) };
        Ok(self.clip(raw)?.keep(clip).keep(Arc::clone(source)))
    }

    pub fn layer(self: &Arc<Self>, source: &Clip, options: &Options, limit: f32) -> Result<Clip> {
        let Options {
            supers,
            analyse,
            half,
            ..
        } = options;
        self.clip(unsafe {
            (self.api.layer)(
                self.context,
                source.raw,
                supers.as_ptr(),
                analyse.as_ptr(),
                i32::from(*half),
                f64::from(limit),
            )
        })
    }

    pub fn peel(self: &Arc<Self>, source: &Clip, layer: &Clip) -> Result<Clip> {
        self.clip(unsafe { (self.api.peel)(self.context, source.raw, layer.raw) })
    }

    pub fn unpeel(
        self: &Arc<Self>,
        clip: Clip,
        source: &Arc<Clip>,
        layer: &Arc<Clip>,
    ) -> Result<Clip> {
        let raw = unsafe { (self.api.unpeel)(self.context, clip.raw, source.raw, layer.raw) };
        let clip = self.clip(raw)?.keep(clip);
        Ok(clip.keep(Arc::clone(source)).keep(Arc::clone(layer)))
    }
}

unsafe fn error(api: &Api) -> Error {
    Error(
        unsafe { CStr::from_ptr((api.error)()) }
            .to_string_lossy()
            .into_owned(),
    )
}

impl Clip {
    pub fn keep(mut self, parent: impl Into<Arc<Clip>>) -> Self {
        self.parents.push(parent.into());
        self
    }

    pub fn shape(&self) -> Shape {
        let mut info = VideoInfo::default();
        unsafe { (self.svp.api.clip_info)(self.raw, &raw mut info) };
        Shape {
            deep: matches!(info.format, YUV420P16 | YUV444P16),
            yuv444: matches!(info.format, YUV444P8 | YUV444P16),
            width: info.width as u32,
            height: info.height as u32,
            fps: (info.fps_num, info.fps_den),
            frames: info.num_frames,
        }
    }

    pub fn end(&self, last: i32) {
        unsafe { (self.svp.api.clip_set_end)(self.raw, last) };
    }

    pub fn frame(&self, n: i32) -> Result<Frame> {
        let raw = unsafe { (self.svp.api.get_frame)(self.raw, n) };
        if raw.is_null() {
            return Err(unsafe { error(self.svp.api) });
        }
        Ok(Frame {
            api: self.svp.api,
            raw,
        })
    }
}

pub struct Options {
    supers: CString,
    analyse: CString,
    smooth: CString,
    half: bool,
}

impl Options {
    // stolen from smoothie with love <3
    pub fn new(
        interpolation: &Interpolation,
        rate: &str,
        mask: Option<&Mask>,
        dedup: Option<f32>,
        gpu: bool,
        half: bool,
    ) -> Self {
        let Interpolation { speed, tuning, .. } = *interpolation;
        let animation = tuning == Tuning::Animation;
        let coarse = animation || speed == Speed::Fastest;
        let quick = coarse || speed == Speed::Faster;

        let pel = if speed == Speed::Medium { "" } else { "pel:1," };
        let lean = if gpu && speed == Speed::Medium && !half {
            "lean:true,"
        } else {
            ""
        };
        let supers = format!("{{{pel}{lean}gpu:{}}}", u8::from(gpu));

        let block = match speed {
            _ if coarse => 32,
            Speed::Medium if gpu => 8,
            _ => 16,
        };
        let overlap = match speed {
            _ if coarse => 0,
            Speed::Faster if gpu => 1,
            _ => 2,
        };
        let search = match tuning {
            Tuning::Animation => "type:2,distance:-6,satd:false},distance:0,",
            Tuning::Weak => "distance:-1,trymany:true,",
            _ => "distance:-10,",
        };
        let distance = if animation || speed == Speed::Faster {
            ""
        } else {
            "distance:0,"
        };
        let refine = match tuning {
            _ if quick => "",
            Tuning::Weak => ",refine:[{thsad:250,search:{distance:-1,satd:true}}]",
            _ => ",refine:[{thsad:250}]",
        };
        let analyse = format!(
            "{{block:{{w:{block},overlap:{overlap}}},main:{{search:{{{distance}coarse:{{{search}bad:{{sad:2000}}}}}}}}{refine}}}"
        );

        let algorithm = match interpolation.algorithm {
            Algorithm::Sharp => 2,
            Algorithm::Standard => 13,
            Algorithm::Smooth => 23,
        };
        let masking = match mask {
            Some(Mask {
                method: Masking::Peel,
                peel,
                ..
            }) => format!(",peel:{{limit:{peel}}}"),
            Some(Mask {
                limit,
                edge,
                tolerance,
                ..
            }) => format!(",still:{{limit:{limit},edge:{edge},tolerance:{tolerance}}}"),
            None => String::new(),
        };
        let scene = if tuning == Tuning::Weak {
            "blend:true,mode:0,limits:{blocks:50}"
        } else {
            "blend:false,mode:0"
        };
        let dedup = dedup.map_or(String::new(), |limit| format!(",dedup:{limit}"));
        let smooth = format!(
            "{{rate:{{{rate}}},algo:{algorithm},mask:{{cover:80,area:0,area_sharp:1.2{masking}}},scene:{{{scene}}}{dedup}}}"
        );

        let text = |text: String| CString::new(text).unwrap_or_default();
        Self {
            supers: text(supers),
            analyse: text(analyse),
            smooth: text(smooth),
            half,
        }
    }
}
