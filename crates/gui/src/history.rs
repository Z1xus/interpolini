use std::cell::RefCell;
use std::sync::Arc;
use std::time::{Duration, Instant};

use slint::{ComponentHandle, Model, ModelRc, VecModel};

use crate::project::touch;
use crate::state::{Entry, Shared, State, Tracks, lock};
use crate::view::{fit, load, refresh};
use crate::{App, Clip};

struct Moment {
    entries: Vec<Entry>,
    rows: Vec<Clip>,
    tracks: Tracks,
    size: Option<(u32, u32)>,
    time: f64,
}

struct History {
    undo: Vec<Moment>,
    redo: Vec<Moment>,
    recorded: Instant,
}

thread_local! {
    // the rows hold images, so the history stays on the thread of the window
    static HISTORY: RefCell<History> = RefCell::new(History {
        undo: Vec::new(),
        redo: Vec::new(),
        recorded: Instant::now(),
    });
}

fn moment(ui: &App, state: &State) -> Moment {
    Moment {
        entries: state.entries.clone(),
        rows: ui.get_clips().iter().collect(),
        tracks: state.tracks.clone(),
        size: state.project.look.size,
        time: state.time(ui.get_position()),
    }
}

// call before a change, a drag or a slider gives many changes and they count as one
pub fn record(ui: &App, state: &mut State, gesture: bool) {
    touch(ui, state);
    HISTORY.with_borrow_mut(|history| {
        let recent = history.recorded.elapsed() < Duration::from_millis(600);
        history.recorded = Instant::now();
        if gesture && recent {
            return;
        }
        history.undo.push(moment(ui, state));
        history.redo.clear();
        if history.undo.len() > 200 {
            history.undo.remove(0);
        }
    });
}

pub fn clear() {
    HISTORY.with_borrow_mut(|history| {
        history.undo.clear();
        history.redo.clear();
    });
}

pub fn travel(ui: &App, shared: &Shared, back: bool) {
    let now = moment(ui, &lock(shared));
    let target = HISTORY.with_borrow_mut(|history| {
        let History { undo, redo, .. } = history;
        let (from, to) = if back { (undo, redo) } else { (redo, undo) };
        let target = from.pop()?;
        to.push(now);
        Some(target)
    });
    if let Some(target) = target {
        restore(ui, shared, target);
    }
}

// takes back a change that is not possible, it leaves nothing to redo
pub fn revert(ui: &App, shared: &Shared) {
    if let Some(target) = HISTORY.with_borrow_mut(|history| history.undo.pop()) {
        restore(ui, shared, target);
    }
}

fn restore(ui: &App, shared: &Shared, target: Moment) {
    let mut state = lock(shared);
    touch(ui, &mut state);
    state.entries = target.entries;
    for entry in state
        .entries
        .iter_mut()
        .filter(|entry| entry.differences.is_empty())
    {
        entry.measured = None;
    }
    state.tracks = target.tracks;
    if !state.project.look.chosen {
        state.project.look.size = target.size;
    }
    ui.set_clips(ModelRc::new(VecModel::from(target.rows)));
    ui.set_playing(false);
    ui.set_selected(-1);
    drop(state);
    crate::clips::fill(ui, shared);
    fit(ui, shared);
    let span = lock(shared).span.max(0.001);
    ui.set_position((target.time / span).clamp(0.0, 1.0) as f32);
    load(ui, &mut lock(shared));
    refresh(ui, shared);
}

pub fn wire(ui: &App, state: &Shared) {
    for back in [true, false] {
        let (weak, shared) = (ui.as_weak(), Arc::clone(state));
        let travel = move || {
            if let Some(ui) = weak.upgrade() {
                travel(&ui, &shared, back);
            }
        };
        match back {
            true => ui.on_undo(travel),
            false => ui.on_redo(travel),
        }
    }
}
