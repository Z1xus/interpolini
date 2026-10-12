use std::collections::VecDeque;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use ffmpeg_next::frame;

use crate::Result;
use crate::blend::Blend;
use crate::config::Blending;
use crate::decode::{Ahead, Decoder, Meta};

// the most memory for the pictures of all lanes
const BUDGET: usize = 4 << 30;
// the pictures that one decoder and its blend hold
const HELD: usize = 40;
// the frames that a part has at least, so that the pictures it decodes again stay few
const LEAST: i32 = 16;
// the frames in a row that the reader waits for before the lanes start
const PATIENCE: u32 = 8;

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
    room: usize,
    jobs: Sender<Job>,
    idle: Jobs,
    started: usize,
    gpu: bool,
    lanes: Vec<JoinHandle<()>>,
    parts: VecDeque<Part>,
    planned: i32,
    next: Option<i32>,
    longest: usize,
    waits: u32,
    hurry: bool,
}

// one lane on the gpu waits while it blends, two keep the decoder of the gpu at work
const GPUS: usize = 2;
// in place of a thread count, for a decoder on the gpu
const GPU: usize = usize::MAX;

enum Feed {
    Cpu(Box<Decoder>),
    Gpu(Ahead),
}

fn feed(clip: &Path, threads: usize) -> Result<(usize, Feed)> {
    let feed = match threads {
        GPU => Feed::Gpu(Ahead::new(Decoder::gpu(clip)?)),
        threads => Feed::Cpu(Box::new(Decoder::threaded(clip, threads)?)),
    };
    Ok((threads, feed))
}

fn work(
    clip: PathBuf,
    blend: Blend,
    size: (u32, u32),
    jobs: Jobs,
    mut gpu: bool,
) -> JoinHandle<()> {
    std::thread::spawn(move || {
        let mut decoder = match gpu.then(|| feed(&clip, GPU)) {
            Some(Ok(decoder)) => Some(decoder),
            Some(Err(_)) => return,
            None => None,
        };
        let lane = gpu;
        while lane == gpu {
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
            for n in frames {
                let picture = |decoder: &mut Option<(usize, Feed)>, threads| {
                    if decoder.as_ref().is_none_or(|decoder| decoder.0 != threads) {
                        *decoder = Some(feed(&clip, threads)?);
                    }
                    let source = |index| match decoder {
                        Some((_, Feed::Cpu(decoder))) => decoder.picture(index),
                        Some((_, Feed::Gpu(decoder))) => decoder.picture(index, last),
                        None => Err("no decoder".into()),
                    };
                    blend.frame(source, n, size.0, size.1)
                };
                // a part that is alone has all threads, which is faster than the gpu
                let hard = gpu && threads > 0;
                let mut made_now = picture(&mut decoder, if hard { GPU } else { threads });
                if hard && made_now.is_err() {
                    gpu = false;
                    made_now = picture(&mut decoder, threads);
                }
                if made.send(made_now).is_err() {
                    break;
                }
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
        // a decoder is the most efficient with few threads
        let most = (cores / 2).min(BUDGET / 2 / HELD / picture).max(1);
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
            room: BUDGET / 2 / picture,
            jobs,
            idle: Arc::new(Mutex::new(idle)),
            started: 0,
            gpu: false,
            lanes: Vec::new(),
            parts: VecDeque::new(),
            planned: 0,
            next: None,
            longest: 0,
            waits: 0,
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

    fn plan(&mut self) {
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
                self.started += 1;
                let (clip, jobs) = (self.clip.clone(), Arc::clone(&self.idle));
                self.lanes
                    .push(work(clip, (self.blend)(), self.size, jobs, false));
            }
            if lanes > 1 && !self.gpu {
                self.gpu = true;
                for _ in 0..GPUS {
                    let (clip, jobs) = (self.clip.clone(), Arc::clone(&self.idle));
                    self.lanes
                        .push(work(clip, (self.blend)(), self.size, jobs, true));
                }
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

    pub fn frame(&mut self, n: i32) -> Result<frame::Video> {
        let near = |part: &Part| part.frames.start <= n;
        if !(self.parts.front().is_some_and(near) && n <= self.planned) {
            self.parts.clear();
            (self.planned, self.next, self.longest) = (n, None, 0);
            (self.waits, self.hurry) = (0, false);
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
                    self.waits = 0;
                    picture
                }
                Err(_) => {
                    self.waits += 1;
                    self.hurry |= self.waits >= PATIENCE && self.most > 1;
                    part.made.recv().map_err(|_| "the decoder stopped")?
                }
            };
            part.frames.start += 1;
            if part.frames.start > n {
                return picture;
            }
        }
    }
}

impl Drop for Segments {
    // the lanes must end before the process does, or the gpu context goes away under them
    fn drop(&mut self) {
        self.parts.clear();
        self.jobs = mpsc::channel().0;
        for lane in self.lanes.drain(..) {
            let _ = lane.join();
        }
    }
}
