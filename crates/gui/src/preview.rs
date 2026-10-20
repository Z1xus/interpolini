use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel, sync_channel};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use interpolini_core::budget::{self, Class};
use interpolini_core::config::Config;
use interpolini_core::work::Cancel;
use interpolini_core::{
    Canvas, Event, Fade, Graph, Image, Job, Place, Sound, Source, Sources, estimate, peaks,
};
use slint::{Model, ModelRc, Rgba8Pixel, SharedPixelBuffer, VecModel, Weak};

use crate::App;
use crate::view::clock;

const WIDTH: u32 = 1280;
const SHOWN_FPS: f64 = 60.0;
const AHEAD: usize = 8;
const LEAD: Duration = Duration::from_millis(50);
const LATE: Duration = Duration::from_millis(100);
// the seconds before the end of a stretch when the clips of the next one open
const EARLY: f64 = 2.0;
pub const THUMBS: usize = 8;
const WAVE: usize = 120;

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
    pub total: f64,
    // a frame to save at full size, the original or the result
    pub save: Option<bool>,
}

pub struct Measure {
    pub jobs: Vec<Job>,
    pub sounds: Option<Vec<Sound>>,
    pub stop: Cancel,
    pub done: Box<dyn FnOnce(Option<u64>) + Send>,
}

pub enum Request {
    Show(Show),
    Measure(Measure),
}

struct Frame {
    moved: f64,
    image: Image,
    before: Option<Image>,
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

fn picture(image: Image) -> slint::Image {
    let pixels =
        SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(&image.rgba, image.width, image.height);
    slint::Image::from_rgba8(pixels)
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
        if let Some(Ok(picture)) =
            source.map(|source| source.at(layer.start + moved, &Cancel::default()))
        {
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

fn post(ui: &Weak<App>, frame: Frame, text: String, position: Option<f32>, first: bool) {
    let Frame {
        image,
        before,
        details,
        fps,
        ..
    } = frame;
    let _ = ui.upgrade_in_event_loop(move |ui| {
        ui.set_preview(picture(image));
        if let Some(before) = before {
            ui.set_original(picture(before));
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

#[derive(Default)]
struct Waiting {
    show: Option<Show>,
    measure: Option<Measure>,
}

impl Waiting {
    fn add(&mut self, request: Request) {
        match request {
            Request::Show(show) => self.show = Some(show),
            Request::Measure(measure) => self.measure = Some(measure),
        }
    }
}

struct Worker {
    ui: Weak<App>,
    open: Sources,
    canvas: Option<(PathBuf, Canvas)>,
    recent: Vec<Key>,
    close: Sender<Sources>,
}

pub fn spawn(ui: Weak<App>) -> (Sender<Request>, JoinHandle<()>) {
    let (sender, receiver) = channel::<Request>();
    let thread = std::thread::spawn(move || {
        budget::enter(Class::Interactive);
        // graphs close slowly
        let (close, closing) = channel::<Sources>();
        let closer = std::thread::spawn(move || {
            budget::enter(Class::Interactive);
            closing.into_iter().for_each(drop)
        });
        let mut worker = Worker {
            ui,
            open: Sources::default(),
            canvas: None,
            recent: Vec::new(),
            close,
        };
        let mut waiting = Waiting::default();
        loop {
            if waiting.show.is_none() && waiting.measure.is_none() {
                match receiver.recv() {
                    Ok(request) => waiting.add(request),
                    Err(_) => break,
                }
            }
            receiver.try_iter().for_each(|request| waiting.add(request));
            if let Some(show) = waiting.show.take() {
                worker.show(show, &receiver, &mut waiting);
            } else if let Some(Measure {
                jobs,
                sounds,
                stop,
                done,
            }) = waiting.measure.take()
            {
                done(estimate(&jobs, sounds.as_deref(), &stop, &mut worker.open).ok());
            }
        }
        drop(worker);
        let _ = closer.join();
    });
    (sender, thread)
}

impl Worker {
    fn show(&mut self, show: Show, receiver: &Receiver<Request>, waiting: &mut Waiting) {
        let Worker {
            ui,
            open,
            canvas,
            recent,
            close,
        } = self;
        let Show {
            parts,
            canvas: base,
            play,
            quiet,
            original,
            width,
            total,
            save,
        } = show;
        let Some(first) = parts.first() else {
            // a render gets all of the gpu
            *open = Sources::default();
            recent.clear();
            note(ui, String::new());
            return;
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
            if let Err(error) =
                open.open(&layer.clip, config, layer.still, &warn, &Cancel::default())
            {
                failed = Some(error);
            }
        }
        if quiet {
            for (layer, config) in &shown {
                if let Some(source) = open.get_mut(&layer.clip, config) {
                    let _ = source.at(layer.start, &Cancel::default());
                }
            }
            return;
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
            return fail(ui, error);
        }
        let Some((_, canvas)) = canvas else {
            return;
        };
        let layers = &first.layers;
        if let Some(original) = save {
            let image = compose(open, canvas, layers, original, 0.0, u32::MAX);
            let path = image.ok().and_then(|image| write(layers, &image));
            let _ = ui.upgrade_in_event_loop(move |ui| match path {
                Some(path) => {
                    ui.set_preview_note("Saved".into());
                    ui.set_shot(path.to_string_lossy().as_ref().into());
                    ui.set_flash(true);
                }
                None => ui.set_preview_note("Could not save the frame".into()),
            });
            return;
        }
        if play {
            let parts = (&parts[..], original, total);
            return playback(ui, open, canvas, close, parts, receiver, waiting);
        }
        let Ok(image) = compose(open, canvas, layers, false, 0.0, width) else {
            return;
        };
        let before = original.then(|| compose(open, canvas, layers, true, 0.0, width).ok());
        let (details, fps) = details(open, layers);
        let frame = Frame {
            moved: 0.0,
            image,
            before: before.flatten(),
            details,
            fps,
            step: 1.0 / fps,
        };
        post(ui, frame, String::new(), None, false);
    }
}

fn playback(
    ui: &Weak<App>,
    open: &mut Sources,
    canvas: &mut Canvas,
    close: &Sender<Sources>,
    (parts, original, total): (&[Stretch], bool, f64),
    receiver: &Receiver<Request>,
    waiting: &mut Waiting,
) {
    let time = parts[0].time;
    let warn = warn(ui);
    let (pictures, queue) = sync_channel(AHEAD);
    let (opened, ready) = channel::<interpolini_core::Result<(Key, Source)>>();
    let held = open.keys();
    let (current, stop) = (AtomicUsize::new(0), Cancel::default());
    let reached = AtomicU64::new(0);
    std::thread::scope(|scope| {
        // the next clips open while this stretch plays, so the picture does not stop
        scope.spawn(|| {
            budget::enter(Class::Interactive);
            for (index, stretch) in parts.iter().enumerate().skip(1) {
                let from = parts[index - 1].end - EARLY;
                let early = || {
                    let made = f64::from_bits(reached.load(Ordering::Relaxed));
                    current.load(Ordering::Relaxed) + 1 < index || made < from
                };
                while early() {
                    if stop.stopped() {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
                let mut there: Vec<Key> = match index {
                    1 => held.clone(),
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
                    let source = Source::open(&key.0, &key.1, layer.still, &warn, &stop)
                        .and_then(|mut source| source.at(layer.start, &stop).map(|_| source));
                    there.push(key.clone());
                    if opened.send(source.map(|source| (key, source))).is_err() {
                        return;
                    }
                }
            }
            drop(opened);
        });
        // in order: the host is slow after a picture that is left out
        scope.spawn(|| {
            budget::enter(Class::Interactive);
            let ready = ready;
            'play: for (index, stretch) in parts.iter().enumerate() {
                let needs = wanted(&stretch.layers, original);
                if index > 0 {
                    let missing = |open: &Sources| {
                        let there = |(layer, config): &(&Layer, Config)| {
                            open.get(&layer.clip, config).is_some()
                        };
                        !needs.iter().all(there)
                    };
                    while missing(open) {
                        match ready.recv() {
                            Ok(Ok(((clip, config), source))) => open.add(clip, config, source),
                            Ok(Err(error)) => {
                                fail(ui, error);
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
                    let _ = close.send(open.split(needed));
                    current.store(index, Ordering::Relaxed);
                }
                let (details, fps) = details(open, &stretch.layers);
                let step = (fps / SHOWN_FPS).round().max(1.0) / fps;
                let layers = &stretch.layers;
                for moved in (0..).map(|index| f64::from(index) * step) {
                    let Ok(image) = compose(open, canvas, layers, false, moved, WIDTH) else {
                        break 'play;
                    };
                    let before = original
                        .then(|| compose(open, canvas, layers, true, moved, WIDTH).ok())
                        .flatten();
                    let made = (stretch.time + moved).to_bits();
                    reached.store(made, Ordering::Relaxed);
                    let sent = pictures.send(Frame {
                        moved: stretch.time + moved - time,
                        image,
                        before,
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
            drop(pictures);
        });
        let mut clock: Option<(Instant, f64)> = None;
        let mut started = None;
        for (shown, frame) in queue.into_iter().enumerate() {
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
            std::thread::sleep(due.saturating_duration_since(Instant::now()));
            receiver.try_iter().for_each(|request| waiting.add(request));
            if waiting.show.is_some() {
                break;
            }
            let started: Instant = *started.get_or_insert_with(Instant::now);
            let position = ((time + frame.moved) / total) as f32;
            let speed = shown as f64 / started.elapsed().as_secs_f64().max(0.001);
            let rate = 1.0 / frame.step;
            let text = match shown > 0 {
                true => format!("{:.0} of {rate:.0} fps", speed.min(rate)),
                false => String::new(),
            };
            post(ui, frame, text, Some(position), first);
        }
        stop.stop();
    });
    if waiting.show.is_none() {
        let _ = ui.upgrade_in_event_loop(|ui| {
            ui.set_preview_note("".into());
            ui.invoke_ended();
        });
    }
}

// the work for the clips waits here: each job opens a decoder, and a thread for each one fills the memory
fn queue(job: impl FnOnce() + Send + 'static) {
    type Job = Box<dyn FnOnce() + Send>;
    static JOBS: OnceLock<Sender<Job>> = OnceLock::new();
    let jobs = JOBS.get_or_init(|| {
        let (sender, receiver) = channel::<Job>();
        let receiver = Arc::new(Mutex::new(receiver));
        for _ in 0..2 {
            let receiver = Arc::clone(&receiver);
            std::thread::spawn(move || {
                budget::enter(Class::Background);
                loop {
                    let job = receiver
                        .lock()
                        .unwrap_or_else(|poison| poison.into_inner())
                        .recv();
                    match job {
                        Ok(job) => job(),
                        Err(_) => return,
                    }
                }
            });
        }
        sender
    });
    let _ = jobs.send(Box::new(job));
}

// places gives the rows of the clips of one file, with the audio track of each
pub fn thumbs(
    ui: Weak<App>,
    clip: PathBuf,
    config: Config,
    places: impl Fn() -> Vec<(Option<usize>, usize)> + Send + Clone + 'static,
) {
    queue(move || {
        let Ok(graph) = Graph::open(&clip, &source(&config), &|_| {}, &Cancel::default()) else {
            return;
        };
        for thumb in 0..THUMBS {
            let frame = (thumb * graph.info.frames as usize / THUMBS) as i32;
            let Ok(image) = graph.preview(frame, 160, &Cancel::default()) else {
                return;
            };
            let places = places.clone();
            let _ = ui.upgrade_in_event_loop(move |ui| {
                // the clip can move or go away while its thumbnails are made
                let rows = ui.get_clips();
                let shown = picture(image);
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
    clip: PathBuf,
    part: (f64, f64),
    done: impl FnOnce(&App, Vec<Vec<f32>>) + Send + 'static,
) {
    queue(move || {
        if let Ok(differences) =
            interpolini_core::differences(&clip, part.0, part.1, &Cancel::default())
        {
            let _ = ui.upgrade_in_event_loop(move |ui| done(&ui, differences));
        }
    });
}

pub fn waves(
    ui: Weak<App>,
    clip: PathBuf,
    seconds: f64,
    places: impl Fn() -> Vec<(Option<usize>, usize)> + Send + 'static,
) {
    queue(move || {
        let Ok(tracks) = peaks(&clip, seconds, WAVE, &Cancel::default()) else {
            return;
        };
        let _ = ui.upgrade_in_event_loop(move |ui| {
            let rows = ui.get_clips();
            for (audio, index) in places() {
                let wave = audio.and_then(|audio| tracks.get(audio));
                if let (Some(wave), Some(mut row)) = (wave, rows.row_data(index)) {
                    row.wave = ModelRc::new(VecModel::from(wave.clone()));
                    rows.set_row_data(index, row);
                }
            }
        });
    });
}
