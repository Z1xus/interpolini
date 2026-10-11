use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{Sender, channel, sync_channel};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use interpolini_core::config::Config;
use interpolini_core::{Canvas, Event, Fade, Graph, Image, Place, Source, peaks};
use slint::{Model, ModelRc, Rgba8Pixel, SharedPixelBuffer, VecModel, Weak};

use crate::App;

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

pub struct Request {
    // a playback has all stretches up to a gap, a paused picture has one
    pub parts: Vec<Stretch>,
    pub canvas: PathBuf,
    pub play: bool,
    // nothing shows, the clips get ready during a gap
    pub quiet: bool,
    pub original: bool,
    pub width: u32,
    pub total: f64,
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

type Held = (PathBuf, Config, Source);

fn plain(layer: &Layer) -> Config {
    match layer.still {
        true => layer.config.clone(),
        false => source(&layer.config),
    }
}

// each clip is open two times, with its settings and as the source
fn wanted(layers: &[Layer]) -> Vec<(&Layer, Config)> {
    let layers = layers.iter();
    layers
        .flat_map(|layer| [(layer, layer.config.clone()), (layer, plain(layer))])
        .collect()
}

fn same(held: &Held, wanted: &(&Layer, Config)) -> bool {
    held.0 == wanted.0.clip && held.1 == wanted.1
}

fn compose(
    open: &mut [Held],
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
        let source = open
            .iter_mut()
            .find(|held| held.0 == layer.clip && held.1 == config);
        if let Some(Ok(picture)) = source.map(|held| held.2.at(layer.start + moved)) {
            pictures.push((picture, layer.place, level(layer)));
        }
    }
    canvas.preview(pictures, width)
}

pub fn spawn(ui: Weak<App>) -> (Sender<Request>, JoinHandle<()>) {
    let (sender, receiver) = channel::<Request>();
    let thread = std::thread::spawn(move || {
        let mut open: Vec<Held> = Vec::new();
        let mut canvas: Option<(PathBuf, Canvas)> = None;
        let mut waiting = None;
        while let Some(mut request) = waiting.take().or_else(|| receiver.recv().ok()) {
            while let Ok(newer) = receiver.try_recv() {
                request = newer;
            }
            let Request {
                parts,
                canvas: base,
                play,
                quiet,
                original,
                width,
                total,
            } = request;
            let note = |text: String| {
                let _ = ui.upgrade_in_event_loop(move |ui| ui.set_preview_note(text.into()));
            };
            let fail = |error: interpolini_core::Error| {
                let _ = ui.upgrade_in_event_loop(move |ui| {
                    ui.set_failed(true);
                    ui.set_status(error.to_string().into());
                    ui.set_playing(false);
                    ui.set_preview_note("".into());
                });
            };
            let Some(first) = parts.first() else {
                open.clear();
                note(String::new());
                continue;
            };
            if !quiet {
                note("Rendering…".into());
            }
            // the old graphs must free the gpu first
            let soon = |held: &Held| {
                let near = parts.iter().take(2);
                near.flat_map(|stretch| wanted(&stretch.layers))
                    .any(|wanted| same(held, &wanted))
            };
            open.retain(soon);
            let warn = |event| {
                if let Event::Warning(text) = event {
                    note(text);
                }
            };
            let mut failed = None;
            for wanted in &wanted(&first.layers) {
                if open.iter().any(|held| same(held, wanted)) {
                    continue;
                }
                let (layer, config) = wanted;
                match Source::open(&layer.clip, config, layer.still, &warn) {
                    Ok(source) => open.push((layer.clip.clone(), config.clone(), source)),
                    Err(error) => failed = Some(error),
                }
            }
            if quiet {
                for (layer, config) in &wanted(&first.layers) {
                    let held = |held: &&mut Held| held.0 == layer.clip && held.1 == *config;
                    if let Some(held) = open.iter_mut().find(held) {
                        let _ = held.2.at(layer.start);
                    }
                }
                continue;
            }
            if !matches!(&canvas, Some(canvas) if canvas.0 == base) {
                canvas = match Canvas::open(&base) {
                    Ok(canvas) => Some((base, canvas)),
                    Err(error) => {
                        failed = Some(error);
                        None
                    }
                };
            }
            let (None, Some((_, canvas))) = (failed.take(), &mut canvas) else {
                failed.map(fail);
                continue;
            };
            let time = first.time;
            let width = if play { WIDTH } else { width };
            let (pictures, queue) = sync_channel(AHEAD);
            let (opened, ready) = channel::<interpolini_core::Result<Held>>();
            let held: Vec<(PathBuf, Config)> = open
                .iter()
                .map(|held| (held.0.clone(), held.1.clone()))
                .collect();
            let (current, stop) = (AtomicUsize::new(0), AtomicBool::new(false));
            let reached = AtomicU64::new(0);
            std::thread::scope(|scope| {
                // the next clips open while this stretch plays, so the picture does not stop
                scope.spawn(|| {
                    for (index, stretch) in parts.iter().enumerate().skip(1) {
                        let from = parts[index - 1].end - EARLY;
                        let early = || {
                            let made = f64::from_bits(reached.load(Ordering::Relaxed));
                            current.load(Ordering::Relaxed) + 1 < index || made < from
                        };
                        while early() {
                            if stop.load(Ordering::Relaxed) {
                                return;
                            }
                            std::thread::sleep(Duration::from_millis(5));
                        }
                        let mut there = match index {
                            1 => held.clone(),
                            _ => wanted(&parts[index - 1].layers)
                                .into_iter()
                                .map(|(layer, config)| (layer.clip.clone(), config))
                                .collect(),
                        };
                        for (layer, config) in wanted(&stretch.layers) {
                            let key = (layer.clip.clone(), config);
                            if there.contains(&key) {
                                continue;
                            }
                            if stop.load(Ordering::Relaxed) {
                                return;
                            }
                            // the first picture of a clip takes long, so it is made here
                            let source = Source::open(&key.0, &key.1, layer.still, &warn)
                                .and_then(|mut source| source.at(layer.start).map(|_| source));
                            there.push(key.clone());
                            let sent = opened.send(source.map(|source| (key.0, key.1, source)));
                            if sent.is_err() {
                                return;
                            }
                        }
                    }
                    drop(opened);
                });
                // in order: the host is slow after a picture that is left out
                scope.spawn(|| {
                    let ready = ready;
                    'play: for (index, stretch) in parts.iter().enumerate() {
                        let needs = wanted(&stretch.layers);
                        if index > 0 {
                            let missing = |open: &[Held]| {
                                let there = |wanted| open.iter().any(|held| same(held, wanted));
                                !needs.iter().all(there)
                            };
                            while missing(&open) {
                                match ready.recv() {
                                    Ok(Ok(held)) => open.push(held),
                                    Ok(Err(error)) => {
                                        fail(error);
                                        break 'play;
                                    }
                                    Err(_) => break 'play,
                                }
                            }
                            // a graph closes slowly, so on another thread
                            let (kept, old) = std::mem::take(&mut open)
                                .into_iter()
                                .partition(|held| needs.iter().any(|wanted| same(held, wanted)));
                            open = kept;
                            std::thread::spawn(move || drop::<Vec<Held>>(old));
                            current.store(index, Ordering::Relaxed);
                        }
                        // the video on top gives the details and the speed
                        let graphs = stretch.layers.iter().rev().filter_map(|layer| {
                            let held = open
                                .iter()
                                .find(|held| held.0 == layer.clip && held.1 == layer.config)?;
                            held.2.graph()
                        });
                        let (mut details, mut fps) = (String::new(), 30.0);
                        if let Some(graph) = graphs.into_iter().next() {
                            let info = graph.info;
                            fps = info.fps.0 as f64 / info.fps.1 as f64;
                            details = format!(
                                "{}x{}  |  {:.0} to {fps:.0} fps",
                                info.width,
                                info.height,
                                graph.source_fps()
                            );
                        }
                        let step = (fps / SHOWN_FPS).round().max(1.0) / fps;
                        let layers = &stretch.layers;
                        for moved in (0..).map(|index| f64::from(index) * step) {
                            let Ok(image) = compose(&mut open, canvas, layers, false, moved, width)
                            else {
                                break 'play;
                            };
                            let before = original
                                .then(|| {
                                    compose(&mut open, canvas, layers, true, moved, width).ok()
                                })
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
                            if sent.is_err() || !play {
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
                    let Frame {
                        moved,
                        image,
                        before,
                        details,
                        fps,
                        step,
                    } = frame;
                    let due = clock.map(|(at, from)| at + Duration::from_secs_f64(moved - from));
                    // the sound starts with the first picture, and again after pictures that are too late
                    let first = due.is_none_or(|due| due + LATE < Instant::now());
                    let due = match first {
                        true => Instant::now() + if play { LEAD } else { Duration::ZERO },
                        false => due.unwrap_or_else(Instant::now),
                    };
                    if first {
                        clock = Some((due, moved));
                    }
                    std::thread::sleep(due.saturating_duration_since(Instant::now()));
                    if play && let Ok(newer) = receiver.try_recv() {
                        waiting = Some(newer);
                        break;
                    }
                    let started: Instant = *started.get_or_insert_with(Instant::now);
                    let position = play.then_some(((time + moved) / total) as f32);
                    let speed = shown as f64 / started.elapsed().as_secs_f64().max(0.001);
                    let text = match play && shown > 0 {
                        true => format!("{:.0} of {:.0} fps", speed.min(1.0 / step), 1.0 / step),
                        false => String::new(),
                    };
                    let _ = ui.upgrade_in_event_loop(move |ui| {
                        ui.set_preview(picture(image));
                        if let Some(before) = before {
                            ui.set_original(picture(before));
                        }
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
                stop.store(true, Ordering::Relaxed);
            });
            if play && waiting.is_none() {
                let _ = ui.upgrade_in_event_loop(|ui| {
                    ui.set_preview_note("".into());
                    ui.invoke_ended();
                });
            }
        }
    });
    (sender, thread)
}

// places gives the rows of the clips of one file, with the audio track of each
pub fn thumbs(
    ui: Weak<App>,
    clip: PathBuf,
    config: Config,
    places: impl Fn() -> Vec<(Option<usize>, usize)> + Send + Clone + 'static,
) {
    std::thread::spawn(move || {
        let Ok(graph) = Graph::open(&clip, &source(&config), &|_| {}) else {
            return;
        };
        for thumb in 0..THUMBS {
            let frame = (thumb * graph.info.frames as usize / THUMBS) as i32;
            let Ok(image) = graph.preview(frame, 160) else {
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

// counts the repeated frames of a clip, which takes a moment
pub fn repeats(
    ui: Weak<App>,
    clip: PathBuf,
    threshold: f32,
    part: (f64, f64),
    done: impl FnOnce(&App, f32) + Send + 'static,
) {
    std::thread::spawn(move || {
        if let Ok(repeats) = interpolini_core::repeats(&clip, threshold, part.0, part.1) {
            let _ = ui.upgrade_in_event_loop(move |ui| done(&ui, repeats));
        }
    });
}

pub fn waves(
    ui: Weak<App>,
    clip: PathBuf,
    seconds: f64,
    places: impl Fn() -> Vec<(Option<usize>, usize)> + Send + 'static,
) {
    std::thread::spawn(move || {
        let Ok(tracks) = peaks(&clip, seconds, WAVE) else {
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
