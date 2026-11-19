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
    draft: Entry,
    rows: Vec<Clip>,
    tracks: Tracks,
    size: Option<(u32, u32)>,
    time: f64,
    at: Instant,
}

struct History {
    undo: Vec<Moment>,
    redo: Vec<Moment>,
    cleared: Vec<Moment>,
    recorded: Instant,
    gesture: String,
    evicted: Option<Moment>,
}

thread_local! {
    // the rows hold images of the window thread
    static HISTORY: RefCell<History> = RefCell::new(History {
        undo: Vec::new(),
        redo: Vec::new(),
        cleared: Vec::new(),
        recorded: Instant::now(),
        gesture: String::new(),
        evicted: None,
    });
}

fn moment(ui: &App, state: &State) -> Moment {
    Moment {
        entries: state.entries.clone(),
        draft: state.draft.clone(),
        rows: ui.get_clips().iter().collect(),
        tracks: state.tracks.clone(),
        size: state.project.look.size,
        time: state.time(ui.get_position()),
        at: Instant::now(),
    }
}

// call before a change, the steps of one gesture count as one
pub fn record(ui: &App, state: &mut State, gesture: &str) {
    touch(ui, state);
    HISTORY.with_borrow_mut(|history| {
        // only steps of the same gesture join, any other step between them starts a new one
        let recent = history.recorded.elapsed() < Duration::from_millis(600);
        let joins = recent && !gesture.is_empty() && history.gesture == gesture;
        history.recorded = Instant::now();
        history.gesture = gesture.into();
        if joins {
            return;
        }
        // a forgotten step brings the oldest one back
        history.evicted = (history.undo.len() >= 200).then(|| history.undo.remove(0));
        let at = history.recorded;
        history.undo.push(Moment {
            at,
            ..moment(ui, state)
        });
        history.cleared = std::mem::take(&mut history.redo);
    });
}

pub fn clear() {
    HISTORY.with_borrow_mut(|history| {
        history.gesture.clear();
        history.evicted = None;
        history.undo.clear();
        history.redo.clear();
        history.cleared.clear();
    });
}

pub fn recorded() -> Instant {
    HISTORY.with_borrow(|history| history.recorded)
}

// the step recorded at that time goes, the steps after it stay
pub fn forget(at: Instant) {
    HISTORY.with_borrow_mut(|history| {
        history.gesture.clear();
        let Some(index) = history.undo.iter().rposition(|moment| moment.at == at) else {
            return;
        };
        let last = index + 1 == history.undo.len();
        history.undo.remove(index);
        // the room brings back the step that the last one pushed out
        history.undo.splice(0..0, history.evicted.take());
        if last {
            history.redo = std::mem::take(&mut history.cleared);
        }
    });
}

pub fn amend(change: impl Fn(&mut Entry)) {
    HISTORY.with_borrow_mut(|history| {
        let moments = history
            .undo
            .iter_mut()
            .chain(&mut history.redo)
            .chain(&mut history.cleared)
            .chain(&mut history.evicted);
        moments
            .flat_map(|moment| moment.entries.iter_mut().chain([&mut moment.draft]))
            .for_each(change);
    });
}

pub fn travel(ui: &App, shared: &Shared, back: bool) {
    let now = moment(ui, &lock(shared));
    let target = HISTORY.with_borrow_mut(|history| {
        history.gesture.clear();
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

pub fn revert(ui: &App, shared: &Shared) {
    let target = HISTORY.with_borrow_mut(|history| {
        history.gesture.clear();
        history.redo = std::mem::take(&mut history.cleared);
        let target = history.undo.pop();
        history.undo.splice(0..0, history.evicted.take());
        target
    });
    if let Some(target) = target {
        restore(ui, shared, target);
    }
}

fn restore(ui: &App, shared: &Shared, target: Moment) {
    let mut state = lock(shared);
    touch(ui, &mut state);
    state.entries = target.entries;
    state.draft = target.draft;
    for entry in state
        .entries
        .iter_mut()
        .filter(|entry| entry.differences.is_empty())
    {
        entry.measured = None;
    }
    state.tracks = target.tracks;
    state.spare = None;
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
            if let Some(ui) = weak.upgrade()
                && !crate::editor::travel(&ui, &shared, back)
            {
                travel(&ui, &shared, back);
            }
        };
        match back {
            true => ui.on_undo(travel),
            false => ui.on_redo(travel),
        }
    }
}
