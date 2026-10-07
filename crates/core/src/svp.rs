use std::ffi::{CStr, CString, c_char, c_void};
use std::path::Path;
use std::sync::{Arc, Mutex};

use libloading::Library;

use crate::config::{Algorithm, Interpolation, Mask, Speed, Tuning};
use crate::{Error, Result};

const YUV420P8: i32 = 0;

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

api! {
    error: b"osvp_error\0" fn() -> Text;
    create: b"osvp_create\0" fn(Text, i32) -> Raw;
    destroy: b"osvp_destroy\0" fn(Raw);
    source: b"osvp_source\0" fn(Raw, *const VideoInfo, ReadFn, ReleaseFn, Raw) -> Raw;
    smooth_fps: b"osvp_smooth_fps\0" fn(Raw, Raw, Text, Text, Text, i32) -> Raw;
    smooth_fps_blend: b"osvp_smooth_fps_blend\0"
        fn(Raw, Raw, Text, Text, Text, i32, *const f64, i32, i64, i64) -> Raw;
    still: b"osvp_still\0" fn(Raw, Raw, Raw, f64, f64, f64) -> Raw;
    clip_info: b"osvp_clip_info\0" fn(Raw, *mut VideoInfo);
    clip_free: b"osvp_clip_free\0" fn(Raw);
    get_frame: b"osvp_get_frame\0" fn(Raw, i32) -> Raw;
    frame_data: b"osvp_frame_data\0" fn(Raw, i32) -> *const u8;
    frame_stride: b"osvp_frame_stride\0" fn(Raw, i32) -> isize;
    frame_free: b"osvp_frame_free\0" fn(Raw);
}

pub struct Svp {
    api: Api,
    context: Raw,
}

unsafe impl Send for Svp {}
unsafe impl Sync for Svp {}

impl Drop for Svp {
    fn drop(&mut self) {
        unsafe { (self.api.destroy)(self.context) };
    }
}

pub trait Reader: Send + 'static {
    fn read(&mut self, n: i32, planes: [*mut u8; 3], strides: [isize; 3]) -> Result<()>;
}

unsafe extern "C" fn read<R: Reader>(
    user: Raw,
    n: i32,
    planes: *const *mut u8,
    strides: *const isize,
) -> i32 {
    let cell = unsafe { &*user.cast::<Mutex<Option<R>>>() };
    let (planes, strides) = unsafe { (*planes.cast(), *strides.cast()) };
    let mut reader = cell.lock().unwrap_or_else(|poison| poison.into_inner());
    let done = reader
        .as_mut()
        .is_some_and(|reader| reader.read(n, planes, strides).is_ok());
    i32::from(!done)
}

unsafe extern "C" fn release<R: Reader>(user: Raw) {
    drop(unsafe { Box::from_raw(user.cast::<Mutex<Option<R>>>()) });
}

pub struct Clip {
    svp: Arc<Svp>,
    raw: Raw,
    close: Option<Box<dyn Fn() + Send + Sync>>,
    parents: Vec<Clip>,
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
    svp: Arc<Svp>,
    raw: Raw,
}

unsafe impl Send for Frame {}
unsafe impl Sync for Frame {}

impl Drop for Frame {
    fn drop(&mut self) {
        unsafe { (self.svp.api.frame_free)(self.raw) };
    }
}

impl Frame {
    pub fn data(&self, plane: usize) -> *const u8 {
        unsafe { (self.svp.api.frame_data)(self.raw, plane as i32) }
    }

    pub fn stride(&self, plane: usize) -> usize {
        unsafe { (self.svp.api.frame_stride)(self.raw, plane as i32) as usize }
    }

    pub fn copy(&self, planes: [*mut u8; 3], strides: [isize; 3], width: usize, height: usize) {
        for plane in 0..3 {
            let shift = usize::from(plane > 0);
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
    pub width: u32,
    pub height: u32,
    pub fps: (i64, i64),
    pub frames: i32,
}

impl Svp {
    pub fn load(directory: &Path) -> Result<Arc<Self>> {
        let path = directory.join(libloading::library_filename("open_svpflow"));
        let library = unsafe { Library::new(&path) }
            .map_err(|error| format!("cannot load {}: {error}", path.display()))?;
        let api = Api::load(library).map_err(|error| error.to_string())?;
        let directory = CString::new(directory.to_string_lossy().as_bytes())
            .map_err(|error| error.to_string())?;
        let context = unsafe { (api.create)(directory.as_ptr(), 0) };
        if context.is_null() {
            return Err(unsafe { error(&api) });
        }
        Ok(Arc::new(Self { api, context }))
    }

    fn clip(self: &Arc<Self>, raw: Raw) -> Result<Clip> {
        if raw.is_null() {
            return Err(unsafe { error(&self.api) });
        }
        Ok(Clip {
            svp: Arc::clone(self),
            raw,
            close: None,
            parents: Vec::new(),
        })
    }

    pub fn source<R: Reader>(self: &Arc<Self>, shape: &Shape, reader: R) -> Result<Clip> {
        let info = VideoInfo {
            format: YUV420P8,
            width: shape.width as i32,
            height: shape.height as i32,
            fps_num: shape.fps.0,
            fps_den: shape.fps.1,
            num_frames: shape.frames,
        };
        let cell = Box::into_raw(Box::new(Mutex::new(Some(reader)))) as usize;
        let (read, release) = (read::<R>, release::<R>);
        let user = cell as Raw;
        let raw = unsafe { (self.api.source)(self.context, &raw const info, read, release, user) };
        // the host frees the cell with the clip, but not when it makes no clip
        let mut clip = self.clip(raw).inspect_err(|_| unsafe { release(user) })?;
        clip.close = Some(Box::new(move || {
            let cell = unsafe { &*(cell as *const Mutex<Option<R>>) };
            drop(cell.lock().map(|mut reader| reader.take()));
        }));
        Ok(clip)
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
        fps: u32,
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
                i64::from(fps),
                1,
            )
        })
    }

    pub fn still(self: &Arc<Self>, clip: Clip, source: &Clip, mask: &Mask) -> Result<Clip> {
        let [limit, edge, tolerance] = [mask.limit, mask.edge, mask.tolerance].map(f64::from);
        let raw =
            unsafe { (self.api.still)(self.context, clip.raw, source.raw, limit, edge, tolerance) };
        Ok(self.clip(raw)?.keep(clip))
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
    pub fn keep(mut self, parent: Clip) -> Self {
        self.parents.push(parent);
        self
    }

    pub fn shape(&self) -> Shape {
        let mut info = VideoInfo::default();
        unsafe { (self.svp.api.clip_info)(self.raw, &raw mut info) };
        Shape {
            width: info.width as u32,
            height: info.height as u32,
            fps: (info.fps_num, info.fps_den),
            frames: info.num_frames,
        }
    }

    pub fn frame(&self, n: i32) -> Result<Frame> {
        let raw = unsafe { (self.svp.api.get_frame)(self.raw, n) };
        if raw.is_null() {
            return Err(unsafe { error(&self.svp.api) });
        }
        Ok(Frame {
            svp: Arc::clone(&self.svp),
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
        gpu: bool,
        half: bool,
    ) -> Self {
        let Interpolation { speed, tuning, .. } = *interpolation;
        let animation = tuning == Tuning::Animation;
        let coarse = animation || speed == Speed::Fastest;
        let quick = coarse || speed == Speed::Faster;

        let pel = if speed == Speed::Medium { "" } else { "pel:1," };
        let supers = format!("{{{pel}gpu:{}}}", u8::from(gpu));

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
        let still = match mask {
            Some(Mask {
                limit,
                edge,
                tolerance,
                ..
            }) if gpu => {
                format!(",still:{{limit:{limit},edge:{edge},tolerance:{tolerance}}}")
            }
            _ => String::new(),
        };
        let scene = if tuning == Tuning::Weak {
            "blend:true,mode:0,limits:{blocks:50}"
        } else {
            "blend:false,mode:0"
        };
        let smooth = format!(
            "{{rate:{{{rate}}},algo:{algorithm},mask:{{cover:80,area:0,area_sharp:1.2{still}}},scene:{{{scene}}}}}"
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
