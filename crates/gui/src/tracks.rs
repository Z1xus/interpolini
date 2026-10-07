use std::sync::Arc;

use slint::{ComponentHandle, Model};

use crate::App;
use crate::edit::discard;
use crate::history::record;
use crate::state::{Shared, lock};
use crate::view::{fit, lanes, refresh, select};

pub fn wire(ui: &App, state: &Shared) {
    let (weak, shared) = (ui.as_weak(), Arc::clone(state));
    ui.on_lane_height(move |audio, index, height| {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        let mut state = lock(&shared);
        let (index, height) = (index as usize, height.clamp(24.0, 400.0));
        match audio {
            true => state.tracks.audio[index].0 = height,
            false => state.tracks.video[index] = height,
        }
        lanes(&ui, &state);
    });

    let (weak, shared) = (ui.as_weak(), Arc::clone(state));
    ui.on_lane_wheel(move |depth, delta| {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        let mut state = lock(&shared);
        let (_, audio, content) = state.places();
        if !(0.0..content).contains(&depth) {
            return;
        }
        let sound = audio.first().is_some_and(|top| depth >= *top);
        let index = state.track(sound, depth);
        let height = match sound {
            true => &mut state.tracks.audio[index].0,
            false => &mut state.tracks.video[index],
        };
        *height = (*height + 8.0 * delta.signum()).clamp(24.0, 400.0);
        lanes(&ui, &state);
    });

    let (weak, shared) = (ui.as_weak(), Arc::clone(state));
    ui.on_add_lane(move |audio| {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        let mut state = lock(&shared);
        record(&ui, &mut state, false);
        match audio {
            true => state.tracks.audio.push((36.0, false)),
            false => state.tracks.video.push(64.0),
        }
        lanes(&ui, &state);
    });

    let (weak, shared) = (ui.as_weak(), Arc::clone(state));
    ui.on_remove_lane(move |audio, index| {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        let rows = ui.get_clips();
        let mut state = lock(&shared);
        let index = index as usize;
        if !audio && state.tracks.video.len() < 2 {
            return;
        }
        record(&ui, &mut state, false);
        // the clips on the track go with it, and the tracks above it move down
        discard(&ui, &mut state, |entry| {
            entry.audio.is_some() == audio && entry.track == index
        });
        for (place, entry) in state.entries.iter().enumerate() {
            let Some(mut row) = rows
                .row_data(place)
                .filter(|_| entry.audio.is_some() == audio && entry.track > index)
            else {
                continue;
            };
            row.track -= 1;
            rows.set_row_data(place, row);
        }
        match audio {
            true => drop(state.tracks.audio.remove(index)),
            false => drop(state.tracks.video.remove(index)),
        }
        drop(state);
        fit(&ui, &shared);
        select(&ui, &shared, -1);
        refresh(&ui, &shared);
    });

    let (weak, shared) = (ui.as_weak(), Arc::clone(state));
    ui.on_mute_lane(move |index| {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        let mut state = lock(&shared);
        if let Some(track) = state.tracks.audio.get_mut(index as usize) {
            track.1 = !track.1;
        }
        lanes(&ui, &state);
    });
}
