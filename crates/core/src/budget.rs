use std::cell::Cell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use rayon::{ThreadPool, ThreadPoolBuilder};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Class {
    Interactive,
    Background,
    Render,
}

#[derive(Clone, Copy)]
pub struct Machine {
    pub cores: usize,
    pub ram: usize,
}

static PREVIEW: AtomicBool = AtomicBool::new(false);

thread_local! {
    static CLASS: Cell<Class> = const { Cell::new(Class::Render) };
}

#[cfg(target_os = "linux")]
fn memory() -> Option<usize> {
    let text = std::fs::read_to_string("/proc/meminfo").ok()?;
    let line = text.lines().find(|line| line.starts_with("MemTotal:"))?;
    Some(line.split_whitespace().nth(1)?.parse::<usize>().ok()? * 1024)
}

#[cfg(windows)]
fn memory() -> Option<usize> {
    // MEMORYSTATUSEX, the fields after the total are not read
    #[repr(C)]
    struct Status {
        length: u32,
        load: u32,
        total: u64,
        rest: [u64; 6],
    }
    unsafe extern "system" {
        fn GlobalMemoryStatusEx(status: *mut Status) -> i32;
    }
    let mut status = Status {
        length: size_of::<Status>() as u32,
        load: 0,
        total: 0,
        rest: [0; 6],
    };
    (unsafe { GlobalMemoryStatusEx(&raw mut status) } != 0).then_some(status.total as usize)
}

#[cfg(target_os = "macos")]
fn memory() -> Option<usize> {
    use std::ffi::{c_char, c_void};
    unsafe extern "C" {
        fn sysctlbyname(
            name: *const c_char,
            value: *mut c_void,
            size: *mut usize,
            new: *mut c_void,
            new_size: usize,
        ) -> i32;
    }
    let (mut total, mut size) = (0u64, size_of::<u64>());
    let name = c"hw.memsize".as_ptr();
    let none = std::ptr::null_mut();
    let code = unsafe { sysctlbyname(name, (&raw mut total).cast(), &raw mut size, none, 0) };
    (code == 0).then_some(total as usize)
}

#[cfg(not(any(target_os = "linux", windows, target_os = "macos")))]
fn memory() -> Option<usize> {
    None
}

pub fn machine() -> Machine {
    static MACHINE: OnceLock<Machine> = OnceLock::new();
    *MACHINE.get_or_init(|| Machine {
        cores: std::thread::available_parallelism().map_or(1, usize::from),
        // 8 GiB when the system does not tell
        ram: memory().unwrap_or(8 << 30),
    })
}

// the gui tells when a preview plays, so that a render leaves it some cores
pub fn preview(active: bool) {
    PREVIEW.store(active, Ordering::Relaxed);
}

// on a computer with fewer than 3 cores the classes share them
pub fn threads(class: Class) -> usize {
    let cores = machine().cores;
    let front = (cores / 4).clamp(1, 4);
    let back = (cores / 8).clamp(1, 2);
    match class {
        Class::Interactive => front,
        Class::Background => back,
        Class::Render if PREVIEW.load(Ordering::Relaxed) => {
            cores.saturating_sub(front + back).max(1)
        }
        Class::Render => cores,
    }
}

pub fn bytes(class: Class) -> usize {
    let ram = machine().ram;
    match class {
        Class::Interactive => ram / 8,
        Class::Background => ram / 16,
        Class::Render => (ram / 4).min(8 << 30),
    }
}

pub fn enter(class: Class) {
    CLASS.set(class);
}

pub fn current() -> Class {
    CLASS.get()
}

pub fn own() -> usize {
    threads(current())
}

// a decoder shares the cores with the encoder, and ffmpeg gains little over 16
pub fn decode() -> usize {
    match current() {
        Class::Interactive => own().min(2),
        Class::Background => 1,
        Class::Render => own().div_ceil(2).min(16),
    }
}

type Pools = [Mutex<Option<(usize, Arc<ThreadPool>)>>; 3];

// the rayon work of this thread runs in the pool of its class
pub fn install<R: Send>(work: impl FnOnce() -> R + Send) -> R {
    static POOLS: Pools = [const { Mutex::new(None) }; 3];
    let class = current();
    let wanted = threads(class);
    let pool = {
        let mut held = POOLS[class as usize]
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        match held.as_ref().filter(|held| held.0 == wanted) {
            Some((_, pool)) => Arc::clone(pool),
            None => {
                let name = format!("{class:?}").to_lowercase();
                let builder = ThreadPoolBuilder::new().num_threads(wanted);
                let pool = builder.thread_name(move |index| format!("{name} {index}"));
                let pool = Arc::new(pool.build().expect("a thread pool could not start"));
                *held = Some((wanted, Arc::clone(&pool)));
                pool
            }
        }
    };
    pool.install(work)
}
