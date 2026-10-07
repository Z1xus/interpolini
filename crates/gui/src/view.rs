use std::sync::Arc;
use std::time::Duration;

use interpolini_core::config::{self, Config};
use interpolini_core::{Place, Plan, lossless};
use slint::{ComponentHandle, Model, ModelRc, VecModel};

use crate::settings::settings;
use crate::state::{Shared, State, lock};
use crate::{App, Clip, Lane, preview};

// the rows hold the place and the cut of each clip, and this brings the rest in line
pub fn layout(ui: &App, state: &mut State) {
    let rows = ui.get_clips();
    for (index, entry) in state.entries.iter_mut().enumerate() {
        let Some(mut row) = rows.row_data(index) else {
            continue;
        };
        entry.cut = (row.cut_in, row.cut_out);
        entry.at = f64::from(row.at);
        entry.track = row.track as usize;
        entry.place = Place {
            x: row.x,
            y: row.y,
            scale: [row.zoom_x, row.zoom_y],
            crop: [row.crop_left, row.crop_top, row.crop_right, row.crop_bottom],
        };
        let size = (entry.kept() as f32, entry.seconds as f32);
        if (row.kept, row.length) != size {
            (row.kept, row.length) = size;
            rows.set_row_data(index, row);
        }
    }
    // the playhead stays at its time when the timeline gets longer or shorter
    let time = ui.get_position() * ui.get_seconds();
    let seconds = state.seconds() as f32;
    if seconds != ui.get_seconds() {
        ui.set_seconds(seconds);
        ui.set_position((time / seconds.max(0.001)).min(1.0));
    }
}

pub fn lanes(ui: &App, state: &State) {
    let (video, audio, content) = state.places();
    let lane = |name: String, y: f32, height: f32, muted: bool| Lane {
        name: name.into(),
        y,
        height,
        muted,
    };
    // the rows change in place: a new list would end a drag that started in a row
    let fill = |rows: ModelRc<Lane>, lanes: Vec<Lane>| match rows.row_count() == lanes.len() {
        true => {
            for (index, lane) in lanes.into_iter().enumerate() {
                rows.set_row_data(index, lane);
            }
            rows
        }
        false => ModelRc::new(VecModel::from(lanes)),
    };
    let heights = state.tracks.video.iter().zip(video).enumerate();
    let rows =
        heights.map(|(index, (height, y))| lane(format!("V{}", index + 1), y, *height, false));
    ui.set_video(fill(ui.get_video(), rows.collect()));
    let heights = state.tracks.audio.iter().zip(audio).enumerate();
    let rows = heights
        .map(|(index, ((height, muted), y))| lane(format!("A{}", index + 1), y, *height, *muted));
    ui.set_audio(fill(ui.get_audio(), rows.collect()));
    ui.set_content(content);
    let videos = state.videos().into_iter();
    ui.set_videos(videos.filter(|index| !state.entries[*index].still).count() as i32);
}

pub fn fit(ui: &App, shared: &Shared) {
    let mut state = lock(shared);
    layout(ui, &mut state);
    ui.set_extent(state.seconds().max(1.0) as f32);
    lanes(ui, &state);
    drop(state);
    crate::clips::measure(ui, shared);
}

pub fn selected(ui: &App) -> Option<usize> {
    usize::try_from(ui.get_selected()).ok()
}

pub fn update(ui: &App, index: usize, change: impl FnOnce(&mut Clip)) {
    let rows = ui.get_clips();
    if let Some(mut row) = rows.row_data(index) {
        change(&mut row);
        rows.set_row_data(index, row);
    }
}

pub fn clock(seconds: f64) -> String {
    let whole = seconds as u32;
    let rest = (seconds.fract() * 1000.0) as u32;
    format!("{}:{:02}.{rest:03}", whole / 60, whole % 60)
}

pub fn label(entry: &config::Entry) -> String {
    match entry.local {
        true => format!("{} (folder)", entry.name),
        false => entry.name.clone(),
    }
}

pub fn curve(ui: &App, config: &Config, fps: f64) {
    let plan = Plan::new(fps, config);
    let peak = plan.weights.iter().copied().fold(0.0, f64::max);
    let weights: Vec<f32> = plan
        .weights
        .iter()
        .map(|weight| (weight / peak) as f32)
        .collect();
    ui.set_summary(match plan.blended {
        true => format!("{} frames blended", weights.len()).into(),
        false => "".into(),
    });
    ui.set_weights(ModelRc::new(VecModel::from(weights)));
    ui.set_lossless(fps > 0.0 && lossless(fps, config));
}

pub fn timecode(ui: &App, state: &Shared) {
    let total = lock(state).seconds();
    let now = f64::from(ui.get_position()) * total;
    ui.set_timecode(format!("{} / {}", clock(now), clock(total)).into());
}

// the model list follows the backend, and a model that the backend does not have gives way
pub fn models(ui: &App, rife: &mut config::Rife) {
    let models = config::models(rife.backend);
    if let Some(first) = models.first().filter(|_| !models.contains(&rife.model)) {
        rife.model = first.clone();
    }
    let models: Vec<slint::SharedString> = models.into_iter().map(Into::into).collect();
    ui.set_models(ModelRc::new(VecModel::from(models)));
}

pub fn unsaved(entry: &crate::state::Entry) -> bool {
    let saved = match &entry.origin {
        Some(origin) => config::read(origin.clone()).config,
        None => Config::default(),
    };
    saved != entry.config
}

pub fn load(ui: &App, state: &mut State) {
    let link = selected(ui)
        .and_then(|index| state.entries.get(index))
        .map(|entry| entry.link);
    ui.set_link(link.map_or(-1, |link| link as i32));
    // the picture of the selected clip, which the transform box moves
    let picture = |entry: &crate::state::Entry| Some(entry.link) == link && entry.audio.is_none();
    let framed = state.entries.iter().position(picture);
    ui.set_framed(framed.map_or(-1, |index| index as i32));
    let entry = state.current(ui);
    models(ui, &mut entry.config.rife);
    ui.set_settings(settings(&entry.config));
    let found = config::entries(&entry.clip);
    let mut configs: Vec<slint::SharedString> =
        found.iter().map(|entry| label(entry).into()).collect();
    if configs.is_empty() {
        configs.push("built-in".into());
    }
    ui.set_configs(ModelRc::new(VecModel::from(configs)));
    let origin = entry.origin.as_ref().map(label);
    ui.set_config(origin.unwrap_or("built-in".into()).into());
    curve(ui, &entry.config, entry.fps);
    ui.set_unsaved(unsaved(entry));
    ui.set_repeats(repeats(ui, state));
}

// the repeats of the clip that the settings are for, or of the worst clip when they are for all
pub fn repeats(ui: &App, state: &State) -> f32 {
    let all = state.entries.iter().map(|entry| entry.repeats);
    match state.video(ui) {
        Some(index) => state.entries[index].repeats,
        None => all.fold(0.0, f32::max),
    }
}

pub fn refresh(ui: &App, shared: &Shared) {
    timecode(ui, shared);
    let mut state = lock(shared);
    layout(ui, &mut state);
    let entry = state.current(ui);
    curve(ui, &entry.config, entry.fps);
    // a render gets all of the gpu, so the preview closes its clips until the render is done
    if ui.get_rendering() {
        if let Some(preview) = &state.preview {
            let _ = preview.send(preview::Request {
                layers: Vec::new(),
                canvas: Default::default(),
                play: false,
                time: 0.0,
                end: 0.0,
                total: 0.0,
            });
        }
        return;
    }
    let shown = state.part(ui.get_position());
    let Some((time, part)) = shown.filter(|shown| !shown.1.clips.is_empty()) else {
        // no clip is here, so the picture is empty and playback walks on by the clock
        ui.set_preview(slint::Image::default());
        ui.set_preview_note("".into());
        let walk = ui.get_playing() && !state.entries.is_empty();
        drop(state);
        if walk {
            idle(ui, shared);
        }
        return;
    };
    // without a video the lowest image gives the picture size
    let base = &state.entries[state.base().unwrap_or(part.clips[0])];
    // a video that fills the picture hides the clips under it
    let fills = |index: &usize| {
        let entry = &state.entries[*index];
        !entry.still && entry.place.covers(base.size, entry.size)
    };
    let shown = &part.clips[part.clips.iter().rposition(fills).unwrap_or(0)..];
    let original = ui.get_view() == "original";
    let layer = |index: &usize| {
        let entry = &state.entries[*index];
        preview::Layer {
            clip: entry.clip.clone(),
            config: match original {
                true => preview::source(&entry.config),
                false => entry.config.clone(),
            },
            still: entry.still,
            place: entry.place,
            start: f64::from(entry.cut.0) * entry.seconds + time - entry.at,
        }
    };
    let request = preview::Request {
        layers: shown.iter().map(layer).collect(),
        canvas: base.clip.clone(),
        play: ui.get_playing(),
        time,
        end: part.end,
        total: state.seconds(),
    };
    if let Some(preview) = &state.preview {
        let _ = preview.send(request);
    }
}

thread_local! {
    pub static WALK: slint::Timer = slint::Timer::default();
}

pub fn idle(ui: &App, shared: &Shared) {
    const STEP: Duration = Duration::from_millis(33);
    ui.invoke_sounding();
    let (weak, shared) = (ui.as_weak(), Arc::clone(shared));
    WALK.with(|walk| {
        walk.start(slint::TimerMode::Repeated, STEP, move || {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let state = lock(&shared);
            let next =
                f64::from(ui.get_position()) + STEP.as_secs_f64() / state.seconds().max(0.001);
            let over = next >= 1.0 || !ui.get_playing();
            let clip = state.locate(next as f32).is_some();
            drop(state);
            if over || clip {
                WALK.with(slint::Timer::stop);
            }
            if over && ui.get_playing() {
                ui.set_playing(false);
                return;
            }
            ui.set_position(next.min(1.0) as f32);
            timecode(&ui, &shared);
            if clip {
                refresh(&ui, &shared);
            }
        });
    });
}

pub fn select(ui: &App, state: &Shared, index: i32) {
    // a plain selection ends a selection of more clips
    let rows = ui.get_clips();
    for place in 0..rows.row_count() {
        if let Some(mut row) = rows.row_data(place).filter(|row| row.picked) {
            row.picked = false;
            rows.set_row_data(place, row);
        }
    }
    ui.set_selected(index);
    load(ui, &mut lock(state));
}
