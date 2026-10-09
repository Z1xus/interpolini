use std::env::consts::{DLL_PREFIX, DLL_SUFFIX};
use std::sync::OnceLock;

use libloading::Library;

use crate::config::{self, Backend};

#[cfg(windows)]
const DRIVER: &str = "nvcuda.dll";
#[cfg(not(windows))]
const DRIVER: &str = "libcuda.so.1";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Support {
    Ready,
    Platform,
    Gpu,
    Driver,
    Libraries,
    Models,
}

impl Support {
    pub fn need(self) -> &'static str {
        match self {
            Self::Ready => "",
            Self::Platform => "TensorRT isn't available on this system.",
            Self::Gpu => "TensorRT needs an NVIDIA GPU.",
            Self::Driver => "TensorRT needs a newer NVIDIA driver.",
            Self::Libraries => "TensorRT isn't installed.",
            Self::Models => "TensorRT has no model installed.",
        }
    }

    pub fn fixable(self) -> bool {
        matches!(self, Self::Libraries | Self::Models)
    }
}

pub fn plugin() -> bool {
    let name = format!("{DLL_PREFIX}interpolini_rife_trt{DLL_SUFFIX}");
    config::app_dir().join(name).exists()
}

pub fn libraries() -> bool {
    let name = match cfg!(windows) {
        true => "nvinfer_10.dll",
        false => "libnvinfer.so.10",
    };
    config::app_dir().join("tensorrt").join(name).exists()
}

fn probe() -> Option<i32> {
    type Count = unsafe extern "C" fn(*mut i32) -> i32;
    let library = unsafe { Library::new(DRIVER) }.ok()?;
    let found = unsafe {
        let init: unsafe extern "C" fn(u32) -> i32 = *library.get(b"cuInit\0").ok()?;
        let version: Count = *library.get(b"cuDriverGetVersion\0").ok()?;
        let count: Count = *library.get(b"cuDeviceGetCount\0").ok()?;
        let (mut cuda, mut devices) = (0, 0);
        let ready = init(0) == 0 && version(&mut cuda) == 0 && count(&mut devices) == 0;
        (ready && devices > 0).then_some(cuda)
    };
    // the driver keeps gpu state, so it stays loaded
    std::mem::forget(library);
    found
}

pub fn support() -> Support {
    static GPU: OnceLock<Option<i32>> = OnceLock::new();
    if !plugin() {
        return Support::Platform;
    }
    match GPU.get_or_init(probe) {
        None => Support::Gpu,
        // the libraries are made for cuda 12
        Some(cuda) if *cuda < 12000 => Support::Driver,
        Some(_) if !libraries() => Support::Libraries,
        Some(_) if config::models(Backend::Tensorrt).is_empty() => Support::Models,
        Some(_) => Support::Ready,
    }
}
