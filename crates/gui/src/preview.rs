use std::path::PathBuf;
use std::sync::mpsc::{Sender, channel};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use interpolini_core::config::Config;
use interpolini_core::{Canvas, Event, Graph, Image, Place, Source, peaks};
use slint::{Model, ModelRc, Rgba8Pixel, SharedPixelBuffer, VecModel, Weak};

use crate::App;

const WIDTH: u32 = 1280;
const SHOWN_FPS: f64 = 60.0;
pub const THUMBS: usize = 8;
const WAVE: usize = 120;

pub struct Layer {
    pub clip: PathBuf,
    pub config: Config,
    pub still: bool,
    pub place: Place,
    // the time in the source that shows first
    pub start: f64,
}

pub struct Request {
    // the clips that show, the lowest one first
    pub layers: Vec<Layer>,
    pub canvas: PathBuf,
    pub play: bool,
    // the time on the timeline, the time where other clips take over, and the length of it all
    pub time: f64,
    pub end: f64,
    pub total: f64,
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

pub fn spawn(ui: Weak<App>) -> (Sender<Request>, JoinHandle<()>) {
    let (sender, receiver) = channel::<Request>();
    let thread = std::thread::spawn(move || {
        let mut open: Vec<(PathBuf, Config, Source)> = Vec::new();
        let mut canvas: Option<(PathBuf, Canvas)> = None;
        let mut waiting = None;
        while let Some(mut request) = waiting.take().or_else(|| receiver.recv().ok()) {
            while let Ok(newer) = receiver.try_recv() {
                request = newer;
            }
            let Request {
                layers,
                canvas: base,
                play,
                time,
                end,
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
            if layers.is_empty() {
                open.clear();
                note(String::new());
                continue;
            }
            note("Rendering…".into());
            let same = |held: &(PathBuf, Config, Source), layer: &Layer| {
                held.0 == layer.clip && held.1 == layer.config
            };
            // the old graphs must free the gpu first
            open.retain(|held| layers.iter().any(|layer| same(held, layer)));
            let warn = |event| {
                if let Event::Warning(text) = event {
                    note(text);
                }
            };
            let mut failed = None;
            for layer in &layers {
                if open.iter().any(|held| same(held, layer)) {
                    continue;
                }
                match Source::open(&layer.clip, &layer.config, layer.still, &warn) {
                    Ok(source) => open.push((layer.clip.clone(), layer.config.clone(), source)),
                    Err(error) => failed = Some(error),
                }
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
            // the video on top gives the details and the speed
            let graphs = layers.iter().rev().filter_map(|layer| {
                let held = open.iter().find(|held| same(held, layer))?;
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
            let mut started = None;
            let mut shown = 0;
            let mut moved = 0.0;
            loop {
                let mut pictures = Vec::new();
                for layer in &layers {
                    let source = open.iter_mut().find(|held| same(held, layer));
                    let picture = source.map(|held| held.2.at(layer.start + moved));
                    if let Some(Ok(picture)) = picture {
                        pictures.push((picture, layer.place));
                    }
                }
                let Ok(image) = canvas.preview(pictures, WIDTH) else {
                    break;
                };
                // the sound starts with the first picture, and the clock of the pictures too
                let first = started.is_none();
                let started: Instant = *started.get_or_insert_with(Instant::now);
                let details = details.clone();
                let position = play.then_some(((time + moved) / total) as f32);
                let speed = f64::from(shown) / started.elapsed().as_secs_f64().max(0.001);
                let text = match play && shown > 0 {
                    true => format!("{:.0} of {:.0} fps", speed.min(1.0 / step), 1.0 / step),
                    false => String::new(),
                };
                let _ = ui.upgrade_in_event_loop(move |ui| {
                    ui.set_preview(picture(image));
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
                shown += 1;
                // a preview that is too slow leaves pictures out, so it stays with the sound
                let due = (started.elapsed().as_secs_f64() / step).ceil();
                moved = due.max((moved / step).round() + 1.0) * step;
                if !play || time + moved >= end {
                    break;
                }
                let wait = Duration::from_secs_f64(moved).saturating_sub(started.elapsed());
                std::thread::sleep(wait);
                if let Ok(newer) = receiver.try_recv() {
                    waiting = Some(newer);
                    break;
                }
            }
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
