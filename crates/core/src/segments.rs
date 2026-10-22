use std::collections::VecDeque;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use ffmpeg_next::format::Pixel;
use ffmpeg_next::log::Level;
use ffmpeg_next::{Packet, encoder, ffi, frame, picture};

use crate::Result;
use crate::blend::Blend;
use crate::budget;
use crate::config::Blending;
use crate::decode::{Ahead, Decoder, Gpu, Meta, gpus};
use crate::encode::{GOP, Spare};
use crate::work::{self, Cancel};

// the pictures that one decoder holds, next to the ones that its blend takes for a frame
const HELD: usize = 32;
// the frames that a part has at least, so that the pictures it decodes again stay few
const LEAST: i32 = 16;
// the reader's waits are counted over this time, a shorter read starts no lanes
const PATIENCE: Duration = Duration::from_secs(1);
// the wait that the lanes must save, their start costs time
const START: Duration = Duration::from_secs(2);
// a lane with fewer threads is too slow for the reader
const THREADS: usize = 12;
const POLL: Duration = Duration::from_millis(20);

pub enum Made {
    Picture(frame::Video),
    Packets(Vec<Packet>),
}

struct Job {
    frames: Range<i32>,
    threads: usize,
    packets: bool,
    made: SyncSender<Result<Made>>,
}

struct Part {
    frames: Range<i32>,
    made: Receiver<Result<Made>>,
}

struct Packer(encoder::video::Encoder);

unsafe impl Send for Packer {}

impl Packer {
    fn drain(&mut self, packets: &mut Vec<Packet>) {
        let mut packet = Packet::empty();
        while self.0.receive_packet(&mut packet).is_ok() {
            packets.push(std::mem::replace(&mut packet, Packet::empty()));
        }
    }

    fn end(&mut self, packets: &mut Vec<Packet>) -> Result<()> {
        self.0.send_eof()?;
        self.drain(packets);
        unsafe { ffi::avcodec_flush_buffers(self.0.as_mut_ptr()) };
        Ok(())
    }

    fn pack(&mut self, mut picture: frame::Video, n: i32, first: bool, last: bool) -> Result<Made> {
        picture.set_pts(Some(i64::from(n)));
        if first {
            picture.set_kind(picture::Type::I);
        }
        self.0.send_frame(&picture)?;
        let mut packets = Vec::new();
        match last {
            true => self.end(&mut packets)?,
            false => self.drain(&mut packets),
        }
        Ok(Made::Packets(packets))
    }
}

#[derive(Default)]
struct Jobs {
    queue: Mutex<(VecDeque<Job>, bool)>,
    ready: Condvar,
    packing: AtomicUsize,
}

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
    jobs: Arc<Jobs>,
    started: usize,
    shared: Arc<Shared>,
    gpus: usize,
    tune: Tune,
    trial: i32,
    lanes: Vec<JoinHandle<()>>,
    closing: Vec<JoinHandle<()>>,
    parts: VecDeque<Part>,
    planned: i32,
    next: Option<i32>,
    longest: usize,
    waiting: (Instant, Duration, u32),
    warm: bool,
    hurry: bool,
    spare: Option<Spare>,
    packers: Arc<Mutex<Vec<Packer>>>,
    packed: usize,
    from: i32,
    meta: Meta,
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
    base: f32,
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
            base: 0.0,
            idle: 0,
            done: false,
        }
    }
}

enum Feed {
    Cpu(Box<Decoder>),
    Gpu(Ahead),
}

// 0 threads is a decoder that has the whole cpu
fn feed(
    clip: &Path,
    threads: usize,
    gpu: Option<Gpu>,
    cores: usize,
    by: u32,
) -> Result<(usize, Feed)> {
    let feed = match threads {
        GPU => Feed::Gpu(Ahead::new(
            Decoder::gpu(clip, gpu.ok_or("no gpu")?)?.shrink(by),
        )),
        0 => Feed::Cpu(Box::new(Decoder::threaded(clip, cores)?.shrink(by))),
        threads => Feed::Cpu(Box::new(Decoder::threaded(clip, threads)?.shrink(by))),
    };
    Ok((threads, feed))
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

// the answer for each clip, a check opens two decoders
fn decoder(clip: &Path, size: (u32, u32), at: i32) -> Option<Gpu> {
    static FOUND: Mutex<Vec<(PathBuf, Option<Gpu>)>> = Mutex::new(Vec::new());
    let mut found = FOUND.lock().unwrap_or_else(|poison| poison.into_inner());
    if let Some((_, gpu)) = found.iter().find(|found| found.0 == clip) {
        return *gpu;
    }
    let works = |gpu: &Gpu| same(clip, *gpu, size, at);
    let gpu = gpus().into_iter().find(works);
    if found.len() >= 64 {
        found.remove(0);
    }
    found.push((clip.to_owned(), gpu));
    gpu
}

struct Lane {
    clip: PathBuf,
    blend: Blend,
    size: (u32, u32),
    jobs: Arc<Jobs>,
    gpu: Option<(usize, Arc<Shared>, i32)>,
    packers: Arc<Mutex<Vec<Packer>>>,
    from: i32,
    meta: Meta,
    by: u32,
    cores: usize,
    workers: usize,
}

fn work(lane: Lane) -> JoinHandle<()> {
    work::spawn("decoder", move || {
        let jobs = Arc::clone(&lane.jobs);
        let mut packer = None;
        budget::divided(lane.workers, || run(lane, &mut packer));
        if packer.is_some() {
            jobs.packing.fetch_sub(1, Ordering::Relaxed);
        }
    })
}

fn run(lane: Lane, packer: &mut Option<Packer>) {
    let Lane {
        clip,
        blend,
        size,
        jobs,
        gpu: place,
        packers,
        from,
        meta,
        by,
        cores,
        workers: _,
    } = lane;
    let mut gpu = place
        .as_ref()
        .and_then(|(_, shared, at)| *shared.gpu.get_or_init(|| decoder(&clip, size, *at)));
    let mut decoder = gpu.and_then(|gpu| feed(&clip, GPU, Some(gpu), cores, by).ok());
    let shared = place.as_ref().map(|place| Arc::clone(&place.1));
    let shared = shared.unwrap_or_default();
    loop {
        if let Some((number, shared, _)) = &place
            && (decoder.is_none() || *number >= shared.wanted.load(Ordering::Relaxed))
        {
            return;
        }
        let mut queue = jobs
            .queue
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let Job {
            frames,
            threads,
            packets,
            made,
        } = loop {
            if queue.1 {
                return;
            }
            // a lane on the gpu starts slowly, so it takes the last part
            let job = match place {
                Some(_) => queue.0.pop_back(),
                None => queue.0.pop_front(),
            };
            match job {
                Some(job) => break job,
                None => {
                    let woken = jobs.ready.wait(queue);
                    queue = woken.unwrap_or_else(|poison| poison.into_inner());
                }
            }
        };
        if packets && packer.is_none() {
            let packers = packers.lock();
            *packer = packers.unwrap_or_else(|poison| poison.into_inner()).pop();
            if packer.is_some() {
                jobs.packing.fetch_add(1, Ordering::Relaxed);
            }
        }
        let packets = packets && (packer.is_some() || jobs.packing.load(Ordering::Relaxed) > 0);
        if packets && packer.is_none() {
            queue.0.push_front(Job {
                frames,
                threads,
                packets,
                made,
            });
            jobs.ready.notify_all();
            return;
        }
        drop(queue);
        let (first, end) = (frames.start, frames.end);
        let last = blend.last(frames.end - 1);
        // a part that is alone has all threads, which is faster than the gpu
        let mut hard = gpu.filter(|_| threads > 0);
        for n in frames {
            let picture = |decoder: &mut Option<(usize, Feed)>, threads, gpu| {
                if decoder.as_ref().is_none_or(|decoder| decoder.0 != threads) {
                    *decoder = Some(feed(&clip, threads, gpu, cores, by)?);
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
            let made_now = match packer.as_mut().filter(|_| packets) {
                Some(packer) => made_now.and_then(|mut picture| {
                    meta.tag(&mut picture);
                    packer.pack(picture, n - from, n == first, n + 1 == end)
                }),
                None => made_now.map(Made::Picture),
            };
            if made.send(made_now).is_err() {
                // the reader left this part, and the encoder still holds pictures of it
                if let Some(packer) = packer.as_mut().filter(|_| packets && n + 1 < end) {
                    let _ = packer.end(&mut Vec::new());
                }
                break;
            }
        }
        if place.is_some() && gpu.is_none() {
            decoder = None;
        }
    }
}

impl Segments {
    pub fn new(clip: &Path, probe: Decoder, weights: &[f64], blending: &Blending) -> Result<Self> {
        let Meta {
            width,
            height,
            fps,
            frames,
            ..
        } = probe.meta();
        let meta = probe.meta();
        let (weights, blending) = (weights.to_vec(), blending.clone());
        let blend = move || Blend::new(&weights, fps, &blending, frames);
        let math = blend();
        let class = budget::current();
        let cores = budget::threads(class);
        let luma = width as usize * height as usize;
        let picture = luma + 2 * (luma >> (meta.shift() * 2));
        // a quarter of the memory: half for the pictures of the lanes, half for the frames that wait
        let budget = budget::bytes(class);
        // a clip that does not fit still runs with one lane and few frames in flight
        let lanes = (budget / 2 / ((math.window() + HELD) * picture)).max(1);
        let most = (cores / THREADS).clamp(1, lanes);
        Ok(Self {
            clip: clip.to_owned(),
            size: (width, height),
            frames: math.frames,
            blend: Box::new(blend),
            probe,
            math,
            cores,
            most,
            gpus_most: lanes.saturating_sub(most).min(GPUS),
            room: (budget / 2 / picture).max(2),
            jobs: Arc::default(),
            started: 0,
            shared: Arc::default(),
            gpus: 0,
            tune: Tune::new(),
            trial: (frames - 1).min(30),
            lanes: Vec::new(),
            closing: Vec::new(),
            parts: VecDeque::new(),
            planned: 0,
            next: None,
            longest: 0,
            waiting: (Instant::now(), Duration::ZERO, 0),
            warm: false,
            hurry: false,
            spare: None,
            packers: Arc::default(),
            packed: 0,
            from: 0,
            meta,
        })
    }

    pub fn encode(&mut self, spare: Spare, from: i32, end: i32) {
        self.spare = Some(spare);
        self.from = from;
        self.frames = self.frames.min(end);
    }

    fn end(&mut self, start: i32) -> i32 {
        // each part starts with a key frame, so short parts make the file large
        let mut step = match self.spare {
            Some(_) => GOP as i32 / 2,
            None => LEAST,
        };
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
        let workers = self.most + self.gpus_most;
        let gpu = gpu.then(|| (self.gpus, Arc::clone(&self.shared), self.trial));
        match gpu {
            Some(_) => self.gpus += 1,
            None => self.started += 1,
        }
        while self.hurry && self.packed < self.started + self.gpus {
            let Some(Ok(encoder)) = self
                .spare
                .as_ref()
                .map(|spare| budget::divided(workers, || spare()))
            else {
                self.spare = None;
                break;
            };
            let packers = self.packers.lock();
            let mut packers = packers.unwrap_or_else(|poison| poison.into_inner());
            packers.push(Packer(encoder));
            self.packed += 1;
        }
        self.lanes.push(work(Lane {
            clip: self.clip.clone(),
            blend: (self.blend)(),
            size: self.size,
            jobs: Arc::clone(&self.jobs),
            gpu,
            packers: Arc::clone(&self.packers),
            from: self.from,
            meta: self.meta,
            by: self.probe.by(),
            cores: self.cores,
            workers,
        }));
    }

    fn tune(&mut self) {
        let Tune {
            since,
            frames,
            settled,
            before,
            base,
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
        let slow = rate * (self.cores as f32) < (all - rate) * lanes as f32;
        let wanted = match *before {
            // the gpu takes a moment to start, and some computers have none
            _ if rate == 0.0 && *idle < 2 => {
                *idle += 1;
                lanes
            }
            _ if rate == 0.0 || slow => 0,
            // the gpu must make the clip faster than one decoder did
            None if all < 1.1 * *base => 0,
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
                    // the last part has no part to run beside it
                    let length = (end - start) as usize;
                    if end == self.frames && length > 2 * self.longest {
                        (end, 0)
                    } else {
                        self.longest = self.longest.max(length);
                        let fit = match self.spare {
                            Some(_) => self.most,
                            None => self.room / self.longest,
                        };
                        (end, fit.clamp(1, self.most))
                    }
                }
                false => (start.saturating_add(LEAST).min(self.frames), 0),
            };
            // parts get done out of order, so some more than the lanes keep all lanes at work
            let all = lanes + self.shared.wanted.load(Ordering::Relaxed);
            let open = match self.spare {
                Some(_) => all * 2,
                None => (self.room / self.longest.max(1)).min(all * 2).max(1),
            };
            if self.parts.len() >= if lanes == 0 { 1 } else { open } {
                break;
            }
            self.next = None;
            // 0 lanes: one decoder makes the part with all threads
            let (threads, room) = match lanes {
                0 => (0, 2),
                lanes => (self.cores.div_ceil(lanes), (end - start) as usize),
            };
            let (made, pictures) = mpsc::sync_channel(room.min(self.room));
            if self.started < lanes.max(1) {
                self.lane(false);
            }
            if lanes > 0 && self.gpus == 0 && self.gpus_most > 0 {
                self.shared.wanted.store(1, Ordering::Relaxed);
                self.tune.since = Instant::now();
                self.lane(true);
            }
            let packets = lanes > 0 && self.spare.is_some();
            let queue = self.jobs.queue.lock();
            let mut queue = queue.unwrap_or_else(|poison| poison.into_inner());
            queue.0.push_back(Job {
                frames: start..end,
                threads,
                packets,
                made,
            });
            drop(queue);
            self.jobs.ready.notify_all();
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

    // the lanes end on their own, a thread joins them so that the caller does not wait
    fn stop(&mut self) {
        self.parts.clear();
        let jobs = std::mem::take(&mut self.jobs);
        jobs.queue
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .1 = true;
        jobs.ready.notify_all();
        self.closing.retain(|closer| !closer.is_finished());
        let lanes = std::mem::take(&mut self.lanes);
        if !lanes.is_empty() {
            self.closing.push(work::spawn("closer", move || {
                for lane in lanes {
                    let _ = lane.join();
                }
            }));
        }
        (self.started, self.gpus, self.shared) = (0, 0, Arc::default());
        self.tune = Tune::new();
    }

    pub fn frame(&mut self, n: i32, cancel: &Cancel) -> Result<frame::Video> {
        match self.take(n, cancel)? {
            Made::Picture(picture) => Ok(picture),
            Made::Packets(_) => Err("no picture".into()),
        }
    }

    pub fn take(&mut self, n: i32, cancel: &Cancel) -> Result<Made> {
        let near = |part: &Part| part.frames.start <= n;
        if !(self.parts.front().is_some_and(near) && n <= self.planned) {
            if self.started > 1 || self.gpus > 0 {
                self.stop();
            }
            self.parts.clear();
            (self.planned, self.next, self.longest) = (n, None, 0);
            self.waiting = (Instant::now(), Duration::ZERO, 0);
            (self.warm, self.hurry) = (false, false);
        }
        loop {
            self.plan();
            let part = self.parts.front_mut().ok_or("no frame")?;
            if part.frames.is_empty() {
                self.parts.pop_front();
                continue;
            }
            let picture = match part.made.try_recv() {
                Ok(picture) => picture,
                Err(_) => {
                    let since = Instant::now();
                    let stopped = "the decoder stopped";
                    let picture = loop {
                        match part.made.recv_timeout(POLL) {
                            Ok(picture) => break picture,
                            Err(RecvTimeoutError::Timeout) => {
                                cancel.check()?;
                                // a part that no lane takes would be waited for forever
                                if self.lanes.iter().all(JoinHandle::is_finished) {
                                    return Err(stopped.into());
                                }
                            }
                            Err(RecvTimeoutError::Disconnected) => return Err(stopped.into()),
                        }
                    };
                    self.waiting.1 += since.elapsed();
                    picture
                }
            };
            let (since, waited, seen) = &mut self.waiting;
            *seen += 1;
            if since.elapsed() >= PATIENCE {
                let left = (self.frames - n) as f32 / *seen as f32;
                // lanes with their own encoders can halve the time that is left
                let least = match self.spare {
                    Some(_) => PATIENCE / 2,
                    None => Duration::ZERO,
                };
                let saved = (*waited).max(least).mul_f32(left);
                // a decoder is slow in its first second on any computer
                if self.warm && !self.hurry && saved >= START {
                    self.hurry = true;
                    self.tune.base =
                        *seen as f32 * self.math.pace() / since.elapsed().as_secs_f32();
                }
                self.waiting = (Instant::now(), Duration::ZERO, 0);
                self.warm = true;
            }
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
        for closer in self.closing.drain(..) {
            let _ = closer.join();
        }
    }
}
