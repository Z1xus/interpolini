use std::cell::RefCell;
use std::sync::Arc;

use slint::ComponentHandle;

use crate::state::{Shared, lock, sounds};
use crate::view::{refresh, timecode, wanted};
use crate::{App, sound};

thread_local! {
    // the sound card stream stays on the thread of the window
    static PLAYER: RefCell<Option<sound::Player>> = const { RefCell::new(None) };
}

pub fn wire(ui: &App, state: &Shared) {
    let (weak, shared) = (ui.as_weak(), Arc::clone(state));
    ui.on_step(move |frames, seconds| {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        let state = lock(&shared);
        // in a gap one step is a 60th of a second
        let frame = match state.locate(ui.get_position()) {
            Some(_) => f64::from(ui.get_tick()),
            None => 1.0 / 60.0,
        };
        let moved = (f64::from(frames) * frame + f64::from(seconds)) / state.seconds().max(0.001);
        drop(state);
        ui.set_position((f64::from(ui.get_position()) + moved).clamp(0.0, 1.0) as f32);
        ui.set_playing(false);
        refresh(&ui, &shared);
    });

    let (weak, shared) = (ui.as_weak(), Arc::clone(state));
    ui.on_sounding(move || {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        let state = lock(&shared);
        let time = f64::from(ui.get_position()) * state.seconds();
        let player = ui
            .get_playing()
            .then(|| sound::Player::start(&sounds(&state), time))
            .flatten();
        PLAYER.set(player);
    });

    let (weak, shared) = (ui.as_weak(), Arc::clone(state));
    ui.on_ended(move || {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        let state = lock(&shared);
        // the playhead is at the end of the part, which is also the start of the next one
        let time = f64::from(ui.get_position()) * state.seconds() - 0.001;
        let part = state
            .parts()
            .into_iter()
            .find(|part| part.start <= time && time < part.end);
        let next = part.map_or(1.0, |part| part.end / state.seconds());
        drop(state);
        if next >= 0.999 || !ui.get_playing() {
            ui.set_playing(false);
            return;
        }
        ui.set_position(next as f32 + 0.000_01);
        refresh(&ui, &shared);
    });

    let (weak, shared) = (ui.as_weak(), Arc::clone(state));
    ui.on_seek(move || {
        if let Some(ui) = weak.upgrade() {
            ui.set_playing(false);
            refresh(&ui, &shared);
        }
    });

    let (weak, shared) = (ui.as_weak(), Arc::clone(state));
    ui.on_viewed(move || {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        let (original, width) = wanted(&ui);
        let enough =
            (ui.get_compared() || !original) && (ui.get_playing() || ui.get_sharp() >= width);
        if !enough {
            refresh(&ui, &shared);
        }
    });

    let (weak, shared) = (ui.as_weak(), Arc::clone(state));
    ui.on_moved(move || {
        if let Some(ui) = weak.upgrade() {
            timecode(&ui, &shared);
        }
    });

    let (weak, shared) = (ui.as_weak(), Arc::clone(state));
    ui.on_play(move || {
        if let Some(ui) = weak.upgrade() {
            ui.set_playing(!ui.get_playing());
            if ui.get_playing() && ui.get_position() >= 1.0 {
                ui.set_position(0.0);
            }
            refresh(&ui, &shared);
        }
    });
}
