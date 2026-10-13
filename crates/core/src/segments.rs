use std::collections::VecDeque;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, SyncSender};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use ffmpeg_next::format::Pixel;
use ffmpeg_next::frame;
use ffmpeg_next::log::Level;

use crate::Result;
use crate::blend::Blend;
use crate::config::Blending;
use crate::decode::{Ahead, Decoder, Gpu, Meta, gpus};

// the pictures that one decoder holds, next to the ones that its blend takes for a frame
const HELD: usize = 32;
// the frames that a part has at least, so that the pictures it decodes again stay few
const LEAST: i32 = 16;
// the reader waits this long for each frame before the lanes start, a short read must not start them
const PATIENCE: Duration = Duration::from_secs(1);

type Made = Result<frame::Video>;

struct Job {
    frames: Range<i32>,
    threads: usize,
    made: SyncSender<Made>,
}

struct Part {
    frames: Range<i32>,
    made: Receiver<Made>,
}

type Jobs = Arc<Mutex<Receiver<Job>>>;

pub struct Segments {
    clip: PathBuf,
    blend: Box<dyn Fn() -> Blend + Send>,
    size: (u32, u32),
    pub frames: i32,
    probe: Decoder,
    math: Blend,
    cores: usize,
    most: usize,
    gpus_most: usize,
    room: usize,
    jobs: Sender<Job>,
    idle: Jobs,
    started: usize,
    shared: Arc<Shared>,
    gpus: usize,
    tune: Tune,
    trial: i32,
    lanes: Vec<JoinHandle<()>>,
    parts: VecDeque<Part>,
    planned: i32,
    next: Option<i32>,
    longest: usize,
    waiting: Option<Instant>,
    hurry: bool,
}

const GPUS: usize = 4;
// in place of a thread count, for a decoder on the gpu
const GPU: usize = usize::MAX;

#[derive(Default)]
struct Shared {
    gpu: OnceLock<Option<Gpu>>,
    wanted: AtomicUsize,
    pictures: AtomicUsize,
}

// the seconds that the lanes get to settle after a change, and to count the frames after that
const SETTLE: f32 = 1.0;
const COUNT: f32 = 2.0;

struct Tune {
    since: Instant,
    frames: usize,
    settled: Option<(usize, usize)>,
    before: Option<f32>,
    idle: u32,
    done: bool,
}

impl Tune {
    fn new() -> Self {
        Self {
            since: Instant::now(),
            frames: 0,
            settled: None,
            before: None,
            idle: 0,
            done: false,
        }
    }
}

enum Feed {
    Cpu(Box<Decoder>),
    Gpu(Ahead),
}

fn feed(clip: &Path, threads: usize, gpu: Option<Gpu>) -> Result<(usize, Feed)> {
    let feed = match threads {
        GPU => Feed::Gpu(Ahead::new(Decoder::gpu(clip, gpu.ok_or("no gpu")?)?)),
        threads => Feed::Cpu(Box::new(Decoder::threaded(clip, threads)?)),
    };
    Ok((threads, feed))
}

#[cfg(target_os = "linux")]
pub fn memory() -> Option<usize> {
    let text = std::fs::read_to_string("/proc/meminfo").ok()?;
    let line = text.lines().find(|line| line.starts_with("MemTotal:"))?;
    Some(line.split_whitespace().nth(1)?.parse::<usize>().ok()? * 1024)
}

#[cfg(windows)]
pub fn memory() -> Option<usize> {
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
pub fn memory() -> Option<usize> {
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
pub fn memory() -> Option<usize> {
    None
}

fn pixels(picture: &frame::Video, size: (u32, u32)) -> Option<Vec<u8>> {
    let mut pixels = Vec::new();
    for plane in 0..3 {
        let shift = usize::from(plane > 0);
        let (from, step, skip) = match picture.format() {
            Pixel::NV12 if plane > 0 => (1, 2, plane - 1),
            Pixel::NV12 | Pixel::YUV420P | Pixel::YUVJ420P => (plane, 1, 0),
            _ => return None,
        };
        let rows = picture.data(from).chunks(picture.stride(from));
        for row in rows.take(size.1 as usize >> shift) {
            let samples = row.iter().skip(skip).step_by(step);
            pixels.extend(samples.take(size.0 as usize >> shift));
        }
    }
    Some(pixels)
}

// a decoder of the gpu is used only when it gives the same picture as the cpu, some drivers do not
// a computer without this decoder, or a clip that it cannot decode, fails here without a word
fn same(clip: &Path, gpu: Gpu, size: (u32, u32), at: i32) -> bool {
    let picture = |decoder: Result<Decoder>| pixels(&decoder.ok()?.picture(at).ok()?, size);
    ffmpeg_next::log::set_level(Level::Quiet);
    let hard = picture(Decoder::gpu(clip, gpu));
    ffmpeg_next::log::set_level(Level::Error);
    hard.is_some() && hard == picture(Decoder::threaded(clip, 1))
}

struct Lane {
    clip: PathBuf,
    blend: Blend,
    size: (u32, u32),
    jobs: Jobs,
    gpu: Option<(usize, Arc<Shared>, i32)>,
}

fn work(lane: Lane) -> JoinHandle<()> {
    let Lane {
        clip,
        blend,
        size,
        jobs,
        gpu: place,
    } = lane;
    std::thread::spawn(move || {
        let mut gpu = place.as_ref().and_then(|(_, shared, at)| {
            let works = |gpu: &Gpu| same(&clip, *gpu, size, *at);
            *shared.gpu.get_or_init(|| gpus().into_iter().find(works))
        });
        let mut decoder = gpu.and_then(|gpu| feed(&clip, GPU, Some(gpu)).ok());
        let shared = place.as_ref().map(|place| Arc::clone(&place.1));
        let shared = shared.unwrap_or_default();
        loop {
            if let Some((number, shared, _)) = &place
                && (decoder.is_none() || *number >= shared.wanted.load(Ordering::Relaxed))
            {
                return;
            }
            let job = jobs.lock().unwrap_or_else(|poison| poison.into_inner());
            let Ok(Job {
                frames,
                threads,
                made,
            }) = job.recv()
            else {
                return;
            };
            drop(job);
            let last = blend.last(frames.end - 1);
            // a part that is alone has all threads, which is faster than the gpu
            let mut hard = gpu.filter(|_| threads > 0);
            for n in frames {
                let picture = |decoder: &mut Option<(usize, Feed)>, threads, gpu| {
                    if decoder.as_ref().is_none_or(|decoder| decoder.0 != threads) {
                        *decoder = Some(feed(&clip, threads, gpu)?);
                    }
                    let source = |index| match decoder {
                        Some((_, Feed::Cpu(decoder))) => decoder.picture(index),
                        Some((_, Feed::Gpu(decoder))) => {
                            shared.pictures.fetch_add(1, Ordering::Relaxed);
                            decoder.picture(index, last)
                        }
                        None => Err("no decoder".into()),
                    };
                    blend.frame(source, n, size.0, size.1)
                };
                let wanted = if hard.is_some() { GPU } else { threads };
                let mut made_now = picture(&mut decoder, wanted, hard);
                if hard.is_some() && made_now.is_err() {
                    (gpu, hard) = (None, None);
                    decoder = None;
                    made_now = picture(&mut decoder, threads, None);
                }
                if made.send(made_now).is_err() {
                    break;
                }
            }
            if place.is_some() && gpu.is_none() {
                decoder = None;
            }
        }
    })
}

impl Segments {
    pub fn new(clip: &Path, probe: Decoder, weights: &[f64], blending: &Blending) -> Self {
        let Meta {
            width,
            height,
            fps,
            frames,
            ..
        } = probe.meta();
        let (weights, blending) = (weights.to_vec(), blending.clone());
        let blend = move || Blend::new(&weights, fps, &blending, frames);
        let math = blend();
        let cores = std::thread::available_parallelism().map_or(1, usize::from);
        let picture = width as usize * height as usize * 3 / 2;
        // a quarter of the memory: half for the pictures of the lanes, half for the frames that wait
        let budget = memory().map_or(2 << 30, |memory| (memory / 4).clamp(1 << 30, 8 << 30));
        let lanes = budget / 2 / ((math.window() + HELD) * picture);
        // a decoder is the most efficient with few threads
        let most = (cores / 2).min(lanes).max(1);
        let (jobs, idle) = mpsc::channel();
        Self {
            clip: clip.to_owned(),
            size: (width, height),
            frames: math.frames,
            blend: Box::new(blend),
            probe,
            math,
            cores,
            most,
            gpus_most: lanes.saturating_sub(most).clamp(1, GPUS),
            room: budget / 2 / picture,
            jobs,
            idle: Arc::new(Mutex::new(idle)),
            started: 0,
            shared: Arc::default(),
            gpus: 0,
            tune: Tune::new(),
            trial: (frames - 1).min(30),
            lanes: Vec::new(),
            parts: VecDeque::new(),
            planned: 0,
            next: None,
            longest: 0,
            waiting: None,
            hurry: false,
        }
    }

    fn end(&mut self, start: i32) -> i32 {
        let mut step = LEAST;
        let mut end = start;
        loop {
            end = end.saturating_add(step);
            if end >= self.frames {
                return self.frames;
            }
            let Ok(key) = self.probe.key(self.math.first(end)) else {
                return self.frames;
            };
            let aligned = self.math.after(key);
            if aligned > start {
                return aligned.min(self.frames);
            }
            // a clip with few keyframes must not get a seek for each step
            step = step.saturating_mul(2);
        }
    }

    fn lane(&mut self, gpu: bool) {
        let gpu = gpu.then(|| (self.gpus, Arc::clone(&self.shared), self.trial));
        match gpu {
            Some(_) => self.gpus += 1,
            None => self.started += 1,
        }
        self.lanes.push(work(Lane {
            clip: self.clip.clone(),
            blend: (self.blend)(),
            size: self.size,
            jobs: Arc::clone(&self.idle),
            gpu,
        }));
    }

    fn tune(&mut self) {
        let Tune {
            since,
            frames,
            settled,
            before,
            idle,
            done,
        } = &mut self.tune;
        let seconds = since.elapsed().as_secs_f32();
        if *done || self.gpus == 0 || seconds < SETTLE {
            return;
        }
        let pictures = self.shared.pictures.load(Ordering::Relaxed);
        let first = *settled.get_or_insert((*frames, pictures));
        if seconds < SETTLE + COUNT {
            return;
        }
        let lanes = self.shared.wanted.load(Ordering::Relaxed);
        let rate = (pictures - first.1) as f32 / (seconds - SETTLE);
        let all = (*frames - first.0) as f32 * self.math.pace() / (seconds - SETTLE);
        let slow = rate / lanes as f32 * 2.0 < (all - rate) / self.started.max(1) as f32;
        let wanted = match *before {
            // the gpu takes a moment to start, and some computers have none
            _ if rate == 0.0 && *idle < 2 => {
                *idle += 1;
                lanes
            }
            _ if rate == 0.0 || slow => 0,
            Some(before) if rate < 1.15 * before => lanes - 1,
            _ => (lanes + 1).min(self.gpus_most),
        };
        *done = wanted <= lanes && rate > 0.0 || wanted == 0;
        (*since, *settled) = (Instant::now(), None);
        if rate > 0.0 {
            *before = Some(rate);
        }
        self.shared.wanted.store(wanted, Ordering::Relaxed);
        if wanted > self.gpus {
            self.lane(true);
        }
    }

    fn plan(&mut self) {
        if self.hurry {
            self.tune();
        }
        while self.planned < self.frames {
            let start = self.planned;
            let (end, lanes) = match self.hurry {
                true => {
                    let end = match self.next {
                        Some(end) => end,
                        None => self.end(start),
                    };
                    self.next = Some(end);
                    // a long part to the end has no part after it to run at the same time
                    let length = (end - start) as usize;
                    if end == self.frames && length > 2 * self.longest {
                        (end, 1)
                    } else {
                        self.longest = self.longest.max(length);
                        (end, (self.room / self.longest).clamp(1, self.most))
                    }
                }
                false => (start.saturating_add(LEAST).min(self.frames), 1),
            };
            // parts get done out of order, so some more than the lanes keep all lanes at work
            let open = (self.room / self.longest.max(1)).min(lanes * 2).max(1);
            if self.parts.len() >= if lanes == 1 { 1 } else { open } {
                break;
            }
            self.next = None;
            let (threads, room) = match lanes {
                1 => (0, 2),
                lanes => (self.cores.div_ceil(lanes), (end - start) as usize),
            };
            let (made, pictures) = mpsc::sync_channel(room);
            if self.started < lanes {
                self.lane(false);
            }
            if lanes > 1 && self.gpus == 0 {
                self.shared.wanted.store(1, Ordering::Relaxed);
                self.tune.since = Instant::now();
                self.lane(true);
            }
            let _ = self.jobs.send(Job {
                frames: start..end,
                threads,
                made,
            });
            self.parts.push_back(Part {
                frames: start..end,
                made: pictures,
            });
            self.planned = end;
        }
    }

    pub fn lanes(&self) -> (usize, usize) {
        let gpu = match self.shared.gpu.get() {
            Some(Some(_)) => self.shared.wanted.load(Ordering::Relaxed).min(self.gpus),
            _ => 0,
        };
        (self.started, gpu)
    }

    fn stop(&mut self) {
        self.parts.clear();
        let (jobs, idle) = mpsc::channel();
        (self.jobs, self.idle) = (jobs, Arc::new(Mutex::new(idle)));
        for lane in self.lanes.drain(..) {
            let _ = lane.join();
        }
        (self.started, self.gpus, self.shared) = (0, 0, Arc::default());
        self.tune = Tune::new();
    }

    pub fn frame(&mut self, n: i32) -> Result<frame::Video> {
        let near = |part: &Part| part.frames.start <= n;
        if !(self.parts.front().is_some_and(near) && n <= self.planned) {
            if self.started > 1 || self.gpus > 0 {
                self.stop();
            }
            self.parts.clear();
            (self.planned, self.next, self.longest) = (n, None, 0);
            (self.waiting, self.hurry) = (None, false);
        }
        loop {
            self.plan();
            let part = self.parts.front_mut().ok_or("no frame")?;
            if part.frames.is_empty() {
                self.parts.pop_front();
                continue;
            }
            let picture = match part.made.try_recv() {
                Ok(picture) => {
                    self.waiting = None;
                    picture
                }
                Err(_) => {
                    let since = *self.waiting.get_or_insert_with(Instant::now);
                    self.hurry |= since.elapsed() >= PATIENCE && self.most > 1;
                    part.made.recv().map_err(|_| "the decoder stopped")?
                }
            };
            part.frames.start += 1;
            self.tune.frames += 1;
            if part.frames.start > n {
                return picture;
            }
        }
    }
}

impl Drop for Segments {
    // the lanes must end before the process does, or the gpu context goes away under them
    fn drop(&mut self) {
        self.stop();
    }
}
