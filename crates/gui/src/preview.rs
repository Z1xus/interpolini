use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, SyncSender, channel, sync_channel};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use interpolini_core::budget::{self, Class};
use interpolini_core::config::Config;
use interpolini_core::work::{self, Cancel};
use interpolini_core::{
    Canvas, Event, Fade, Graph, Image, Job, Place, Sound, Source, Sources, estimate, peaks,
};
use slint::{Model, Rgba8Pixel, SharedPixelBuffer, Weak};

use crate::view::clock;
use crate::{App, services};

const WIDTH: u32 = 1280;
const SHOWN_FPS: f64 = 60.0;
const AHEAD: usize = 8;
const LEAD: Duration = Duration::from_millis(50);
const LATE: Duration = Duration::from_millis(100);
// the longest wait before a stop is seen
const POLL: Duration = Duration::from_millis(25);
// the seconds before the end of a stretch when the clips of the next one open
const EARLY: f64 = 2.0;
pub const THUMBS: usize = 8;
const WAVE: usize = 120;
// the rows of a wave, which the window stretches to the height of the track
const STRIP: u32 = 64;

pub struct Layer {
    pub clip: PathBuf,
    pub config: Config,
    pub still: bool,
    pub place: Place,
    // the time in the source that shows first
    pub start: f64,
    pub inside: f64,
    pub length: f64,
    pub fade: Fade,
    pub fills: bool,
}

pub struct Stretch {
    // the clips that show, the lowest one first
    pub layers: Vec<Layer>,
    // the time on the timeline where it starts to show, and the time where other clips take over
    pub time: f64,
    pub end: f64,
}

pub struct Show {
    // a playback has all stretches up to a gap, a paused picture has one
    pub parts: Vec<Stretch>,
    pub canvas: PathBuf,
    pub play: bool,
    // nothing shows, the clips get ready during a gap
    pub quiet: bool,
    pub original: bool,
    pub width: u32,
    pub size: (u32, u32),
    pub total: f64,
    // a frame to save at full size, the original or the result
    pub save: Option<bool>,
}

pub struct Measure {
    pub jobs: Vec<Job>,
    pub sounds: Option<Vec<Sound>>,
    pub done: Box<dyn FnOnce(Option<u64>) + Send>,
}

type Pixels = SharedPixelBuffer<Rgba8Pixel>;

struct Frame {
    moved: f64,
    image: Pixels,
    before: Option<Pixels>,
    details: String,
    fps: f64,
    step: f64,
}

pub fn source(config: &Config) -> Config {
    let mut config = config.clone();
    config.dedup.enabled = false;
    config.pre.enabled = false;
    config.interpolation.enabled = false;
    config.blending.enabled = false;
    config.color.enabled = false;
    config
}

// the copy is made here, so that the window thread only wraps the pixels
fn pixels(image: &Image) -> Pixels {
    let mut pixels = Pixels::new(image.width, image.height);
    pixels.make_mut_bytes().copy_from_slice(&image.rgba);
    pixels
}

fn strip(peaks: &[f32]) -> Pixels {
    let mut pixels = Pixels::new(peaks.len() as u32, STRIP);
    let bytes = pixels.make_mut_bytes();
    for (column, peak) in peaks.iter().enumerate() {
        let rows = ((peak.clamp(0.0, 1.0) * STRIP as f32 / 2.0).round() as usize).max(1) * 2;
        for row in (STRIP as usize - rows) / 2..(STRIP as usize + rows) / 2 {
            let at = (row * peaks.len() + column) * 4;
            bytes[at..at + 4].copy_from_slice(&[255; 4]);
        }
    }
    pixels
}

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poison| poison.into_inner())
}

type Key = (PathBuf, Config);

// the frame goes next to the video on top, named by its time in that video
fn write(layers: &[Layer], image: &Image) -> Option<PathBuf> {
    let layer = layers.iter().rev().find(|layer| !layer.still);
    let layer = layer.or(layers.last())?;
    let stem = layer.clip.file_stem()?.to_string_lossy();
    let name = format!("{stem} {}.png", clock(layer.start).replace(':', "-"));
    let path = layer.clip.with_file_name(name);
    let file = BufWriter::new(File::create(&path).ok()?);
    let mut encoder = png::Encoder::new(file, image.width, image.height);
    encoder.set_color(png::ColorType::Rgba);
    let mut writer = encoder.write_header().ok()?;
    writer.write_image_data(&image.rgba).ok()?;
    writer.finish().ok()?;
    Some(path)
}

fn plain(layer: &Layer) -> Config {
    match layer.still {
        true => layer.config.clone(),
        false => source(&layer.config),
    }
}

fn wanted(layers: &[Layer], original: bool) -> Vec<(&Layer, Config)> {
    let mut wanted = Vec::new();
    for layer in layers {
        wanted.push((layer, layer.config.clone()));
        if original && !layer.still {
            wanted.push((layer, plain(layer)));
        }
    }
    wanted
}

fn key((layer, config): &(&Layer, Config)) -> Key {
    (layer.clip.clone(), config.clone())
}

fn compose(
    open: &mut Sources,
    canvas: &mut Canvas,
    layers: &[Layer],
    original: bool,
    moved: f64,
    width: u32,
    cancel: &Cancel,
) -> interpolini_core::Result<Image> {
    let level = |layer: &Layer| layer.fade.level(layer.inside + moved, layer.length) as f32;
    let top = layers
        .iter()
        .rposition(|layer| layer.fills && level(layer) >= 1.0);
    let mut pictures = Vec::new();
    for layer in &layers[top.unwrap_or(0)..] {
        let config = if original {
            plain(layer)
        } else {
            layer.config.clone()
        };
        let source = open.get_mut(&layer.clip, &config);
        if let Some(Ok(picture)) = source.map(|source| source.at(layer.start + moved, cancel)) {
            pictures.push((picture, layer.place, level(layer)));
        }
    }
    canvas.preview(pictures, width)
}

// the video on top gives the details and the speed
fn details(open: &Sources, layers: &[Layer]) -> (String, f64) {
    let mut graphs = layers.iter().rev().filter_map(|layer| {
        let source = open.get(&layer.clip, &layer.config)?;
        source.graph()
    });
    let Some(graph) = graphs.next() else {
        return (String::new(), 30.0);
    };
    let info = graph.info;
    let fps = info.fps.0 as f64 / info.fps.1 as f64;
    let source = graph.source_fps();
    let details = format!(
        "{}x{}  |  {source:.0} to {fps:.0} fps",
        info.width, info.height
    );
    (details, fps)
}

fn note(ui: &Weak<App>, text: String) {
    let _ = ui.upgrade_in_event_loop(move |ui| ui.set_preview_note(text.into()));
}

fn warn(ui: &Weak<App>) -> impl Fn(Event) + Sync + '_ {
    move |event| match event {
        Event::Warning(text) => note(ui, text),
        Event::Preparing(done) => {
            let done = done * 100.0;
            note(ui, format!("Preparing TensorRT for first use, {done:.0}%"));
        }
        _ => {}
    }
}

fn fail(ui: &Weak<App>, error: interpolini_core::Error) {
    let _ = ui.upgrade_in_event_loop(move |ui| {
        ui.set_failed(true);
        ui.set_status(error.to_string().into());
        ui.set_playing(false);
        ui.set_preview_note("".into());
    });
}

fn post(
    ui: &Weak<App>,
    cancel: &Cancel,
    frame: Frame,
    text: String,
    position: Option<f32>,
    first: bool,
) {
    let Frame {
        image,
        before,
        details,
        fps,
        ..
    } = frame;
    let cancel = cancel.clone();
    let _ = ui.upgrade_in_event_loop(move |ui| {
        if cancel.stopped() {
            return;
        }
        ui.set_preview(slint::Image::from_rgba8(image));
        if let Some(before) = before {
            ui.set_original(slint::Image::from_rgba8(before));
        }
        // a copy keeps the frame rate
        let rate = details.split_once(" to ").filter(|_| ui.get_copying());
        let details = match rate {
            Some((size, _)) => format!("{size} fps"),
            None => details,
        };
        ui.set_details(details.into());
        ui.set_tick((1.0 / fps) as f32);
        ui.set_preview_note(text.into());
        if let Some(position) = position.filter(|_| ui.get_playing()) {
            ui.set_position(position);
            ui.invoke_moved();
            if first {
                ui.invoke_sounding();
            }
        }
    });
}

pub struct Worker {
    ui: Weak<App>,
    open: Sources,
    canvas: Option<(PathBuf, Canvas)>,
    recent: Vec<Key>,
}

struct Play {
    ui: Weak<App>,
    parts: Vec<Stretch>,
    original: bool,
    total: f64,
    width: u32,
    ahead: usize,
    held: Vec<Key>,
}

// the most pixels of a picture, with the copies of it, that the memory of the preview holds
fn widest(size: (u32, u32)) -> u32 {
    let pixels = budget::bytes(Class::Interactive) / 48;
    let (wide, high) = (f64::from(size.0.max(1)), f64::from(size.1.max(1)));
    ((pixels as f64 * wide / high).sqrt() as u32).max(WIDTH)
}

fn ahead(size: (u32, u32), original: bool) -> usize {
    let (wide, high) = (u64::from(size.0.max(1)), u64::from(size.1.max(1)));
    let frame = u64::from(WIDTH) * (u64::from(WIDTH) * high / wide + 1) * 4;
    let copies = if original { 2 } else { 1 };
    let room = budget::bytes(Class::Interactive) as u64 / 8 / (frame * copies);
    (room as usize).clamp(2, AHEAD)
}

impl Worker {
    pub fn new(ui: Weak<App>) -> Self {
        Self {
            ui,
            open: Sources::default(),
            canvas: None,
            recent: Vec::new(),
        }
    }

    fn show(&mut self, show: Show, cancel: &Cancel) -> Option<Play> {
        let Worker {
            ui,
            open,
            canvas,
            recent,
        } = self;
        let Show {
            parts,
            canvas: base,
            play,
            quiet,
            original,
            width,
            size,
            total,
            save,
        } = show;
        let Some(first) = parts.first() else {
            // a render gets all of the gpu
            *open = Sources::default();
            recent.clear();
            note(ui, String::new());
            return None;
        };
        if !quiet {
            note(ui, "Rendering…".into());
        }
        let original = original || save == Some(true);
        // the old graphs must free the gpu first
        let near = parts.iter().take(2);
        let near = near.flat_map(|stretch| wanted(&stretch.layers, original));
        let mut near: Vec<Key> = near.map(|wanted| key(&wanted)).collect();
        near.append(recent);
        drop(open.split(|clip, config| near.iter().any(|key| key.0 == clip && key.1 == *config)));
        let shown = wanted(&first.layers, original);
        *recent = shown.iter().map(key).collect();
        let warn = warn(ui);
        let mut failed = None;
        for (layer, config) in &shown {
            if let Err(error) = open.open(&layer.clip, config, layer.still, &warn, cancel) {
                failed = Some(error);
            }
        }
        if cancel.stopped() {
            return None;
        }
        if quiet {
            for (layer, config) in &shown {
                if let Some(source) = open.get_mut(&layer.clip, config) {
                    let _ = source.at(layer.start, cancel);
                }
            }
            return None;
        }
        if !matches!(canvas, Some(canvas) if canvas.0 == base) {
            *canvas = match Canvas::open(&base) {
                Ok(canvas) => Some((base, canvas)),
                Err(error) => {
                    failed = Some(error);
                    None
                }
            };
        }
        if let Some(error) = failed {
            fail(ui, error);
            return None;
        }
        let (_, canvas) = canvas.as_mut()?;
        let width = width.min(widest(size));
        let layers = &first.layers;
        if let Some(original) = save {
            let image = compose(open, canvas, layers, original, 0.0, u32::MAX, cancel);
            let path = image.ok().and_then(|image| write(layers, &image));
            let _ = ui.upgrade_in_event_loop(move |ui| match path {
                Some(path) => {
                    ui.set_preview_note("Saved".into());
                    ui.set_shot(path.to_string_lossy().as_ref().into());
                    ui.set_flash(true);
                }
                None => ui.set_preview_note("Could not save the frame".into()),
            });
            return None;
        }
        if play {
            return Some(Play {
                ui: ui.clone(),
                original,
                total,
                width: width.min(WIDTH),
                ahead: ahead(size, original),
                held: open.keys(),
                parts,
            });
        }
        let image = compose(open, canvas, layers, false, 0.0, width, cancel).ok()?;
        let before = original.then(|| compose(open, canvas, layers, true, 0.0, width, cancel));
        let (details, fps) = details(open, layers);
        let frame = Frame {
            moved: 0.0,
            image: pixels(&image),
            before: before.and_then(|before| Some(pixels(&before.ok()?))),
            details,
            fps,
            step: 1.0 / fps,
        };
        post(ui, cancel, frame, String::new(), None, false);
        None
    }
}

fn run(worker: &Arc<Mutex<Worker>>, show: Show, cancel: &Cancel) {
    let play = locked(worker).show(show, cancel);
    if let Some(play) = play {
        playback(worker, play, cancel);
    }
}

pub fn show(show: Show) {
    services::with(|services| {
        let worker = Arc::clone(&services.worker);
        services
            .preview
            .submit(move |cancel| run(&worker, show, cancel));
    });
}

pub fn measure(Measure { jobs, sounds, done }: Measure) {
    services::with(|services| {
        services.measure.submit(move |cancel| {
            let mut open = Sources::default();
            let bytes = estimate(&jobs, sounds.as_deref(), cancel, &mut open).ok();
            if !cancel.stopped() {
                done(bytes);
            }
        });
    });
}

#[derive(Default)]
struct Reach {
    current: AtomicUsize,
    made: AtomicU64,
    wake: (Mutex<()>, Condvar),
}

impl Reach {
    fn notify(&self) {
        drop(locked(&self.wake.0));
        self.wake.1.notify_all();
    }
}

// the next clips open while this stretch plays, so the picture does not stop
fn open_next(
    ui: &Weak<App>,
    parts: &[Stretch],
    (original, held): (bool, &[Key]),
    reach: &Reach,
    stop: &Cancel,
    opened: &Sender<interpolini_core::Result<(Key, Source)>>,
) {
    let warn = warn(ui);
    for (index, stretch) in parts.iter().enumerate().skip(1) {
        let from = parts[index - 1].end - EARLY;
        let early = || {
            let made = f64::from_bits(reach.made.load(Ordering::Relaxed));
            reach.current.load(Ordering::Relaxed) + 1 < index || made < from
        };
        let mut waiting = locked(&reach.wake.0);
        while early() && !stop.stopped() {
            waiting = reach
                .wake
                .1
                .wait(waiting)
                .unwrap_or_else(|p| p.into_inner());
        }
        drop(waiting);
        let mut there: Vec<Key> = match index {
            1 => held.to_vec(),
            _ => wanted(&parts[index - 1].layers, original)
                .iter()
                .map(key)
                .collect(),
        };
        for wanted in wanted(&stretch.layers, original) {
            let key = key(&wanted);
            if there.contains(&key) {
                continue;
            }
            if stop.stopped() {
                return;
            }
            // the first picture of a clip takes long, so it is made here
            let layer = wanted.0;
            let source = Source::open(&key.0, &key.1, layer.still, &warn, stop)
                .and_then(|mut source| source.at(layer.start, stop).map(|_| source));
            there.push(key.clone());
            if opened.send(source.map(|source| (key, source))).is_err() {
                return;
            }
        }
    }
}

// in order: the host is slow after a picture that is left out
fn compose_all(
    worker: &Mutex<Worker>,
    play: (&Weak<App>, &[Stretch], bool, u32),
    reach: &Reach,
    stop: &Cancel,
    ready: &Receiver<interpolini_core::Result<(Key, Source)>>,
    pictures: &SyncSender<Frame>,
) {
    let (ui, parts, original, width) = play;
    let mut worker = locked(worker);
    let Worker { open, canvas, .. } = &mut *worker;
    let Some((_, canvas)) = canvas else {
        return;
    };
    'play: for (index, stretch) in parts.iter().enumerate() {
        let needs = wanted(&stretch.layers, original);
        if index > 0 {
            let missing = |open: &Sources| {
                let there =
                    |(layer, config): &(&Layer, Config)| open.get(&layer.clip, config).is_some();
                !needs.iter().all(there)
            };
            while missing(open) {
                match ready.recv() {
                    Ok(Ok(((clip, config), source))) => open.add(clip, config, source),
                    Ok(Err(error)) => {
                        if !stop.stopped() {
                            fail(ui, error);
                        }
                        break 'play;
                    }
                    Err(_) => break 'play,
                }
            }
            let needed = |clip: &Path, config: &Config| {
                needs
                    .iter()
                    .any(|(layer, wanted)| layer.clip == clip && wanted == config)
            };
            services::close(open.split(needed));
            reach.current.store(index, Ordering::Relaxed);
            reach.notify();
        }
        let (details, fps) = details(open, &stretch.layers);
        let step = (fps / SHOWN_FPS).round().max(1.0) / fps;
        let layers = &stretch.layers;
        for moved in (0..).map(|index| f64::from(index) * step) {
            let Ok(image) = compose(open, canvas, layers, false, moved, width, stop) else {
                break 'play;
            };
            let before = original
                .then(|| compose(open, canvas, layers, true, moved, width, stop).ok())
                .flatten();
            let made = (stretch.time + moved).to_bits();
            reach.made.store(made, Ordering::Relaxed);
            reach.notify();
            let sent = pictures.send(Frame {
                moved: stretch.time + moved - parts[0].time,
                image: pixels(&image),
                before: before.map(|before| pixels(&before)),
                details: details.clone(),
                fps,
                step,
            });
            if sent.is_err() {
                break 'play;
            }
            if stretch.time + moved + step >= stretch.end {
                break;
            }
        }
    }
}

fn pause(cancel: &Cancel, until: Instant) -> bool {
    loop {
        if cancel.stopped() {
            return false;
        }
        let left = until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return true;
        }
        std::thread::sleep(left.min(POLL));
    }
}

fn show_frames(
    ui: &Weak<App>,
    queue: &Receiver<Frame>,
    cancel: &Cancel,
    (time, total): (f64, f64),
) -> bool {
    let mut clock: Option<(Instant, f64)> = None;
    let mut started = None;
    let mut shown = 0;
    loop {
        let frame = match queue.recv_timeout(POLL) {
            Ok(frame) => frame,
            Err(RecvTimeoutError::Timeout) if !cancel.stopped() => continue,
            Err(RecvTimeoutError::Timeout) => return false,
            Err(RecvTimeoutError::Disconnected) => return !cancel.stopped(),
        };
        let due = clock.map(|(at, from)| at + Duration::from_secs_f64(frame.moved - from));
        // the sound starts with the first picture, and again after pictures that are too late
        let first = due.is_none_or(|due| due + LATE < Instant::now());
        let due = match first {
            true => Instant::now() + LEAD,
            false => due.unwrap_or_else(Instant::now),
        };
        if first {
            clock = Some((due, frame.moved));
        }
        if !pause(cancel, due) {
            return false;
        }
        let started: Instant = *started.get_or_insert_with(Instant::now);
        let position = ((time + frame.moved) / total) as f32;
        let speed = f64::from(shown) / started.elapsed().as_secs_f64().max(0.001);
        let rate = 1.0 / frame.step;
        let text = match shown > 0 {
            true => format!("{:.0} of {rate:.0} fps", speed.min(rate)),
            false => String::new(),
        };
        post(ui, cancel, frame, text, Some(position), first);
        shown += 1;
    }
}

fn playback(worker: &Arc<Mutex<Worker>>, play: Play, cancel: &Cancel) {
    let Play {
        ui,
        parts,
        original,
        total,
        width,
        ahead,
        held,
    } = play;
    let time = parts[0].time;
    let parts = Arc::new(parts);
    let (pictures, queue) = sync_channel(ahead);
    let (opened, ready) = channel();
    let (reach, stop) = (Arc::new(Reach::default()), Cancel::default());
    budget::preview(true);
    let opener = work::spawn("opener", {
        let (ui, parts, reach, stop) = (
            ui.clone(),
            Arc::clone(&parts),
            Arc::clone(&reach),
            stop.clone(),
        );
        move || open_next(&ui, &parts, (original, &held), &reach, &stop, &opened)
    });
    let composer = work::spawn("composer", {
        let (ui, worker, parts) = (ui.clone(), Arc::clone(worker), Arc::clone(&parts));
        let (reach, stop) = (Arc::clone(&reach), stop.clone());
        move || {
            let play = (&ui, &parts[..], original, width);
            compose_all(&worker, play, &reach, &stop, &ready, &pictures)
        }
    });
    let finished = show_frames(&ui, &queue, cancel, (time, total));
    stop.stop();
    drop(queue);
    reach.notify();
    let _ = opener.join();
    let _ = composer.join();
    budget::preview(false);
    if finished {
        let _ = ui.upgrade_in_event_loop(|ui| {
            ui.set_preview_note("".into());
            ui.invoke_ended();
        });
    }
}

// places gives the rows of the clips of one file, with the audio track of each
pub fn thumbs(
    ui: Weak<App>,
    key: &str,
    clip: PathBuf,
    config: Config,
    places: impl Fn() -> Vec<(Option<usize>, usize)> + Send + Clone + 'static,
) {
    services::background(key, move |cancel| {
        let Ok(graph) = Graph::open(&clip, &source(&config), &|_| {}, cancel) else {
            return;
        };
        for thumb in 0..THUMBS {
            let frame = (thumb * graph.info.frames as usize / THUMBS) as i32;
            let Ok(image) = graph.preview(frame, 160, cancel) else {
                return;
            };
            let (places, pixels, cancel) = (places.clone(), pixels(&image), cancel.clone());
            let _ = ui.upgrade_in_event_loop(move |ui| {
                // the clip can move or go away while its thumbnails are made
                if cancel.stopped() {
                    return;
                }
                let rows = ui.get_clips();
                let shown = slint::Image::from_rgba8(pixels);
                let video = places().into_iter().filter(|place| place.0.is_none());
                for row in video.filter_map(|place| rows.row_data(place.1)) {
                    row.thumbs.set_row_data(thumb, shown.clone());
                }
            });
        }
    });
}

// compares the frames of a clip for the repeats, which takes a moment
pub fn differences(
    ui: Weak<App>,
    key: &str,
    clip: PathBuf,
    part: (f64, f64),
    done: impl FnOnce(&App, Vec<Vec<f32>>) + Send + 'static,
) {
    services::background(key, move |cancel| {
        if let Ok(differences) = interpolini_core::differences(&clip, part.0, part.1, cancel) {
            let _ = ui.upgrade_in_event_loop(move |ui| done(&ui, differences));
        }
    });
}

pub fn waves(
    ui: Weak<App>,
    key: &str,
    clip: PathBuf,
    seconds: f64,
    places: impl Fn() -> Vec<(Option<usize>, usize)> + Send + 'static,
) {
    services::background(key, move |cancel| {
        let Ok(tracks) = peaks(&clip, seconds, WAVE, cancel) else {
            return;
        };
        let tracks: Vec<Pixels> = tracks.iter().map(|wave| strip(wave)).collect();
        let _ = ui.upgrade_in_event_loop(move |ui| {
            let rows = ui.get_clips();
            for (audio, index) in places() {
                let wave = audio.and_then(|audio| tracks.get(audio));
                if let (Some(wave), Some(mut row)) = (wave, rows.row_data(index)) {
                    row.wave = slint::Image::from_rgba8(wave.clone());
                    rows.set_row_data(index, row);
                }
            }
        });
    });
}
