use std::collections::BTreeMap;
use std::ops::{Deref, DerefMut};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime};

use ffmpeg_next::codec::threading;
use ffmpeg_next::format::Pixel;
use ffmpeg_next::{Packet, Rational, codec, color, decoder, encoder, frame, picture};

use crate::budget::{self, Class};
use crate::compose::share;
use crate::config::{Config, Effort, Masking, Output};
use crate::decode::Meta;
use crate::encode::{self, OPENS};
use crate::graph::{Graph, Info};
use crate::work::{self, Cancel};

pub const PIECE: i32 = 30;
const ROOM: usize = 8;
const IDLE: Duration = Duration::from_millis(100);
// slices let the decoder use threads without a delay
const SLICES: usize = 8;
// the quantizer, at least 45 db luma psnr on the test clips
const QUALITY: u32 = 20;
// the time between two tries to give a frame to a busy encoder
const BUSY: Duration = Duration::from_millis(2);

struct Piece {
    codec: codec::Id,
    packets: Arc<Vec<Packet>>,
    used: u64,
}

#[derive(Clone, Copy)]
pub(crate) struct Form {
    meta: Meta,
    pub(crate) info: Info,
    // a clip faster than the preview keeps only the frames that the preview shows
    pub(crate) every: i32,
}

impl Form {
    fn fps(&self) -> f64 {
        self.info.fps.0 as f64 / self.info.fps.1 as f64 / f64::from(self.every)
    }

    fn frames(&self) -> i32 {
        (self.info.frames - 1) / self.every + 1
    }

    pub(crate) fn source_fps(&self) -> f64 {
        self.meta.fps.0 as f64 / self.meta.fps.1 as f64
    }
}

struct Entry {
    clip: PathBuf,
    modified: Option<SystemTime>,
    config: Config,
    form: Form,
    pieces: BTreeMap<i32, Piece>,
}

struct Store {
    on: bool,
    epoch: u64,
    clock: u64,
    bytes: usize,
    entries: Vec<Entry>,
    kept: Vec<(PathBuf, Config)>,
}

static STORE: Mutex<Store> = Mutex::new(Store {
    on: true,
    epoch: 0,
    clock: 0,
    bytes: 0,
    entries: Vec::new(),
    kept: Vec::new(),
});
static CHANGES: AtomicU64 = AtomicU64::new(0);

fn store() -> MutexGuard<'static, Store> {
    STORE.lock().unwrap_or_else(|poison| poison.into_inner())
}

fn size(packets: &[Packet]) -> usize {
    packets.iter().map(Packet::size).sum()
}

impl Store {
    fn entry(&mut self, clip: &Path, config: &Config) -> Option<&mut Entry> {
        let held = |entry: &&mut Entry| entry.clip == clip && entry.config == *config;
        self.entries.iter_mut().find(held)
    }

    fn kept(&self, entry: &Entry) -> bool {
        let same =
            |(clip, config): &(PathBuf, Config)| entry.clip == *clip && entry.config == *config;
        self.kept.iter().any(same)
    }

    fn count(&mut self) {
        let pieces = self.entries.iter().flat_map(|entry| entry.pieces.values());
        self.bytes = pieces.map(|piece| size(&piece.packets)).sum();
        self.epoch += 1;
        CHANGES.fetch_add(1, Ordering::Relaxed);
    }

    fn has(&mut self, clip: &Path, config: &Config, start: i32) -> bool {
        let entry = self.entry(clip, config);
        entry.is_some_and(|entry| entry.pieces.contains_key(&start))
    }

    fn insert(
        &mut self,
        (clip, modified): (&Path, Option<SystemTime>),
        config: &Config,
        form: Form,
        start: i32,
        piece: Piece,
    ) {
        self.bytes += size(&piece.packets);
        if self.entry(clip, config).is_none() {
            self.entries.push(Entry {
                clip: clip.to_owned(),
                modified,
                config: config.clone(),
                form,
                pieces: BTreeMap::new(),
            });
        }
        let entry = self.entry(clip, config).expect("an entry");
        if let Some(old) = entry.pieces.insert(start, piece) {
            self.bytes -= size(&old.packets);
        }
        self.trim();
    }

    // the pieces that the timeline does not use go first, then the ones used longest ago
    fn trim(&mut self) {
        let limit = budget::bytes(Class::Interactive) / 2;
        while self.bytes > limit {
            let pieces = self.entries.iter().enumerate().flat_map(|(index, entry)| {
                let (kept, pieces) = (self.kept(entry), entry.pieces.iter());
                pieces.map(move |(start, piece)| (kept, piece.used, index, *start))
            });
            let Some((_, _, index, start)) = pieces.min() else {
                break;
            };
            let pieces = &mut self.entries[index].pieces;
            if let Some(old) = pieces.remove(&start) {
                self.bytes -= size(&old.packets);
            }
            if pieces.is_empty() {
                self.entries.remove(index);
            }
        }
        CHANGES.fetch_add(1, Ordering::Relaxed);
    }
}

pub fn enable(on: bool) {
    let mut store = store();
    store.on = on;
    if !on {
        (store.entries, store.bytes) = (Vec::new(), 0);
        store.epoch += 1;
    }
    // a lower memory limit applies at once
    store.trim();
    CHANGES.fetch_add(1, Ordering::Relaxed);
}

pub fn spans(clip: &Path, config: &Config) -> Vec<(f64, f64)> {
    let mut store = store();
    let Some(entry) = store.entry(clip, config) else {
        return Vec::new();
    };
    let mut spans: Vec<(i32, i32)> = Vec::new();
    for (start, piece) in &entry.pieces {
        let end = start + piece.packets.len() as i32;
        match spans.last_mut() {
            Some(last) if last.1 == *start => last.1 = end,
            _ => spans.push((*start, end)),
        }
    }
    let seconds = |n: i32| f64::from(n) / entry.form.fps();
    spans
        .into_iter()
        .map(|(from, to)| (seconds(from), seconds(to)))
        .collect()
}

pub fn changes() -> u64 {
    CHANGES.load(Ordering::Relaxed)
}

pub fn forget(image: &str) {
    let mut store = store();
    let stale =
        |config: &Config| config.mask.method == Masking::Image && config.mask.image == image;
    store.entries.retain(|entry| !stale(&entry.config));
    store.count();
}

pub fn enabled() -> bool {
    store().on
}

// the clips of the timeline with their settings, their pieces are the last to go
pub fn keep(clips: Vec<(PathBuf, Config)>) {
    store().kept = clips;
    CHANGES.fetch_add(1, Ordering::Relaxed);
}

// false when a new piece can only take the place of one that the timeline uses
pub fn room() -> bool {
    let store = store();
    let pieces = || store.entries.iter().flat_map(|entry| entry.pieces.values());
    let largest = pieces().map(|piece| size(&piece.packets)).max();
    store.bytes + 2 * largest.unwrap_or(0) <= budget::bytes(Class::Interactive) / 2
        || store.entries.iter().any(|entry| !store.kept(entry))
}

type Writer = (SyncSender<(i32, frame::Video, u64)>, JoinHandle<()>);

fn modified(clip: &Path) -> Option<SystemTime> {
    std::fs::metadata(clip)
        .and_then(|meta| meta.modified())
        .ok()
}

pub(crate) struct Reader {
    clip: PathBuf,
    config: Config,
    pub(crate) form: Form,
    decoder: Option<decoder::Video>,
    packets: Arc<Vec<Packet>>,
    at: i32,
    shown: Option<(i32, frame::Video)>,
}

impl Reader {
    fn new(clip: &Path, config: &Config, form: Form) -> Self {
        Self {
            clip: clip.to_owned(),
            config: config.clone(),
            form,
            decoder: None,
            packets: Arc::default(),
            at: -1,
            shown: None,
        }
    }

    // the pieces of a clip that is not open
    pub(crate) fn stored(clip: &Path, config: &Config) -> Option<Self> {
        let modified = modified(clip);
        let mut store = store();
        let entry = store.entry(clip, config)?;
        (entry.modified == modified).then(|| Self::new(clip, config, entry.form))
    }

    pub(crate) fn index(&self, seconds: f64) -> i32 {
        let n = (seconds * self.form.fps() * f64::from(self.form.every)).round() as i32;
        (n / self.form.every).clamp(0, self.form.frames() - 1)
    }

    pub(crate) fn has(&self, n: i32) -> bool {
        let mut store = store();
        store.clock += 1;
        let clock = store.clock;
        let entry = store.entry(&self.clip, &self.config);
        let piece = entry.and_then(|entry| entry.pieces.get_mut(&(n - n % PIECE)));
        piece.map(|piece| piece.used = clock).is_some()
    }

    pub(crate) fn missing(&self, from: f64, to: f64) -> Option<(f64, f64)> {
        let (fps, frames) = (self.form.fps(), self.form.frames());
        let end = ((to * fps).ceil() as i32).min(frames);
        let mut store = store();
        if !store.on {
            return None;
        }
        let mut has = |start: &i32| store.has(&self.clip, &self.config, *start);
        let from = self.index(from) / PIECE * PIECE;
        let first = (from..end)
            .step_by(PIECE as usize)
            .find(|start| !has(start))?;
        let mut rest = (first..frames).step_by(PIECE as usize);
        let last = rest.find(|start| *start >= end || has(start));
        let last = last.unwrap_or(frames);
        Some((f64::from(first) / fps, f64::from(last) / fps))
    }

    pub(crate) fn read(&mut self, n: i32) -> Option<frame::Video> {
        let first = n - n % PIECE;
        let (id, packets) = {
            let mut store = store();
            store.clock += 1;
            let clock = store.clock;
            let piece = store
                .entry(&self.clip, &self.config)?
                .pieces
                .get_mut(&first)?;
            piece.used = clock;
            (piece.codec, Arc::clone(&piece.packets))
        };
        let kept = Arc::ptr_eq(&packets, &self.packets);
        if let Some((shown, picture)) = &self.shown
            && kept
            && *shown == n
        {
            return Some(share(picture));
        }
        if self
            .decoder
            .as_ref()
            .is_none_or(|decoder| decoder.id() != id)
        {
            let mut context = codec::Context::new_with_codec(decoder::find(id)?);
            context.set_flags(codec::Flags::LOW_DELAY);
            context.set_threading(threading::Config {
                kind: threading::Type::Slice,
                count: budget::own().min(SLICES),
            });
            (self.decoder, self.at) = (context.decoder().video().ok(), -1);
        }
        let decoder = self.decoder.as_mut()?;
        // a piece can come from another encoder with the same codec
        if !(kept && (first..=n).contains(&self.at)) {
            decoder.flush();
            self.at = first;
        }
        self.packets = Arc::clone(&packets);
        // a failure starts the next read at its piece
        let mut at = std::mem::replace(&mut self.at, -1);
        let mut picture = frame::Video::empty();
        while at <= n {
            decoder
                .send_packet(packets.get((at - first) as usize)?)
                .ok()?;
            decoder.receive_frame(&mut picture).ok()?;
            at += 1;
        }
        self.at = at;
        let Meta { width, height, .. } = self.form.meta;
        if (picture.format(), picture.width(), picture.height())
            != (self.form.meta.pixel(), width, height)
        {
            return None;
        }
        self.form.meta.tag(&mut picture);
        self.shown = Some((n, share(&picture)));
        Some(picture)
    }
}

pub(crate) struct Cache {
    reader: Reader,
    modified: Option<SystemTime>,
    writer: Option<Writer>,
    next: Option<i32>,
    epoch: u64,
}

impl Deref for Cache {
    type Target = Reader;

    fn deref(&self) -> &Reader {
        &self.reader
    }
}

impl DerefMut for Cache {
    fn deref_mut(&mut self) -> &mut Reader {
        &mut self.reader
    }
}

impl Cache {
    pub(crate) fn new(clip: &Path, config: &Config, graph: &Graph) -> Option<Self> {
        if graph.plain() {
            return None;
        }
        let modified = modified(clip);
        let (num, den) = graph.info.fps;
        let every = match config.shown > 0.0 {
            true => ((num as f64 / den as f64 / config.shown + 0.01) as i32).max(1),
            false => 1,
        };
        let form = Form {
            meta: graph.meta,
            info: graph.info,
            every,
        };
        let mut store = store();
        let stale = |entry: &Entry| entry.clip == clip && entry.modified != modified;
        if store.entries.iter().any(stale) {
            store.entries.retain(|entry| !stale(entry));
            store.count();
        }
        Some(Self {
            reader: Reader::new(clip, config, form),
            modified,
            writer: None,
            next: None,
            epoch: 0,
        })
    }

    // false when the writer is gone, a stop request ends the wait for room
    pub(crate) fn write(&mut self, n: i32, picture: &frame::Video, wait: Option<&Cancel>) -> bool {
        if self.next == Some(n + 1) {
            return true;
        }
        let follows = self.next == Some(n) && n % PIECE != 0;
        let starts = n % PIECE == 0 && {
            let mut store = store();
            self.epoch = store.epoch;
            store.on && !store.has(&self.reader.clip, &self.reader.config, n)
        };
        self.next = None;
        if !follows && !starts {
            return true;
        }
        let Reader {
            clip, config, form, ..
        } = &self.reader;
        let writer = self
            .writer
            .get_or_insert_with(|| writer((clip.clone(), self.modified), config.clone(), *form));
        let mut frame = (n, share(picture), self.epoch);
        let sent = loop {
            match (writer.0.try_send(frame), wait) {
                (Err(TrySendError::Full(back)), Some(cancel))
                    if cancel.pause(Instant::now() + BUSY) =>
                {
                    frame = back
                }
                (sent, _) => break sent,
            }
        };
        if sent.is_ok() {
            self.next = Some(n + 1);
        }
        !matches!(sent, Err(TrySendError::Disconnected(_)))
    }
}

impl Drop for Cache {
    fn drop(&mut self) {
        if let Some((sender, thread)) = self.writer.take() {
            drop(sender);
            let _ = thread.join();
        }
    }
}

fn opened(
    meta: &Meta,
    base: Rational,
    format: Pixel,
    hardware: bool,
) -> Option<encoder::video::Encoder> {
    encode::find_encoders();
    let opens = OPENS.get()?;
    // the vulkan encoders crash when the gpu has no free memory, qsv and vaapi take no picture from memory
    let fits = |name: &&str| {
        hardware
            && !["_vulkan", "_qsv", "_vaapi"]
                .iter()
                .any(|end| name.ends_with(end))
    };
    let hardware = ["h264_", "hevc_"].into_iter().flat_map(|codec| {
        let names = opens.iter().copied().filter(fits);
        names.filter(move |name| name.starts_with(codec))
    });
    // h264 gives a full range picture as yuvj, the range comes back from the tags
    let meta = Meta {
        range: color::Range::MPEG,
        ..*meta
    };
    hardware.chain(["libx264"]).find_map(|name| {
        let options = match name {
            "libx264" => "-preset ultrafast -tune zerolatency -crf -1 -qp",
            name if name.ends_with("_nvenc") => "-tune ull -rc constqp -delay 0 -zerolatency 1 -qp",
            // amf marks no idr without it
            name if name.ends_with("_amf") => "-forced_idr 1 -global_quality",
            _ => "-global_quality",
        };
        let output = Output {
            quality: QUALITY,
            effort: Effort::Fastest,
            options: format!("{options} {QUALITY} -bf 0 -slices {SLICES}"),
            ..Output::default()
        };
        let encoder = encode::context(name, &meta, base, &output, None, false, false)?.ok()?;
        (encoder.format() == format).then_some(encoder)
    })
}

fn writer(clip: (PathBuf, Option<SystemTime>), config: Config, form: Form) -> Writer {
    let (sender, frames) = mpsc::sync_channel::<(i32, frame::Video, u64)>(ROOM);
    let thread = work::spawn("cache", move || {
        budget::enter(Class::Background);
        // a large rate fraction does not fit the i32 of a time base
        let base = Rational::from(1.0 / form.fps());
        let total = form.frames();
        let (mut encoder, mut hardware) = (None::<encoder::video::Encoder>, true);
        // some gpu encoders give packets late
        let mut open: Vec<(i32, Vec<Packet>, u64)> = Vec::new();
        let (mut packet, mut last) = (Packet::empty(), -1);
        loop {
            let received = frames.recv_timeout(IDLE);
            let place = (&clip, &config, form);
            let Ok((n, mut picture, epoch)) = received else {
                let sent =
                    |piece: &(i32, Vec<Packet>, u64)| (piece.0 + PIECE).min(total) <= last + 1;
                let Err(RecvTimeoutError::Timeout) = received else {
                    return flush(&mut encoder, &mut packet, &mut open, place);
                };
                if open.iter().any(sent) {
                    flush(&mut encoder, &mut packet, &mut open, place);
                }
                continue;
            };
            // the timestamps must rise
            if n <= last {
                flush(&mut encoder, &mut packet, &mut open, place);
            }
            if n > last + 1 {
                open.retain(|piece| (piece.0 + PIECE).min(total) <= last + 1);
            }
            last = n;
            let start = n % PIECE == 0;
            // a frame outside of a piece is not kept
            if open.is_empty() && !start {
                continue;
            }
            if encoder.is_none() {
                encoder = opened(&form.meta, base, picture.format(), hardware);
            }
            if encoder.is_none() {
                return;
            }
            if start {
                open.push((n, Vec::new(), epoch));
            }
            picture.set_kind(if start {
                picture::Type::I
            } else {
                picture::Type::None
            });
            picture.set_pts(Some(i64::from(n)));
            let send = |encoder: &mut Option<encoder::video::Encoder>| {
                encoder
                    .as_mut()
                    .is_some_and(|video| video.send_frame(&picture).is_ok())
            };
            if !send(&mut encoder) {
                // a gpu without free memory fails here, the cpu takes this frame again when it starts a piece
                if !std::mem::take(&mut hardware) {
                    return;
                }
                encoder = None;
                open.retain(|piece| piece.0 == n);
                if open.is_empty() {
                    continue;
                }
                encoder = opened(&form.meta, base, picture.format(), false);
                if !send(&mut encoder) {
                    return;
                }
            }
            let Some(video) = encoder.as_mut() else {
                return;
            };
            drain(video, &mut packet, &mut open, place);
            if open.len() > 2 {
                open.remove(0);
            }
        }
    });
    (sender, thread)
}

fn flush(
    encoder: &mut Option<encoder::video::Encoder>,
    packet: &mut Packet,
    open: &mut Vec<(i32, Vec<Packet>, u64)>,
    place: Place,
) {
    if let Some(mut video) = encoder.take()
        && video.send_eof().is_ok()
    {
        drain(&mut video, packet, open, place);
    }
    open.clear();
}

type Place<'a> = (&'a (PathBuf, Option<SystemTime>), &'a Config, Form);

fn drain(
    video: &mut encoder::video::Encoder,
    packet: &mut Packet,
    open: &mut Vec<(i32, Vec<Packet>, u64)>,
    (clip, config, form): Place,
) {
    while video.receive_packet(packet).is_ok() {
        let Some(pts) = packet.pts().and_then(|pts| i32::try_from(pts).ok()) else {
            continue;
        };
        let Some(index) = open.iter().position(|piece| piece.0 == pts - pts % PIECE) else {
            continue;
        };
        let (start, packets, _) = &mut open[index];
        if pts != *start + packets.len() as i32 {
            continue;
        }
        packets.push(std::mem::replace(packet, Packet::empty()));
        if pts + 1 < (*start + PIECE).min(form.frames()) {
            continue;
        }
        let (start, packets, epoch) = open.remove(index);
        let mut store = store();
        if packets[0].is_key() && store.on && store.epoch == epoch {
            store.clock += 1;
            let piece = Piece {
                codec: video.id(),
                packets: Arc::new(packets),
                used: store.clock,
            };
            store.insert((&clip.0, clip.1), config, form, start, piece);
        }
    }
}
