use std::collections::VecDeque;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;

use ffmpeg_next::{Packet, encoder, ffi, frame, picture};

use crate::Result;
use crate::blend::Blend;
use crate::budget;
use crate::config::Blending;
use crate::decode::{Decoder, Meta, PAST, REFERENCES};
use crate::encode::{GOP, Spare};
use crate::work::{self, Cancel, Receiver, Sender};

pub enum Made {
    Picture(frame::Video),
    Packets(Vec<Packet>),
}

struct Job {
    frames: Range<i32>,
    threads: usize,
    packets: bool,
    made: Sender<Result<Made>>,
}

struct Part {
    frames: Range<i32>,
    made: Receiver<Result<Made>>,
    read: bool,
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
    alive: AtomicUsize,
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
    room: usize,
    jobs: Arc<Jobs>,
    started: usize,
    lanes: Vec<JoinHandle<()>>,
    closing: Vec<JoinHandle<()>>,
    parts: VecDeque<Part>,
    planned: i32,
    next: Option<i32>,
    longest: usize,
    spare: Option<Spare>,
    from: i32,
    meta: Meta,
}

fn feed(clip: &Path, threads: usize, by: u32) -> Result<(usize, Decoder)> {
    Ok((threads, Decoder::threaded(clip, threads)?.shrink(by)))
}

struct Lane {
    clip: PathBuf,
    blend: Blend,
    size: (u32, u32),
    jobs: Arc<Jobs>,
    spare: Option<Spare>,
    from: i32,
    meta: Meta,
    by: u32,
    workers: usize,
}

fn work(lane: Lane) -> JoinHandle<()> {
    work::spawn("decoder", move || {
        let jobs = Arc::clone(&lane.jobs);
        budget::divided(lane.workers, || run(lane));
        // end the parts that no lane is left to make
        if jobs.alive.fetch_sub(1, Ordering::AcqRel) == 1 {
            let queue = jobs.queue.lock();
            queue.unwrap_or_else(|poison| poison.into_inner()).0.clear();
        }
    })
}

fn run(lane: Lane) {
    let Lane {
        clip,
        blend,
        size,
        jobs,
        spare,
        from,
        meta,
        by,
        workers: _,
    } = lane;
    let mut decoder: Option<(usize, Decoder)> = None;
    loop {
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
            match queue.0.pop_front() {
                Some(job) => break job,
                None => {
                    let woken = jobs.ready.wait(queue);
                    queue = woken.unwrap_or_else(|poison| poison.into_inner());
                }
            }
        };
        drop(queue);
        let encoder = spare.as_ref().filter(|_| packets).map(|spare| spare());
        let mut packer = match encoder {
            Some(Ok(encoder)) => Some(Packer(encoder)),
            Some(Err(error)) => {
                let _ = made.send(Err(error));
                continue;
            }
            None => None,
        };
        let (first, end) = (frames.start, frames.end);
        for n in frames {
            let picture = |decoder: &mut Option<(usize, Decoder)>| {
                if decoder.as_ref().is_none_or(|decoder| decoder.0 != threads) {
                    *decoder = Some(feed(&clip, threads, by)?);
                }
                let source = |index| match decoder {
                    Some((_, decoder)) => decoder.picture(index),
                    None => Err("no decoder".into()),
                };
                blend.frame(source, n, size.0, size.1)
            };
            let made_now = picture(&mut decoder);
            let made_now = match packer.as_mut() {
                Some(packer) => made_now.and_then(|mut picture| {
                    meta.tag(&mut picture);
                    packer.pack(picture, n - from, n == first, n + 1 == end)
                }),
                None => made_now.map(Made::Picture),
            };
            if made.send(made_now).is_err() {
                break;
            }
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
        let useful = budget::useful(height).min(cores);
        let held = math.window() + PAST + REFERENCES + useful;
        // a clip that does not fit still runs with one lane and few frames in flight
        let lanes = (budget / 2 / (held * picture)).max(1);
        let most = (cores / useful).clamp(1, lanes);
        Ok(Self {
            clip: clip.to_owned(),
            size: (width, height),
            frames: math.frames,
            blend: Box::new(blend),
            probe,
            math,
            cores,
            most,
            room: (budget / 2 / picture).max(2),
            jobs: Arc::default(),
            started: 0,
            lanes: Vec::new(),
            closing: Vec::new(),
            parts: VecDeque::new(),
            planned: 0,
            next: None,
            longest: 0,
            spare: None,
            from: 0,
            meta,
        })
    }

    pub fn encode(&mut self, spare: Spare, from: i32, end: i32) {
        self.spare = Some(spare);
        self.from = from;
        self.frames = self.frames.min(end);
    }

    fn packing(&self) -> bool {
        self.spare.is_some()
    }

    fn end(&mut self, start: i32) -> i32 {
        if self.most == 1 && !self.packing() {
            return start.saturating_add(PAST as i32).min(self.frames);
        }
        // each part starts with a key frame, so short parts make the file large
        let mut step = match self.packing() {
            true => GOP as i32 / 2,
            false => PAST as i32,
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

    fn lane(&mut self) {
        self.started += 1;
        self.jobs.alive.fetch_add(1, Ordering::AcqRel);
        self.lanes.push(work(Lane {
            clip: self.clip.clone(),
            blend: (self.blend)(),
            size: self.size,
            jobs: Arc::clone(&self.jobs),
            spare: self.spare.clone(),
            from: self.from,
            meta: self.meta,
            by: self.probe.by(),
            workers: self.most,
        }));
    }

    fn plan(&mut self) {
        while self.planned < self.frames {
            let start = self.planned;
            let end = match self.next {
                Some(end) => end,
                None => self.end(start),
            };
            self.next = Some(end);
            // the last part has no part to run beside it
            let length = (end - start) as usize;
            let lanes = if end == self.frames && length > 2 * self.longest {
                0
            } else {
                self.longest = self.longest.max(length);
                let fit = match self.packing() {
                    true => self.most,
                    false => self.room / self.longest,
                };
                fit.clamp(1, self.most)
            };
            // a part per lane and one more for a lane that is done
            let open = match self.packing() {
                true => lanes + 1,
                false => (self.room / self.longest.max(1)).min(lanes + 1).max(1),
            };
            if self.parts.len() >= if lanes == 0 { 1 } else { open } {
                break;
            }
            self.next = None;
            // 0 lanes: one decoder makes the part with all threads
            let (threads, room) = match lanes {
                0 => (self.cores, 2),
                lanes => (
                    self.cores.div_ceil(lanes),
                    (end - start) as usize * (lanes - 1) / lanes,
                ),
            };
            let (made, pictures) = work::channel(room.clamp(2, self.room));
            if self.started < lanes.max(1) {
                self.lane();
            }
            let packets = lanes > 0 && self.packing();
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
                read: false,
            });
            self.planned = end;
        }
    }

    pub fn lanes(&self) -> usize {
        self.started
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
        self.started = 0;
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
            if self.started > 1 {
                self.stop();
            }
            self.parts.clear();
            (self.planned, self.next, self.longest) = (n, None, 0);
        }
        loop {
            self.plan();
            let part = self.parts.front_mut().ok_or("no frame")?;
            if part.frames.is_empty() {
                self.parts.pop_front();
                continue;
            }
            if !part.read {
                // the reader takes this part as it comes, only later parts need room
                part.made.narrow(2);
                part.read = true;
            }
            let picture = match part.made.try_recv() {
                Some(picture) => picture,
                None => part.made.recv(cancel)?.ok_or("the decoder stopped")?,
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
        self.stop();
        for closer in self.closing.drain(..) {
            let _ = closer.join();
        }
    }
}
