use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use interpolini_core::config::{self, Config, Ease};
use interpolini_core::{Fade, Part, Place, Plan, lossless};
use slint::{ComponentHandle, Model, ModelRc, VecModel};

use crate::settings::settings;
use crate::state::{Shared, State, lock};
use crate::{App, Clip, Fact, Lane, preview};

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
        entry.gain = row.gain;
        entry.fade = (row.fade_in, row.fade_out);
        entry.cross = row.cross;
        entry.eases = [&row.ease_in, &row.ease_out, &row.ease_cross]
            .map(|name| Ease::parse(name).unwrap_or(Ease::Linear));
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
    let spans = state.spans();
    let mut counts: HashMap<u64, usize> = HashMap::new();
    for entry in &state.entries {
        *counts.entry(entry.link).or_default() += 1;
    }
    for (index, span) in spans.iter().enumerate() {
        let Some(mut row) = rows.row_data(index) else {
            continue;
        };
        let shown = (
            span.cross.0 as f32,
            span.cross.1 as f32,
            slint::SharedString::from(span.fade.curves.1.name()),
            counts[&state.entries[index].link] > 1,
        );
        let now = (
            row.cross_in,
            row.cross_out,
            row.ease_after.clone(),
            row.paired,
        );
        if now != shown {
            (row.cross_in, row.cross_out, row.ease_after, row.paired) = shown;
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

pub fn megabytes(bytes: u64) -> String {
    match bytes as f64 / 1e6 {
        size if size >= 1000.0 => format!("{:.1} GB", size / 1000.0),
        size if size >= 10.0 => format!("{size:.0} MB"),
        size => format!("{size:.1} MB"),
    }
}

pub fn label(entry: &config::Entry) -> String {
    match entry.local {
        true => format!("{} (folder)", entry.name),
        false => entry.name.clone(),
    }
}

pub fn curve(ui: &App, state: &mut State) {
    let video = |entry: &&crate::state::Entry| entry.audio.is_none() && !entry.still;
    let rates: Vec<f64> = match state.video(ui) {
        Some(index) => vec![state.entries[index].fps],
        None => state
            .entries
            .iter()
            .filter(video)
            .map(|entry| entry.fps)
            .collect(),
    };
    let config = &state.current(ui).config;
    let plan = Plan::new(rates.iter().copied().fold(0.0, f64::max), config);
    let peak = plan.weights.iter().copied().fold(0.0, f64::max);
    let weights: Vec<f32> = plan
        .weights
        .iter()
        .filter(|_| plan.blended)
        .map(|weight| (weight / peak) as f32)
        .collect();
    ui.set_summary(match plan.blended {
        true => format!("{} frames blended", weights.len()).into(),
        false => "Nothing to blend".into(),
    });
    ui.set_weights(ModelRc::new(VecModel::from(weights)));
    ui.set_lossless(!rates.is_empty() && rates.iter().all(|fps| lossless(*fps, config)));
}

// what the timeline has that a copy does not keep
fn left(ui: &App, state: &State) {
    let video = |entry: &&crate::state::Entry| entry.audio.is_none() && !entry.still;
    let count = |name: &str, found: usize| {
        (found > 0).then(|| Fact {
            name: name.into(),
            value: found.to_string().into(),
        })
    };
    let entries = || state.entries.iter();
    let faded = |entry: &&crate::state::Entry| entry.fade != (0.0, 0.0) || entry.cross > 0.0;
    let mut fades: Vec<u64> = entries().filter(faded).map(|entry| entry.link).collect();
    fades.dedup();
    let early = |entry: &&crate::state::Entry| {
        let start = f64::from(entry.cut.0) * entry.seconds;
        !entry.keys.is_empty() && entry.key(start, start - 0.005, start + 0.005).is_none()
    };
    let facts = [
        count("Images", entries().filter(|entry| entry.still).count()),
        count("Fades", fades.len()),
        count(
            "Transform",
            entries()
                .filter(video)
                .filter(|entry| entry.place != Place::default())
                .count(),
        ),
        count("Gain", entries().filter(|entry| entry.gain != 0.0).count()),
        count(
            "Exact starts",
            entries().filter(video).filter(early).count(),
        ),
    ];
    let facts: Vec<Fact> = facts.into_iter().flatten().collect();
    ui.set_left(ModelRc::new(VecModel::from(facts)));
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
    ui.set_unsaved(unsaved(entry));
    curve(ui, state);
    ui.set_repeats(repeats(ui, state));
}

// the repeats of the clip that the settings are for, or of the worst clip when they are for all
pub fn repeats(ui: &App, state: &State) -> f32 {
    let repeats = |entry: &crate::state::Entry| interpolini_core::repeats(&entry.differences);
    match state.video(ui) {
        Some(index) => repeats(&state.entries[index]),
        None => state.entries.iter().map(repeats).fold(0.0, f32::max),
    }
}

pub fn only(ui: &App) -> bool {
    !ui.get_copying() && (ui.get_view() == "original" || ui.get_held())
}

fn shrink(ui: &App, wide: u32) -> u32 {
    match ui.get_resolution().as_str() {
        "Full" => 1,
        "1/2" => 2,
        "1/4" => 4,
        _ => {
            let width = wanted(ui).1 as u32;
            [4, 2]
                .into_iter()
                .find(|by| wide / by >= width)
                .unwrap_or(1)
        }
    }
}

pub fn wanted(ui: &App) -> (bool, i32) {
    // a copy has no result to compare with
    let original =
        !ui.get_copying() && (!ui.get_playing() || ui.get_view() != "result" || ui.get_held());
    let shown = ui.get_drawn() * ui.window().scale_factor();
    // in steps, so not each zoom makes a new picture
    let steps = [1280, 1920, 2560, 3840];
    let width = steps.into_iter().find(|step| *step as f32 >= shown);
    (original, width.unwrap_or(i32::MAX))
}

pub fn refresh(ui: &App, shared: &Shared) {
    timecode(ui, shared);
    crate::estimate::ask(ui, shared);
    let mut state = lock(shared);
    layout(ui, &mut state);
    curve(ui, &mut state);
    left(ui, &state);
    ui.set_repeats(repeats(ui, &state));
    let snap = std::mem::take(&mut state.snap);
    ui.set_shot("".into());
    // a render gets all of the gpu, so the preview closes its clips until the render is done
    if ui.get_rendering() || ui.get_preview_off() {
        preview::show(preview::Show {
            parts: Vec::new(),
            canvas: Default::default(),
            play: false,
            quiet: false,
            view: preview::View::Result,
            width: 0,
            size: (0, 0),
            total: 0.0,
            save: None,
        });
        if ui.get_preview_off() {
            ui.set_preview(slint::Image::default());
            ui.set_original(slint::Image::default());
        }
        // a hidden preview has no frames to time playback, so the clock does
        let walk = ui.get_playing() && !state.entries.is_empty();
        drop(state);
        if walk {
            idle(ui, shared);
        }
        return;
    }
    let empty = || {
        ui.set_preview(slint::Image::default());
        ui.set_original(slint::Image::default());
        ui.set_preview_note("".into());
    };
    let spans = state.spans();
    let all = state.parts();
    let seconds = state.seconds();
    let time = (f64::from(ui.get_position()) * seconds).min(seconds - 0.001);
    let inside = |part: &&Part| part.start <= time && time < part.end;
    let shown = all.iter().find(inside).map(|part| (time, part.clone()));
    let shown = shown.filter(|shown| !shown.1.clips.is_empty());
    // without a video the lowest image gives the picture size
    let lowest = shown.as_ref().map(|shown| shown.1.clips[0]);
    let base = state.base().or(lowest);
    let base = base.or_else(|| state.videos().first().copied());
    let Some(base) = base.map(|index| &state.entries[index]) else {
        // only sound is here, so playback walks on by the clock
        empty();
        let walk = ui.get_playing() && !state.entries.is_empty();
        drop(state);
        if walk {
            idle(ui, shared);
        }
        return;
    };
    // a copy shows each video as its file has it
    let copying = ui.get_copying();
    let shrink = shrink(ui, base.size.0);
    let place = |entry: &crate::state::Entry| match copying {
        true => Place::default(),
        false => entry.place,
    };
    let covers = |index: &usize| {
        let entry = &state.entries[*index];
        !entry.still && place(entry).covers(base.size, entry.size)
    };
    let stretch = |part: &Part, time: f64| {
        // a video that fills the picture hides the clips under it
        let fills = |index: &usize| covers(index) && (copying || spans[*index].picture().none());
        let shown = &part.clips[part.clips.iter().rposition(fills).unwrap_or(0)..];
        let shown = shown
            .iter()
            .filter(|index| !copying || !state.entries[**index].still);
        let layer = |index: &usize| {
            let (entry, span) = (&state.entries[*index], &spans[*index]);
            preview::Layer {
                clip: entry.clip.clone(),
                config: Config {
                    shrink,
                    ..match copying {
                        true => preview::source(&entry.config),
                        false => entry.settled(),
                    }
                },
                still: entry.still,
                place: place(entry),
                start: span.start + time - span.at,
                inside: time - span.at,
                length: span.length,
                fade: match copying {
                    true => Fade::default(),
                    false => span.picture(),
                },
                fills: covers(index),
            }
        };
        preview::Stretch {
            layers: shown.map(layer).collect(),
            time,
            end: part.end,
        }
    };
    // a playback goes on through the parts that follow, up to a part without a clip
    let after = |end: f64| {
        let later = all.iter().cloned();
        let later = later.skip_while(move |other| other.start < end - 0.0005);
        later.take_while(|other| !other.clips.is_empty())
    };
    let ask = |parts: Vec<preview::Stretch>,
               quiet: bool,
               view: preview::View,
               width: i32,
               save: Option<bool>| {
        let request = preview::Show {
            parts,
            canvas: base.clip.clone(),
            play: ui.get_playing(),
            quiet,
            view,
            width: (width as u32).min(base.size.0 / shrink),
            size: base.size,
            total: state.seconds(),
            save,
        };
        preview::show(request);
    };
    let Some((time, part)) = shown else {
        // no clip is here, so the picture is empty and playback walks on by the clock
        empty();
        let walk = ui.get_playing();
        if walk {
            let time = f64::from(ui.get_position()) * state.seconds();
            let gap = all.iter().find(|part| part.end > time).cloned();
            let next = gap.map(|gap| after(gap.end).map(|part| stretch(&part, part.start)));
            ask(
                next.map(Iterator::collect).unwrap_or_default(),
                true,
                preview::View::Result,
                0,
                None,
            );
        }
        drop(state);
        if walk {
            idle(ui, shared);
        }
        return;
    };
    let mut parts = vec![stretch(&part, time)];
    if ui.get_playing() {
        parts.extend(after(part.end).map(|part| stretch(&part, part.start)));
    }
    let (original, width) = wanted(ui);
    let bare = only(ui);
    ui.set_compared(original);
    ui.set_bare(bare);
    ui.set_sharp(width);
    let view = match (original, bare) {
        (_, true) => preview::View::Original,
        (true, false) => preview::View::Both,
        (false, false) => preview::View::Result,
    };
    ask(parts, false, view, width, snap.then_some(bare));
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
            let clip = !ui.get_preview_off() && state.locate(next as f32).is_some();
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
