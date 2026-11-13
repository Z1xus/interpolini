use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime};

use interpolini_core::budget::{self, Class};
use interpolini_core::config::{Config, Masking};
use interpolini_core::mask;
use interpolini_core::work::{self, Cancel};
use interpolini_core::{
    Canvas, Event, Fade, Image, Job, Place, Setup, Sink, Sound, Source, Sources, estimate, peaks,
};
use slint::{Model, ModelRc, Rgba8Pixel, SharedPixelBuffer, VecModel, Weak};

use crate::project::missing;
use crate::view::clock;
use crate::{App, Note, services};

const WIDTH: u32 = 1280;
const SHOWN_FPS: f64 = 60.0;
const AHEAD: usize = 8;
const LEAD: Duration = Duration::from_millis(50);
// the sound goes on while a picture is at most this late
const LATE: Duration = Duration::from_millis(250);
// on auto size the picture holds this long before playback waits
const HOLD: Duration = Duration::from_secs(1);
// the seconds that the composer may fall behind before the size goes down
const DEBT: f64 = 0.2;
// the seconds of ready frames that let a slow composer go on at its size
const RESERVE: f64 = 1.0;
// about the time of the first picture of a new source
const SEEK: f64 = 1.0;
// rife makes frames in bursts of 4, so a measure skips and takes at least that many
const WARM: u32 = 4;
// svp renders narrower pictures on the cpu, which is slower
const SMALLEST: u32 = 480;
// 3 s at 60 fps
const FILL: usize = 180;
// the seconds before the end of a stretch when the clips of the next one open
const EARLY: f64 = 2.0;
pub const THUMBS: usize = 8;
// the software renderer skips columns past 4096
const WAVE: (usize, usize) = (120, 4096);
// the rows of a wave, which the window stretches to the height of the track
const STRIP: u32 = 64;

#[derive(Clone)]
pub struct Layer {
    pub clip: PathBuf,
    pub config: Config,
    pub still: bool,
    pub place: Place,
    // the time in the source that shows first
    pub start: f64,
    pub inside: f64,
    pub length: f64,
    pub speed: f64,
    pub fade: Fade,
    pub fills: bool,
    pub missing: Option<(u32, u32)>,
    pub factor: u32,
}

#[derive(Clone)]
pub struct Stretch {
    // the clips that show, the lowest one first
    pub layers: Vec<Layer>,
    // the time on the timeline where it starts to show, and the time where other clips take over
    pub time: f64,
    pub end: f64,
}

#[derive(Clone, Copy, PartialEq)]
pub enum View {
    Result,
    Both,
    Original,
}

#[derive(Clone)]
pub struct Show {
    // a playback has all stretches up to a gap, a paused picture has one
    pub parts: Vec<Stretch>,
    pub canvas: PathBuf,
    pub setup: Setup,
    pub play: bool,
    // nothing shows, the clips get ready during a gap
    pub quiet: bool,
    pub view: View,
    pub width: u32,
    pub size: (u32, u32),
    pub span: f64,
    // a frame to save at full size, the original or the result
    pub save: Option<bool>,
    pub auto: bool,
}

pub struct Measure {
    pub jobs: Vec<Job>,
    pub sounds: Option<Vec<Sound>>,
    pub setup: Setup,
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
    let mut plain = Config {
        shrink: config.shrink,
        ..Config::default()
    };
    plain.dedup.enabled = false;
    plain.pre.enabled = false;
    plain.interpolation.enabled = false;
    plain.blending.enabled = false;
    plain.color.enabled = false;
    plain
}

// a decoded frame does not depend on the encode settings, so they stay out of the key
pub fn shown(mut config: Config) -> Config {
    let plain = Config::default();
    (config.output, config.upscale) = (plain.output, plain.upscale);
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
    let half = STRIP as usize / 2;
    for (column, peak) in peaks.iter().enumerate() {
        let reach = (peak.clamp(0.0, 1.0).sqrt() * half as f32).max(1.0);
        for row in 0..half {
            let alpha = ((reach - row as f32).clamp(0.0, 1.0) * 255.0) as u8;
            for row in [half + row, half - 1 - row] {
                let at = (row * peaks.len() + column) * 4;
                bytes[at..at + 4].copy_from_slice(&[255, 255, 255, alpha]);
            }
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

fn result(layer: &Layer) -> Config {
    Config {
        shrink: layer.config.shrink * layer.factor,
        ..layer.config.clone()
    }
}

fn used(layer: &Layer, original: bool) -> Config {
    match original {
        true => plain(layer),
        false => result(layer),
    }
}

fn sized(layers: &[Layer], factor: u32) -> Vec<Layer> {
    let mut layers = layers.to_vec();
    for layer in layers.iter_mut().filter(|layer| !layer.still) {
        layer.factor = factor;
    }
    layers
}

fn wanted(layers: &[Layer], view: View) -> Vec<(&Layer, Config)> {
    let mut wanted = Vec::new();
    for layer in layers {
        if view != View::Original || layer.still {
            wanted.push((layer, result(layer)));
        }
        if view != View::Result && !layer.still {
            wanted.push((layer, plain(layer)));
        }
    }
    wanted
}

fn idle(layers: &[Layer], view: View, size: (u32, u32)) -> Vec<(&Layer, Config)> {
    let each = u64::from(size.0.max(1)) * u64::from(size.1.max(1)) * 48;
    let room = budget::bytes(Class::Interactive) as u64 / 2 / each;
    let results = layers.iter().filter(|layer| !layer.still);
    let results = results.map(|layer| (layer, layer.config.clone()));
    results
        .filter(|_| view == View::Original)
        .take(room as usize)
        .collect()
}

fn start(
    layer: &Layer,
    config: &Config,
    warn: Sink,
    cancel: &Cancel,
) -> interpolini_core::Result<Source> {
    match layer.missing {
        Some(size) => {
            let art = missing();
            Source::flat(art.rgba(), art.size(), size)
        }
        None => Source::open(&layer.clip, config, layer.still, warn, cancel),
    }
}

fn key((layer, config): &(&Layer, Config)) -> Key {
    (layer.clip.clone(), config.clone())
}

fn fill(
    open: &mut Sources,
    layers: &[Layer],
    view: View,
    warn: Sink,
    cancel: &Cancel,
) -> interpolini_core::Result<()> {
    for (layer, config) in wanted(layers, view) {
        if open.get(&layer.clip, &config).is_none() {
            let source = start(layer, &config, warn, cancel)?;
            open.add(layer.clip.clone(), config, source);
        }
    }
    Ok(())
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
        let config = used(layer, original);
        let source = open.get_mut(&layer.clip, &config);
        if let Some(Ok(picture)) =
            source.map(|source| source.at(layer.start + moved * layer.speed, cancel))
        {
            pictures.push((picture, layer.place, level(layer)));
        }
    }
    canvas.preview(pictures, width, !original)
}

fn pair(
    open: &mut Sources,
    canvas: &mut Canvas,
    layers: &[Layer],
    view: View,
    moved: f64,
    width: u32,
    cancel: &Cancel,
) -> interpolini_core::Result<(Pixels, Option<Pixels>)> {
    let original = view == View::Original;
    let image = pixels(&compose(
        open, canvas, layers, original, moved, width, cancel,
    )?);
    let before = match view {
        View::Result => None,
        View::Both => compose(open, canvas, layers, true, moved, width, cancel)
            .ok()
            .map(|before| pixels(&before)),
        View::Original => Some(image.clone()),
    };
    Ok((image, before))
}

// the video on top gives the details and the speed
fn details(open: &Sources, layers: &[Layer], original: bool, setup: &Setup) -> (String, f64) {
    let mut graphs = layers.iter().rev().filter_map(|layer| {
        let source = open.get(&layer.clip, &used(layer, original))?;
        source.graph().map(|graph| (graph, layer.speed))
    });
    let Some((graph, speed)) = graphs.next() else {
        return (String::new(), 30.0);
    };
    let info = graph.info;
    let munch = setup.munch.filter(|_| !original);
    let mut fps = setup
        .fps
        .unwrap_or(info.fps.0 as f64 / info.fps.1 as f64 * speed);
    let mut size = setup.size.unwrap_or((info.width, info.height));
    if let Some(munch) = munch {
        fps = munch.fps.map_or(fps, f64::from);
        size = munch.size(size);
    }
    let source = graph.source_fps() * speed;
    let rate = match source.round() == fps.round() {
        true => format!("{fps:.0} fps"),
        false => format!("{source:.0} to {fps:.0} fps"),
    };
    let (width, height) = size;
    let details = format!("{width}x{height}  |  {rate}");
    (details, fps)
}

pub fn say(ui: &App, text: String, kind: Note, progress: f32) {
    ui.set_preview_note(text.into());
    ui.set_note_kind(kind);
    ui.set_note_progress(progress);
    ui.set_note_action("".into());
}

pub fn announce(ui: &App, text: String, action: &str) {
    let kind = if text.is_empty() {
        Note::Sticky
    } else {
        Note::Notice
    };
    say(ui, text, kind, -1.0);
    ui.set_note_action(action.into());
}

fn note(ui: &Weak<App>, text: String, kind: Note, progress: f32) {
    let _ = ui.upgrade_in_event_loop(move |ui| say(&ui, text, kind, progress));
}

fn warn(ui: &Weak<App>) -> impl Fn(Event) + Sync + '_ {
    move |event| match event {
        Event::Warning(text) => note(ui, text, Note::Warning, -1.0),
        Event::Preparing(done) => {
            let text = "Preparing TensorRT for first use".into();
            note(ui, text, Note::Sticky, done);
        }
        _ => {}
    }
}

fn fail(ui: &Weak<App>, error: interpolini_core::Error) {
    let _ = ui.upgrade_in_event_loop(move |ui| {
        LAST.take();
        ui.set_failed(true);
        ui.set_status(error.to_string().into());
        ui.set_playing(false);
        say(&ui, String::new(), Note::Sticky, -1.0);
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
        let rate = details.split_once("  |  ").map_or("", |(_, rate)| rate);
        ui.set_rate(rate.into());
        ui.set_details(details.as_str().into());
        ui.set_tick((1.0 / fps) as f32);
        ui.set_speed(text.into());
        if ui.get_note_kind() == Note::Sticky {
            say(&ui, String::new(), Note::Sticky, -1.0);
        }
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
    canvas: Option<(PathBuf, Setup, Canvas)>,
    recent: Vec<Key>,
    smaller: Vec<(Key, u32)>,
}

struct Play {
    ui: Weak<App>,
    parts: Vec<Stretch>,
    view: View,
    span: f64,
    width: u32,
    ahead: (usize, usize),
    held: Vec<Key>,
    setup: Setup,
    auto: bool,
    factor: u32,
}

// the most pixels of a picture, with the copies of it, that the memory of the preview holds
fn widest(size: (u32, u32)) -> u32 {
    let pixels = budget::bytes(Class::Interactive) / 48;
    let (wide, high) = (f64::from(size.0.max(1)), f64::from(size.1.max(1)));
    ((pixels as f64 * wide / high).sqrt() as u32).max(WIDTH)
}

fn ahead(size: (u32, u32), view: View) -> (usize, usize) {
    let (wide, high) = (u64::from(size.0.max(1)), u64::from(size.1.max(1)));
    let frame = u64::from(WIDTH) * (u64::from(WIDTH) * high / wide + 1) * 4;
    let copies = if view == View::Both { 2 } else { 1 };
    let fits = (budget::bytes(Class::Interactive) as u64 / 4 / (frame * copies)) as usize;
    let capacity = fits.clamp(2, AHEAD);
    (capacity, fits.clamp(capacity, FILL))
}

impl Worker {
    pub fn new(ui: Weak<App>) -> Self {
        Self {
            ui,
            open: Sources::default(),
            canvas: None,
            recent: Vec::new(),
            smaller: Vec::new(),
        }
    }

    fn show(&mut self, show: Show, cancel: &Cancel) -> Option<Play> {
        let Worker {
            ui,
            open,
            canvas,
            recent,
            smaller,
        } = self;
        let Show {
            parts,
            canvas: base,
            setup,
            play,
            quiet,
            view,
            width,
            size,
            span,
            save,
            auto,
        } = show;
        let Some(first) = parts.first() else {
            // a render gets all of the gpu
            *open = Sources::default();
            recent.clear();
            note(ui, String::new(), Note::Sticky, -1.0);
            return None;
        };
        if !quiet {
            let _ = ui.upgrade_in_event_loop(|ui| {
                if ui.get_note_kind() != Note::Notice {
                    say(&ui, "Rendering…".into(), Note::Sticky, -1.0);
                }
            });
        }
        let view = match (view, save) {
            (View::Result, Some(true)) => View::Both,
            (view, _) => view,
        };
        let auto = auto && view != View::Original;
        let factors = first.layers.iter().filter(|_| auto).filter_map(|layer| {
            let base = (layer.clip.clone(), layer.config.clone());
            smaller
                .iter()
                .find(|(key, _)| *key == base)
                .map(|found| found.1)
        });
        let factor = factors.max().unwrap_or(1);
        let scaled = parts
            .iter()
            .take(2)
            .map(|stretch| sized(&stretch.layers, factor));
        let scaled: Vec<Vec<Layer>> = scaled.collect();
        let layers = match play {
            true => &scaled[0],
            false => &first.layers,
        };
        let shown = wanted(layers, view);
        let warn = warn(ui);
        // a remask must not stop halfway
        let whole = Cancel::default();
        let changed = std::mem::take(&mut *locked(&CHANGED));
        for name in changed {
            let using = |config: &Config| masked(config) && config.mask.image == name;
            let Ok(gray) = mask::load(&name) else {
                drop(open.split(|_, config| !using(config)));
                continue;
            };
            for (clip, config) in open.keys().into_iter().filter(|key| using(&key.1)) {
                open.remask(&clip, &config, &gray, &warn, &whole);
            }
        }
        for (layer, config) in &shown {
            if masked(config)
                && open
                    .keys()
                    .into_iter()
                    .any(|key| key.0 == layer.clip && masked(&key.1))
                && open.get(&layer.clip, config).is_none()
                && let Ok(gray) = mask::load(&config.mask.image)
            {
                open.remask(&layer.clip, config, &gray, &warn, &whole);
            }
        }
        // the old graphs must free the gpu first
        let near = parts.iter().take(2).map(|stretch| &stretch.layers);
        let near = near.chain(&scaled).flat_map(|layers| wanted(layers, view));
        let mut near: Vec<Key> = near.map(|wanted| key(&wanted)).collect();
        near.extend(idle(&first.layers, view, size).iter().map(key));
        near.append(recent);
        drop(open.split(|clip, config| near.iter().any(|key| key.0 == clip && key.1 == *config)));
        *recent = shown.iter().map(key).collect();
        let mut failed = fill(open, layers, view, &warn, cancel).err();
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
        if !matches!(canvas, Some(canvas) if canvas.0 == base && canvas.1 == setup) {
            *canvas = match Canvas::open(&base, &setup) {
                Ok(canvas) => Some((base, setup, canvas)),
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
        let (_, _, canvas) = canvas.as_mut()?;
        let width = width.min(widest(size));
        let layers = &first.layers;
        if let Some(original) = save {
            let image = compose(open, canvas, layers, original, 0.0, u32::MAX, cancel);
            let path = image.ok().and_then(|image| write(layers, &image));
            let _ = ui.upgrade_in_event_loop(move |ui| match path {
                Some(path) => {
                    say(&ui, "Saved".into(), Note::Brief, -1.0);
                    ui.set_shot(path.to_string_lossy().as_ref().into());
                    ui.set_flash(true);
                }
                None => say(&ui, "Couldn't save the frame".into(), Note::Warning, -1.0),
            });
            return None;
        }
        if play {
            return Some(Play {
                ui: ui.clone(),
                view,
                span,
                width: width.min(WIDTH),
                ahead: ahead(size, view),
                held: open.keys(),
                setup,
                parts,
                auto,
                factor,
            });
        }
        let (image, before) = pair(open, canvas, layers, view, 0.0, width, cancel).ok()?;
        let (details, fps) = details(open, layers, view == View::Original, &setup);
        let frame = Frame {
            moved: 0.0,
            image,
            before,
            details,
            fps,
            step: 1.0 / fps,
        };
        post(ui, cancel, frame, String::new(), None, false);
        for (layer, config) in idle(layers, view, size) {
            open.open(&layer.clip, &config, false, &|_| {}, cancel)
                .ok()?;
        }
        let _ = compose(open, canvas, layers, false, 0.0, width, cancel);
        None
    }
}

fn run(worker: &Arc<Mutex<Worker>>, show: Show, cancel: &Cancel) {
    let play = locked(worker).show(show, cancel);
    if let Some(play) = play {
        playback(worker, play, cancel);
    }
}

thread_local! {
    static LAST: RefCell<Option<Show>> = const { RefCell::new(None) };
}

static CHANGED: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn masked(config: &Config) -> bool {
    config.mask.enabled && config.mask.method == Masking::Image
}

pub fn remask(names: Vec<String>) {
    locked(&CHANGED).extend(names);
    LAST.take();
}

fn same(a: &Show, b: &Show) -> bool {
    let original = a.view == View::Original;
    let layer = |a: &Layer, b: &Layer| {
        a.clip == b.clip
            && (a.config == b.config || original && plain(a) == plain(b))
            && a.still == b.still
            && a.place == b.place
            && a.fade == b.fade
            && a.fills == b.fills
            && a.missing == b.missing
            && a.length == b.length
            && a.speed == b.speed
            && (a.start - a.inside * a.speed - (b.start - b.inside * b.speed)).abs() < 1e-6
    };
    let stretch = |a: &Stretch, b: &Stretch| {
        a.end == b.end
            && a.layers.len() == b.layers.len()
            && a.layers.iter().zip(&b.layers).all(|(a, b)| layer(a, b))
    };
    a.canvas == b.canvas
        && a.setup == b.setup
        && a.view == b.view
        && a.width == b.width
        && a.size == b.size
        && a.span == b.span
        && a.quiet == b.quiet
        && a.save == b.save
        && a.auto == b.auto
        && a.parts.len() == b.parts.len()
        && a.parts.iter().zip(&b.parts).all(|(a, b)| stretch(a, b))
}

pub fn show(show: Show) {
    let kept = LAST.with_borrow_mut(|last| {
        let kept = show.play && last.as_ref().is_some_and(|last| same(last, &show));
        if !kept {
            *last = show.play.then(|| show.clone());
        }
        kept
    });
    if kept {
        return;
    }
    services::with(|services| {
        let worker = Arc::clone(&services.worker);
        services
            .preview
            .submit(move |cancel| run(&worker, show, cancel));
    });
}

pub fn measure(
    Measure {
        jobs,
        sounds,
        setup,
        done,
    }: Measure,
) {
    services::with(|services| {
        services.measure.submit(move |cancel| {
            let mut open = Sources::default();
            let bytes = estimate(&jobs, sounds.as_deref(), &setup, cancel, &mut open).ok();
            if !cancel.stopped() {
                done(bytes);
            }
        });
    });
}

struct Reach {
    current: AtomicUsize,
    made: AtomicU64,
    wake: (Mutex<()>, Condvar),
    factor: AtomicU32,
    floor: AtomicBool,
    switching: AtomicBool,
    opening: AtomicBool,
    // the time on the timeline at begin, not a number while the clock stands
    clock: AtomicU64,
    begin: Instant,
}

impl Reach {
    fn notify(&self) {
        drop(locked(&self.wake.0));
        self.wake.1.notify_all();
    }

    fn clock(&self) -> f64 {
        f64::from_bits(self.clock.load(Ordering::Relaxed)) + self.begin.elapsed().as_secs_f64()
    }
}

type Opened = interpolini_core::Result<Vec<(Key, Source)>>;

// the next clips open while this stretch plays, so the picture does not stop
fn open_next(
    ui: &Weak<App>,
    parts: &[Stretch],
    (view, held): (View, &[Key]),
    reach: &Reach,
    stop: &Cancel,
    opened: &Sender<Opened>,
) {
    let warn = warn(ui);
    let mut there = held.to_vec();
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
        let layers = sized(&stretch.layers, reach.factor.load(Ordering::Relaxed));
        let mut sources = Vec::new();
        reach.opening.store(true, Ordering::Relaxed);
        for wanted in wanted(&layers, view) {
            let key = key(&wanted);
            if there.contains(&key) {
                continue;
            }
            if stop.stopped() {
                return;
            }
            // the first picture of a clip takes long, so it is made here
            let layer = wanted.0;
            let source = start(layer, &key.1, &warn, stop)
                .and_then(|mut source| source.at(layer.start, stop).map(|_| source));
            match source {
                Ok(source) => sources.push((key.clone(), source)),
                Err(error) => {
                    let _ = opened.send(Err(error));
                    return;
                }
            }
            there.push(key);
        }
        there = wanted(&layers, view).iter().map(key).collect();
        reach.opening.store(false, Ordering::Relaxed);
        if opened.send(Ok(sources)).is_err() {
            return;
        }
    }
}

// in order: the host is slow after a picture that is left out
fn compose_all(
    worker: &Mutex<Worker>,
    play: (&Weak<App>, &[Stretch], View, u32, Setup, bool),
    reach: &Reach,
    stop: &Cancel,
    ready: &Receiver<Opened>,
    pictures: &work::Sender<Frame>,
) {
    let (ui, parts, view, width, setup, auto) = play;
    let mut worker = locked(worker);
    let Worker {
        open,
        canvas,
        smaller,
        ..
    } = &mut *worker;
    let Some((_, _, canvas)) = canvas else {
        return;
    };
    let warn = warn(ui);
    let failed = |error| {
        if !stop.stopped() {
            fail(ui, error);
        }
    };
    let mut factor = reach.factor.load(Ordering::Relaxed);
    'play: for (index, stretch) in parts.iter().enumerate() {
        let mut layers = sized(&stretch.layers, factor);
        if index > 0 {
            let Ok(opened) = ready.recv() else {
                break 'play;
            };
            // the size can go down after the opener made its sources
            let filled = opened.and_then(|sources| {
                for ((clip, config), source) in sources {
                    match open.get(&clip, &config) {
                        Some(_) => services::close(source),
                        None => open.add(clip, config, source),
                    }
                }
                fill(open, &layers, view, &warn, stop)
            });
            if let Err(error) = filled {
                failed(error);
                break 'play;
            }
            let needs = wanted(&layers, view);
            let needed = |clip: &Path, config: &Config| {
                let base = |layer: &Layer| layer.clip == clip && layer.config == *config;
                stretch.layers.iter().any(base)
                    || needs
                        .iter()
                        .any(|(layer, wanted)| layer.clip == clip && wanted == config)
            };
            services::close(open.split(needed));
            reach.current.store(index, Ordering::Relaxed);
            reach.notify();
        }
        let original = view == View::Original;
        let (mut text, fps) = details(open, &layers, original, &setup);
        let step = 1.0 / fps.min(SHOWN_FPS);
        let last = (((stretch.end - stretch.time) / step).ceil() as u32).saturating_sub(1);
        let (mut count, mut warm, mut spent, mut done, mut debt) = (0, WARM, 0.0, 0, 0.0);
        loop {
            let moved = f64::from(count) * step;
            let begun = Instant::now();
            let Ok((image, before)) = pair(open, canvas, &layers, view, moved, width, stop) else {
                break 'play;
            };
            // the opener slows the composer down for a moment
            match warm {
                _ if reach.opening.load(Ordering::Relaxed) => {}
                0 => {
                    let took = begun.elapsed().as_secs_f64();
                    (spent, done, debt) = (spent + took, done + 1, (debt + took - step).max(0.0));
                }
                _ => warm -= 1,
            }
            let made = stretch.time + moved;
            reach.made.store(made.to_bits(), Ordering::Relaxed);
            reach.notify();
            let sent = pictures.send(Frame {
                moved: made - parts[0].time,
                image,
                before,
                details: text.clone(),
                fps,
                step,
            });
            if sent.is_err() {
                break 'play;
            }
            reach.switching.store(false, Ordering::Relaxed);
            if made + step >= stretch.end {
                break;
            }
            count += 1;
            if !auto || done < WARM || debt < DEBT || made - reach.clock() > RESERVE {
                continue;
            }
            // the area goes down by the square of the step, with some room
            let short = 1.15 * spent / (f64::from(done) * step);
            let by = [2, 4, 8].into_iter().find(|by| f64::from(by * by) >= short);
            let mut by = by.unwrap_or(8);
            let widths = layers
                .iter()
                .filter(|layer| !layer.still)
                .filter_map(|layer| {
                    Some(open.get(&layer.clip, &result(layer))?.graph()?.info.width)
                });
            let narrowest = widths.min().unwrap_or(0);
            while by > 1 && narrowest / by < SMALLEST {
                by /= 2;
            }
            if by == 1 {
                // a stop into the wait needs a second of measure
                if spent >= 1.0 {
                    reach.floor.store(true, Ordering::Relaxed);
                }
                continue;
            }
            reach.switching.store(true, Ordering::Relaxed);
            let gone: Vec<Key> = layers
                .iter()
                .filter(|layer| layer.factor > 1)
                .map(|layer| (layer.clip.clone(), result(layer)))
                .collect();
            factor *= by;
            reach.factor.store(factor, Ordering::Relaxed);
            layers = sized(&stretch.layers, factor);
            if let Err(error) = fill(open, &layers, view, &warn, stop) {
                failed(error);
                break 'play;
            }
            let kept = |clip: &Path, config: &Config| {
                !gone.iter().any(|key| key.0 == clip && key.1 == *config)
            };
            services::close(open.split(kept));
            for layer in stretch.layers.iter().filter(|layer| !layer.still) {
                let base = (layer.clip.clone(), layer.config.clone());
                smaller.retain(|(key, _)| *key != base);
                smaller.push((base, factor));
            }
            // the new sources seek, so the frames until they are ready are left out
            let ready = ((reach.clock() + SEEK - stretch.time) / step).ceil() as u32;
            count = count.max(ready).min(last);
            text = details(open, &layers, original, &setup).0;
            (warm, spent, done, debt) = (WARM, 0.0, 0, 0.0);
        }
    }
}

fn show_frames(
    ui: &Weak<App>,
    queue: &work::Receiver<Frame>,
    cancel: &Cancel,
    (time, end, span): (f64, f64, f64),
    (ahead, fill): (usize, usize),
    (reach, auto): (&Reach, bool),
) -> bool {
    let mut clock: Option<(Instant, f64)> = None;
    let mut begun = None;
    let mut held = VecDeque::new();
    let mut room = ahead;
    let mut until = Instant::now();
    let mut shown = VecDeque::new();
    loop {
        let (mut frame, (at, from), first) = match clock {
            None => {
                while held.len() < room {
                    let step = match queue.recv(cancel) {
                        Ok(Some(frame)) => {
                            let step = frame.step;
                            held.push_back(frame);
                            step
                        }
                        Ok(None) => break,
                        Err(_) => return false,
                    };
                    let begun: &Instant = begun.get_or_insert_with(Instant::now);
                    let rate = (held.len() - 1) as f64 / begun.elapsed().as_secs_f64();
                    let left = end - time - held[0].moved;
                    let short = (1.0 / step - rate).max(0.0) * left;
                    if held.len() >= ahead + short as usize {
                        break;
                    }
                    if room > ahead {
                        let progress = held.len() as f32 / room as f32;
                        note(ui, "Buffering…".into(), Note::Sticky, progress);
                    }
                }
                let Some(frame) = held.pop_front() else {
                    return !cancel.stopped();
                };
                let anchor = (Instant::now() + LEAD, frame.moved);
                let origin = time + anchor.1 - (anchor.0 - reach.begin).as_secs_f64();
                reach.clock.store(origin.to_bits(), Ordering::Relaxed);
                clock = Some(anchor);
                (frame, anchor, true)
            }
            Some(anchor) => {
                while held.len() < fill {
                    let Some(frame) = queue.try_recv() else {
                        break;
                    };
                    held.push_back(frame);
                }
                // a size switch makes no frames, and a wait would not make it faster
                let switching = reach.switching.load(Ordering::Relaxed);
                let deadline = match auto && !reach.floor.load(Ordering::Relaxed) {
                    true => (!switching).then(|| shown.back().copied().unwrap_or(until) + HOLD),
                    false => Some(until + LATE),
                };
                let frame = match held.pop_front() {
                    Some(frame) => frame,
                    None => match queue.recv_until(cancel, deadline) {
                        Ok(Some(Some(frame))) => frame,
                        Ok(Some(None)) => return !cancel.stopped(),
                        Ok(None) if reach.switching.load(Ordering::Relaxed) => continue,
                        Ok(None) => {
                            (clock, begun) = (None, None);
                            reach.clock.store(f64::NAN.to_bits(), Ordering::Relaxed);
                            let cancel = cancel.clone();
                            let _ = ui.upgrade_in_event_loop(move |_| {
                                if !cancel.stopped() {
                                    services::with(|services| services.audio.stop());
                                }
                            });
                            room = fill;
                            continue;
                        }
                        Err(_) => return false,
                    },
                };
                (frame, anchor, false)
            }
        };
        let when = |frame: &Frame| at + Duration::from_secs_f64((frame.moved - from).max(0.0));
        while held
            .front()
            .is_some_and(|next| when(next) <= Instant::now())
        {
            frame = held.pop_front().unwrap_or(frame);
        }
        let due = when(&frame);
        if !cancel.pause(due) {
            return false;
        }
        until = due + Duration::from_secs_f64(frame.step);
        let now = Instant::now();
        shown.push_back(now);
        while shown
            .front()
            .is_some_and(|at| now - *at > Duration::from_secs(1))
        {
            shown.pop_front();
        }
        let position = ((time + frame.moved) / span) as f32;
        let rate = 1.0 / frame.step;
        let speed = (shown.len() - 1) as f64 / (now - shown[0]).as_secs_f64().max(0.001);
        let text = match shown.len() > 1 {
            true => format!("{:.0} of {rate:.0} fps", speed.min(rate)),
            false => String::new(),
        };
        post(ui, cancel, frame, text, Some(position), first);
    }
}

fn playback(worker: &Arc<Mutex<Worker>>, play: Play, cancel: &Cancel) {
    let Play {
        ui,
        parts,
        view,
        span,
        width,
        ahead,
        held,
        setup,
        auto,
        factor,
    } = play;
    let time = parts[0].time;
    let parts = Arc::new(parts);
    let (pictures, queue) = work::channel(ahead.0);
    let (opened, ready) = channel();
    let reach = Arc::new(Reach {
        current: AtomicUsize::new(0),
        made: AtomicU64::new(0),
        wake: Default::default(),
        factor: AtomicU32::new(factor),
        floor: AtomicBool::new(false),
        switching: AtomicBool::new(false),
        opening: AtomicBool::new(false),
        clock: AtomicU64::new(f64::NAN.to_bits()),
        begin: Instant::now(),
    });
    let stop = Cancel::default();
    budget::preview(true);
    let opener = work::spawn("opener", {
        let (ui, parts, reach, stop) = (
            ui.clone(),
            Arc::clone(&parts),
            Arc::clone(&reach),
            stop.clone(),
        );
        move || open_next(&ui, &parts, (view, &held), &reach, &stop, &opened)
    });
    let composer = work::spawn("composer", {
        let (ui, worker, parts) = (ui.clone(), Arc::clone(worker), Arc::clone(&parts));
        let (reach, stop) = (Arc::clone(&reach), stop.clone());
        move || {
            let play = (&ui, &parts[..], view, width, setup, auto);
            compose_all(&worker, play, &reach, &stop, &ready, &pictures)
        }
    });
    let end = parts[parts.len() - 1].end;
    let finished = show_frames(
        &ui,
        &queue,
        cancel,
        (time, end, span),
        ahead,
        (&reach, auto),
    );
    stop.stop();
    drop(queue);
    reach.notify();
    let _ = opener.join();
    let _ = composer.join();
    budget::preview(false);
    if finished {
        let _ = ui.upgrade_in_event_loop(|ui| {
            LAST.take();
            ui.set_speed("".into());
            say(&ui, String::new(), Note::Sticky, -1.0);
            ui.invoke_ended();
        });
    }
}

// places gives the rows of the clips of one file, with the audio track of each
pub fn thumbs(
    ui: Weak<App>,
    key: &str,
    clip: PathBuf,
    places: impl Fn() -> Vec<(Option<usize>, usize)> + Send + Clone + 'static,
) {
    services::background(key, move |cancel| {
        let _ = interpolini_core::thumbs(&clip, THUMBS, 160, cancel, |thumb, image| {
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
        });
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
    services::with(|services| {
        services.differences.submit(key, move |cancel| {
            if let Ok(differences) = interpolini_core::differences(&clip, part.0, part.1, cancel) {
                let _ = ui.upgrade_in_event_loop(move |ui| done(&ui, differences));
            }
        })
    });
}

type Peaks = BTreeMap<(PathBuf, Option<SystemTime>, u64), Vec<Vec<f32>>>;

pub fn waves(
    ui: Weak<App>,
    key: &str,
    clip: PathBuf,
    seconds: f64,
    places: impl Fn() -> Vec<(Option<usize>, usize)> + Send + 'static,
) {
    static PEAKS: Mutex<Peaks> = Mutex::new(BTreeMap::new());
    services::background(key, move |cancel| {
        let Ok(meta) = std::fs::metadata(&clip) else {
            return;
        };
        let file = (clip.clone(), meta.modified().ok(), meta.len());
        let cached = locked(&PEAKS).get(&file).cloned();
        let tracks = match cached {
            Some(tracks) => tracks,
            None => {
                let count = ((seconds * 100.0) as usize).clamp(WAVE.0, WAVE.1);
                let Ok(tracks) = peaks(&clip, seconds, count, cancel) else {
                    return;
                };
                locked(&PEAKS).insert(file, tracks.clone());
                tracks
            }
        };
        let tracks: Vec<Vec<Pixels>> = tracks
            .into_iter()
            .map(|mut peaks| {
                let mut levels = vec![strip(&peaks)];
                while peaks.len() > 1 {
                    peaks = peaks
                        .chunks(2)
                        .map(|pair| pair.iter().copied().fold(0.0, f32::max))
                        .collect();
                    levels.push(strip(&peaks));
                }
                levels
            })
            .collect();
        let _ = ui.upgrade_in_event_loop(move |ui| {
            let tracks: Vec<ModelRc<slint::Image>> = tracks
                .into_iter()
                .map(|levels| {
                    let images = levels.into_iter().map(slint::Image::from_rgba8);
                    ModelRc::new(VecModel::from_iter(images))
                })
                .collect();
            let rows = ui.get_clips();
            for (audio, index) in places() {
                let waves = audio.and_then(|audio| tracks.get(audio));
                if let (Some(waves), Some(mut row)) = (waves, rows.row_data(index)) {
                    row.waves = waves.clone();
                    rows.set_row_data(index, row);
                }
            }
        });
    });
}
