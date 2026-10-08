use std::sync::Arc;

use slint::{ComponentHandle, Model, VecModel};

use crate::history::record;
use crate::state::{Entry, Shared, State, lock};
use crate::view::{fit, lanes, layout, load, refresh, select, selected};
use crate::{App, Clip};

// a clip that lands on other clips of its track takes their place there
pub fn overwrite(ui: &App, state: &mut State, link: u64) {
    let rows = ui.get_clips();
    let Some(model) = rows.as_any().downcast_ref::<VecModel<Clip>>() else {
        return;
    };
    let movers: Vec<Entry> = state
        .entries
        .iter()
        .filter(|entry| entry.link == link)
        .cloned()
        .collect();
    for mover in movers {
        let (from, to) = (mover.at, mover.at + mover.kept());
        for index in (0..state.entries.len()).rev() {
            let other = state.entries[index].clone();
            let same = other.audio.is_some() == mover.audio.is_some() && other.track == mover.track;
            let (start, end) = (other.at, other.at + other.kept());
            if other.link == link || !same || to <= start + 0.001 || end <= from + 0.001 {
                continue;
            }
            let Some(mut row) = model.row_data(index) else {
                continue;
            };
            let place = |time: f64| row.cut_in + ((time - start) / other.seconds) as f32;
            match (from <= start, to >= end) {
                (true, true) => {
                    state.entries.remove(index);
                    model.remove(index);
                    continue;
                }
                (true, false) => (row.cut_in, row.at) = (place(to), to as f32),
                (false, true) => row.cut_out = place(from),
                (false, false) => {
                    let mut rest = row.clone();
                    (rest.cut_in, rest.at) = (place(to), to as f32);
                    row.cut_out = place(from);
                    // the second piece is a clip of its own, it does not move with the first
                    state.ids += 2;
                    rest.link = state.ids as i32;
                    let copy = Entry {
                        id: state.ids - 1,
                        link: state.ids,
                        ..other.clone()
                    };
                    state.entries.insert(index + 1, copy);
                    model.insert(index + 1, rest);
                }
            }
            model.set_row_data(index, row);
        }
    }
    layout(ui, state);
}

pub fn discard(ui: &App, state: &mut State, gone: impl Fn(&Entry) -> bool) {
    let rows = ui.get_clips();
    let Some(rows) = rows.as_any().downcast_ref::<VecModel<Clip>>() else {
        return;
    };
    for index in (0..state.entries.len()).rev() {
        if gone(&state.entries[index]) {
            state.entries.remove(index);
            rows.remove(index);
        }
    }
}

// the files of the selected clips: the one that was clicked, and those that ctrl added
pub fn chosen(ui: &App, state: &State) -> Vec<u64> {
    let rows = ui.get_clips();
    let picked = |index: &usize| rows.row_data(*index).is_some_and(|row| row.picked);
    let places =
        (0..state.entries.len()).filter(|index| picked(index) || selected(ui) == Some(*index));
    let mut links: Vec<u64> = places.map(|index| state.entries[index].link).collect();
    links.dedup();
    links
}

pub fn wire(ui: &App, state: &Shared) {
    let (weak, shared) = (ui.as_weak(), Arc::clone(state));
    ui.on_extend(move |index| {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        let state = lock(&shared);
        let rows = ui.get_clips();
        let Some(link) = state.entries.get(index as usize).map(|entry| entry.link) else {
            return;
        };
        // the clip that was selected alone joins the others
        let first = selected(&ui).and_then(|index| state.entries.get(index));
        let first = first.map(|entry| entry.link);
        let add = !rows.row_data(index as usize).is_some_and(|row| row.picked);
        for (place, entry) in state.entries.iter().enumerate() {
            let Some(mut row) = rows.row_data(place) else {
                continue;
            };
            if entry.link == link {
                row.picked = add;
            } else if Some(entry.link) == first {
                row.picked = true;
            } else {
                continue;
            }
            rows.set_row_data(place, row);
        }
        drop(state);
        if add {
            ui.set_selected(index);
            load(&ui, &mut lock(&shared));
        }
    });

    let remove = |all: bool| {
        let (weak, shared) = (ui.as_weak(), Arc::clone(state));
        move || {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let mut state = lock(&shared);
            let links = chosen(&ui, &state);
            if (links.is_empty() && !all) || ui.get_rendering() {
                return;
            }
            record(&ui, &mut state, false);
            discard(&ui, &mut state, |entry| all || links.contains(&entry.link));
            drop(state);
            fit(&ui, &shared);
            select(&ui, &shared, -1);
            refresh(&ui, &shared);
        }
    };
    ui.on_remove_clip(remove(false));
    ui.on_close_clips(remove(true));

    let (weak, shared) = (ui.as_weak(), Arc::clone(state));
    ui.on_mark(move |kind| {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        let rows = ui.get_clips();
        let Some(model) = rows.as_any().downcast_ref::<VecModel<Clip>>() else {
            return;
        };
        let mut state = lock(&shared);
        let time = f64::from(ui.get_position()) * state.seconds();
        // a part shorter than this is not worth a clip
        let least = 0.05;
        let under =
            |entry: &Entry| entry.at + least < time && time < entry.at + entry.kept() - least;
        // a split cuts every clip under the playhead, the other marks are for the selected clip
        let chosen = selected(&ui).or_else(|| state.locate(ui.get_position()));
        let link = chosen
            .and_then(|index| state.entries.get(index))
            .map(|entry| entry.link);
        let targets: Vec<usize> = match kind.as_str() {
            "split" => (0..state.entries.len())
                .filter(|index| under(&state.entries[*index]))
                .collect(),
            _ => (0..state.entries.len())
                .filter(|index| Some(state.entries[*index].link) == link)
                .collect(),
        };
        if targets.is_empty() {
            return;
        }
        record(&ui, &mut state, false);
        let first = state.ids + 1;
        for index in targets.into_iter().rev() {
            let Some(mut row) = model.row_data(index) else {
                continue;
            };
            let entry = state.entries[index].clone();
            let length = entry.seconds as f32;
            let local =
                (row.cut_in + (time as f32 - row.at) / length).clamp(row.cut_in, row.cut_out);
            match kind.as_str() {
                "start" if under(&entry) => {
                    // the end of the clip stays where it is
                    row.at += (local - row.cut_in) * length;
                    row.cut_in = local;
                }
                "end" if under(&entry) => row.cut_out = local,
                "split" => {
                    // the second halves of clips with one link get one new link
                    let mut second = row.clone();
                    second.at = time as f32;
                    second.cut_in = local;
                    second.link = (first + entry.link) as i32;
                    row.cut_out = local;
                    model.insert(index + 1, second);
                    state.ids += 1;
                    let copy = Entry {
                        id: first + state.ids + entry.link,
                        link: first + entry.link,
                        ..entry
                    };
                    state.entries.insert(index + 1, copy);
                }
                "clear" if !entry.still => {
                    row.at = (row.at - row.cut_in * length).max(0.0);
                    (row.cut_in, row.cut_out) = (0.0, 1.0);
                }
                _ => {}
            }
            model.set_row_data(index, row);
        }
        // the new ids and links must stay below the next ones
        state.ids = state
            .entries
            .iter()
            .map(|entry| entry.id.max(entry.link))
            .max()
            .unwrap_or(0);
        drop(state);
        fit(&ui, &shared);
        let total = lock(&shared).seconds().max(0.001);
        ui.set_position((time / total).clamp(0.0, 1.0) as f32);
        ui.set_playing(false);
        refresh(&ui, &shared);
    });

    let (weak, shared) = (ui.as_weak(), Arc::clone(state));
    ui.on_grab(move |index, time, alone| {
        let mut state = lock(&shared);
        let Some(ui) = weak.upgrade() else {
            return;
        };
        record(&ui, &mut state, false);
        // with alt the clip leaves the other clips of its file, and moves and cuts on its own
        if let Some(mut row) = ui.get_clips().row_data(index as usize).filter(|_| alone) {
            state.ids += 1;
            row.link = state.ids as i32;
            state.entries[index as usize].link = state.ids;
            ui.get_clips().set_row_data(index as usize, row);
            ui.set_link(state.ids as i32);
        }
        let held = state
            .entries
            .get(index as usize)
            .map(|entry| (entry.id, f64::from(time) - entry.at));
        state.held = held;
        state.touched = state.entries.get(index as usize).map(|entry| entry.link);
    });

    let (weak, shared) = (ui.as_weak(), Arc::clone(state));
    ui.on_drag(move |time, depth, slack| {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        let rows = ui.get_clips();
        let mut state = lock(&shared);
        let Some((id, offset)) = state.held else {
            return;
        };
        let Some(index) = state.entries.iter().position(|entry| entry.id == id) else {
            return;
        };
        let entry = state.entries[index].clone();
        let mut at = (f64::from(time) - offset).max(0.0);
        // the start or the end of the clip goes onto a near edge: the playhead or another clip
        let playhead = f64::from(ui.get_position()) * state.seconds();
        let others = state
            .entries
            .iter()
            .filter(|other| other.link != entry.link);
        let edges = others.flat_map(|other| [other.at, other.at + other.kept()]);
        let edges = edges.chain([0.0, playhead]);
        let near = edges.flat_map(|edge| [edge - at, edge - at - entry.kept()]);
        let near = near.min_by(|a, b| a.abs().total_cmp(&b.abs()));
        if let Some(near) = near.filter(|near| near.abs() < f64::from(slack)) {
            at = (at + near).max(0.0);
        }
        // the clips of one file move as one, with the other selected clips, and none of them goes before the start
        let mut group = chosen(&ui, &state);
        if !group.contains(&entry.link) {
            group = vec![entry.link];
        }
        let linked = |other: &&Entry| group.contains(&other.link);
        let earliest = state
            .entries
            .iter()
            .filter(linked)
            .map(|other| other.at)
            .fold(f64::MAX, f64::min);
        let moved = (at - entry.at).max(-earliest);
        let track = state.reach(entry.audio.is_some(), depth);
        lanes(&ui, &state);
        let mut sounds = 0;
        for (other, linked) in state.entries.iter().enumerate() {
            let Some(mut row) = rows
                .row_data(other)
                .filter(|_| group.contains(&linked.link))
            else {
                continue;
            };
            row.at = (linked.at + moved) as f32;
            if other == index {
                row.track = track as i32;
            } else if linked.link == entry.link && entry.audio.is_none() && linked.audio.is_some() {
                // the sounds of a video go up and down with it
                let lane = (linked.track as i32 + track as i32 - entry.track as i32).max(0);
                row.track = lane;
                sounds = sounds.max(lane as usize + 1);
            }
            rows.set_row_data(other, row);
        }
        if state.tracks.audio.len() < sounds {
            state.tracks.audio.resize(sounds, (36.0, false));
            lanes(&ui, &state);
        }
        layout(&ui, &mut state);
    });

    let (weak, shared) = (ui.as_weak(), Arc::clone(state));
    ui.on_release(move || {
        if let Some(ui) = weak.upgrade() {
            let mut state = lock(&shared);
            state.held = None;
            state.spare = None;
            if let Some(link) = state.touched.take() {
                overwrite(&ui, &mut state, link);
            }
            drop(state);
            fit(&ui, &shared);
            refresh(&ui, &shared);
        }
    });

    let (weak, shared) = (ui.as_weak(), Arc::clone(state));
    ui.on_close_gaps(move || {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        let rows = ui.get_clips();
        let mut state = lock(&shared);
        record(&ui, &mut state, false);
        // on each track the clips keep their order and move up to each other
        let mut order: Vec<usize> = (0..state.entries.len()).collect();
        order.sort_by(|a, b| state.entries[*a].at.total_cmp(&state.entries[*b].at));
        let has_video = |link: u64| {
            state
                .entries
                .iter()
                .any(|entry| entry.link == link && entry.audio.is_none())
        };
        let mut ends = std::collections::HashMap::new();
        let mut moves = std::collections::HashMap::new();
        for &index in &order {
            let entry = &state.entries[index];
            // a sound follows its video, and a sound alone packs on its own track
            if entry.audio.is_some() && has_video(entry.link) {
                continue;
            }
            let end = ends
                .entry((entry.audio.is_some(), entry.track))
                .or_insert(0.0);
            moves.insert(entry.link, *end - entry.at);
            *end += entry.kept();
        }
        for (index, entry) in state.entries.iter().enumerate() {
            if let (Some(mut row), Some(moved)) = (rows.row_data(index), moves.get(&entry.link)) {
                row.at = (entry.at + moved).max(0.0) as f32;
                rows.set_row_data(index, row);
            }
        }
        drop(state);
        fit(&ui, &shared);
        refresh(&ui, &shared);
    });

    let (weak, shared) = (ui.as_weak(), Arc::clone(state));
    ui.on_trimmed(move |index, slack| {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        // the other clips of the same file get the same cut
        let rows = ui.get_clips();
        let mut state = lock(&shared);
        state.touched = state.entries.get(index as usize).map(|entry| entry.link);
        let (Some(mut row), Some(entry)) = (
            rows.row_data(index as usize),
            state.entries.get(index as usize),
        ) else {
            return;
        };
        let link = entry.link;
        let head = row.cut_in != entry.cut.0;
        let edge = match head {
            true => row.at,
            false => row.at + (row.cut_out - row.cut_in) * row.length,
        };
        let playhead = f64::from(ui.get_position()) * state.seconds();
        let others = state.entries.iter().filter(|other| other.link != link);
        let edges = others.flat_map(|other| [other.at, other.at + other.kept()]);
        let near = edges
            .chain([0.0, playhead])
            .map(|other| other as f32 - edge)
            .min_by(|a, b| a.abs().total_cmp(&b.abs()));
        let moved = near.filter(|near| near.abs() < slack).unwrap_or(0.0) / row.length;
        match head {
            true if row.cut_in + moved >= 0.0 => {
                row.cut_in += moved;
                row.at += moved * row.length;
            }
            false if row.cut_out + moved <= 1.0 => row.cut_out += moved,
            _ => {}
        }
        rows.set_row_data(index as usize, row.clone());
        for (other, entry) in state.entries.iter().enumerate() {
            let Some(mut linked) = rows
                .row_data(other)
                .filter(|_| entry.link == link && other != index as usize)
            else {
                continue;
            };
            (linked.at, linked.cut_in, linked.cut_out) = (row.at, row.cut_in, row.cut_out);
            rows.set_row_data(other, linked);
        }
        layout(&ui, &mut state);
    });

    for start in [true, false] {
        let (weak, shared) = (ui.as_weak(), Arc::clone(state));
        let record = move || {
            if let Some(ui) = weak.upgrade() {
                record(&ui, &mut lock(&shared), false);
            }
        };
        match start {
            true => ui.on_trim_start(record),
            false => ui.on_place_start(record),
        }
    }

    let (weak, shared) = (ui.as_weak(), Arc::clone(state));
    ui.on_placed(move || {
        if let Some(ui) = weak.upgrade() {
            refresh(&ui, &shared);
        }
    });

    let (weak, shared) = (ui.as_weak(), Arc::clone(state));
    ui.on_trim_done(move || {
        if let Some(ui) = weak.upgrade() {
            let mut state = lock(&shared);
            if let Some(link) = state.touched.take() {
                overwrite(&ui, &mut state, link);
            }
            drop(state);
            fit(&ui, &shared);
            refresh(&ui, &shared);
        }
    });
}
