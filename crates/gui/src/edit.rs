use std::collections::HashMap;
use std::sync::Arc;

use slint::{ComponentHandle, Model, VecModel};

use crate::history::{record, revert};
use crate::state::{Entry, Shared, State, lock};
use crate::view::{fit, lanes, layout, load, refit, refresh, select, selected};
use crate::{App, Clip};

// moved is how far the clip came, a clip that is not moved gives 0
pub fn overwrite(ui: &App, state: &mut State, link: u64, moved: f64) {
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
    // a video that is cut takes its sounds with it, on any track
    let under = |video: &&Entry| {
        let covers = |mover: &Entry| {
            let (from, to) = (mover.at, mover.at + mover.kept());
            let (start, end) = (video.at, video.at + video.kept());
            mover.audio.is_none()
                && mover.track == video.track
                && from < end - 0.001
                && start < to - 0.001
        };
        video.link != link && video.audio.is_none() && movers.iter().any(covers)
    };
    let cut: Vec<u64> = state
        .entries
        .iter()
        .filter(under)
        .map(|video| video.link)
        .collect();
    let mut pieces = HashMap::new();
    for mover in &movers {
        let (from, to) = (mover.at, mover.at + mover.kept());
        for index in (0..state.entries.len()).rev() {
            let other = state.entries[index].clone();
            let same = other.audio.is_some() == mover.audio.is_some() && other.track == mover.track;
            let follows =
                mover.audio.is_none() && other.audio.is_some() && cut.contains(&other.link);
            let (start, end) = (other.at, other.at + other.kept());
            if other.link == link
                || !(same || follows)
                || to <= start + 0.001
                || end <= from + 0.001
            {
                continue;
            }
            let Some(mut row) = model.row_data(index) else {
                continue;
            };
            // a clip that came from one side takes the other clip up to that side, and does not split it
            let (from, to) = match () {
                () if from - moved >= end - 0.001 => (from, to.max(end)),
                () if to - moved <= start + 0.001 => (from.min(start), to),
                () => (from, to),
            };
            let place = |time: f64| row.cut_in + ((time - start) / other.length()) as f32;
            match (from <= start, to >= end) {
                (true, true) => {
                    let gone = state.entries.remove(index);
                    crate::clips::forget(state, &gone);
                    model.remove(index);
                    continue;
                }
                (true, false) => (row.cut_in, row.at) = (place(to), to as f32),
                (false, true) => row.cut_out = place(from),
                (false, false) => {
                    let mut rest = row.clone();
                    (rest.cut_in, rest.at) = (place(to), to as f32);
                    row.cut_out = place(from);
                    (row.fade_out, rest.fade_in, rest.cross) = (0.0, 0.0, 0.0);
                    // the second pieces are a clip of their own, they do not move with the first
                    state.ids += 1;
                    let id = state.ids;
                    let fresh = *pieces.entry(other.link).or_insert_with(|| {
                        state.ids += 1;
                        state.ids
                    });
                    rest.link = fresh as i32;
                    let mut copy = other.clone();
                    copy.saved.id = id;
                    copy.saved.link = fresh;
                    copy.saved.cut = (rest.cut_in, rest.cut_out);
                    copy.saved.at = to;
                    state.entries.insert(index + 1, copy);
                    model.insert(index + 1, rest);
                }
            }
            // the next mover must see this cut
            (state.entries[index].cut, state.entries[index].at) =
                ((row.cut_in, row.cut_out), f64::from(row.at));
            model.set_row_data(index, row);
        }
    }
    layout(ui, state);
}

fn clashes(state: &State, link: u64) -> bool {
    let over = |mover: &Entry, other: &Entry| {
        let same = other.audio.is_some() == mover.audio.is_some() && other.track == mover.track;
        let apart = mover.at >= other.at + other.kept() - 0.001
            || other.at >= mover.at + mover.kept() - 0.001;
        other.link != link && same && !apart
    };
    let mut movers = state.entries.iter().filter(|entry| entry.link == link);
    movers.any(|mover| state.entries.iter().any(|other| over(mover, other)))
}

// the move nearest to the wanted one that puts no clip of these files over a clip of another file
fn clear(state: &State, group: &[u64], index: usize, track: usize, moved: f64) -> f64 {
    let held = &state.entries[index];
    let lane = |place: usize, entry: &Entry| match () {
        () if place == index => track,
        () if entry.link == held.link && held.audio.is_none() && entry.audio.is_some() => {
            (entry.track + track).saturating_sub(held.track)
        }
        () => entry.track,
    };
    // the start, the length, the kind and the track
    let shape = |place: usize, entry: &Entry| {
        let own = group.contains(&entry.link);
        let track = if own { lane(place, entry) } else { entry.track };
        (own, (entry.at, entry.kept(), entry.audio.is_some(), track))
    };
    let shapes = state.entries.iter().enumerate();
    let (movers, walls): (Vec<_>, Vec<_>) = shapes
        .map(|(place, entry)| shape(place, entry))
        .partition(|shape| shape.0);
    let pairs = || {
        movers.iter().flat_map(|(_, mover)| {
            let same = move |wall: &&(bool, (f64, f64, bool, usize))| {
                wall.1.2 == mover.2 && wall.1.3 == mover.3
            };
            walls.iter().filter(same).map(move |wall| (mover, &wall.1))
        })
    };
    let free = |moved: f64| {
        movers.iter().all(|(_, mover)| mover.0 + moved >= -0.0005)
            && pairs().all(|(mover, wall)| {
                let start = mover.0 + moved;
                start >= wall.0 + wall.1 - 0.0005 || wall.0 >= start + mover.1 - 0.0005
            })
    };
    let sides =
        pairs().flat_map(|(mover, wall)| [wall.0 - mover.1 - mover.0, wall.0 + wall.1 - mover.0]);
    let spots: Vec<f64> = [moved, 0.0]
        .into_iter()
        .chain(sides)
        .filter(|moved| free(*moved))
        .collect();
    let near = |spots: &mut dyn Iterator<Item = &f64>| {
        let best = spots.min_by(|a, b| (*a - moved).abs().total_cmp(&(*b - moved).abs()));
        best.copied()
    };
    let (from, to) = (moved.min(0.0) - 0.0005, moved.max(0.0) + 0.0005);
    near(&mut spots.iter().filter(|spot| (from..=to).contains(*spot)))
        .or_else(|| near(&mut spots.iter()))
        .unwrap_or(0.0)
}

// a clip without keyframes keeps the time, and a clip with no keyframe inside gives none
fn snap(state: &State, entry: &Entry, time: f64) -> Option<f64> {
    let video = |other: &&Entry| {
        other.link == entry.link && other.audio.is_none() && !other.keys.is_empty()
    };
    let Some(video) = state.entries.iter().find(video) else {
        return Some(time);
    };
    let start = f64::from(video.cut.0) * video.seconds;
    let end = f64::from(video.cut.1) * video.seconds;
    let speed = f64::from(video.speed);
    let key = video.key(start + (time - video.at) * speed, start + 0.05, end - 0.05)?;
    Some(video.at + (key - start) / speed)
}

pub fn discard(ui: &App, state: &mut State, gone: impl Fn(&Entry) -> bool) {
    let rows = ui.get_clips();
    let Some(rows) = rows.as_any().downcast_ref::<VecModel<Clip>>() else {
        return;
    };
    for index in (0..state.entries.len()).rev() {
        if gone(&state.entries[index]) {
            let gone = state.entries.remove(index);
            crate::clips::forget(state, &gone);
            rows.remove(index);
        }
    }
}

pub fn pause(ui: &App, state: &Shared) {
    if ui.get_playing() {
        lock(state).resume = true;
        ui.set_playing(false);
    }
}

pub fn resume(ui: &App, state: &Shared) {
    let mut held = lock(state);
    let again = std::mem::take(&mut held.resume);
    let ended = held.time(ui.get_position()) >= held.seconds() - 0.001;
    drop(held);
    if again && !ended {
        ui.invoke_play();
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

// the rows that a change in the menu of a clip is for: its file with the other selected clips, or it alone
fn aimed(ui: &App, state: &State, index: usize, alone: bool) -> Vec<usize> {
    let Some(entry) = state.entries.get(index).filter(|_| !alone) else {
        return vec![index];
    };
    let mut group = chosen(ui, state);
    if !group.contains(&entry.link) {
        group = vec![entry.link];
    }
    let places = 0..state.entries.len();
    places
        .filter(|place| group.contains(&state.entries[*place].link))
        .collect()
}

fn bands(state: &State) -> Vec<(bool, usize, f32, f32)> {
    let (video, audio, _) = state.places();
    let video = video.into_iter().zip(&state.tracks.video).enumerate();
    let audio = audio.into_iter().zip(&state.tracks.audio).enumerate();
    let video = video.map(|(track, (top, height))| (false, track, top, top + height));
    let audio = audio.map(|(track, (top, (height, _)))| (true, track, top, top + height));
    video.chain(audio).collect()
}

fn hole(state: &State, time: f64, depth: f32) -> Option<(f64, f64)> {
    let band = |band: &&(bool, usize, f32, f32)| band.2 <= depth && depth < band.3;
    let bands = bands(state);
    let (sound, track, ..) = bands.iter().find(band)?;
    let lane = || {
        let own = |entry: &&Entry| (entry.audio.is_some(), entry.track) == (*sound, *track);
        state.entries.iter().filter(own)
    };
    if lane().any(|entry| entry.at < time && time < entry.at + entry.kept()) {
        return None;
    }
    let ends = lane().map(|entry| entry.at + entry.kept());
    let before = ends.filter(|end| *end <= time).fold(0.0, f64::max);
    let after = lane().map(|entry| entry.at);
    let after = after.filter(|at| *at >= time).fold(f64::MAX, f64::min);
    (after != f64::MAX && after - before > 0.001).then_some((before, after))
}

fn ripple(ui: &App, state: &mut State, time: f64, mut by: f64) {
    let mut firsts: HashMap<u64, f64> = HashMap::new();
    for entry in &state.entries {
        let first = firsts.entry(entry.link).or_insert(f64::MAX);
        *first = first.min(entry.at);
    }
    let moves: Vec<bool> = (state.entries.iter())
        .map(|entry| firsts[&entry.link] >= time - 0.001)
        .collect();
    if by < 0.0 {
        let entries = || state.entries.iter().zip(&moves);
        for (mover, _) in entries().filter(|(_, moves)| **moves) {
            by = by.max(-mover.at);
            let before = entries().filter(|(other, moves)| {
                let same = other.audio.is_some() == mover.audio.is_some();
                !**moves
                    && same
                    && other.track == mover.track
                    && other.at + other.kept() <= mover.at + 0.001
            });
            for (other, _) in before {
                by = by.max(other.at + other.kept() - mover.at);
            }
        }
    }
    let rows = ui.get_clips();
    for (place, entry) in state.entries.iter_mut().enumerate() {
        if let (true, Some(mut row)) = (moves[place], rows.row_data(place)) {
            entry.at += by;
            row.at = entry.at as f32;
            rows.set_row_data(place, row);
        }
    }
}

fn pick(ui: &App, state: &State, wanted: impl Fn(&Entry) -> bool) {
    let rows = ui.get_clips();
    for (place, entry) in state.entries.iter().enumerate() {
        let picked = wanted(entry);
        if let Some(mut row) = rows.row_data(place).filter(|row| row.picked != picked) {
            row.picked = picked;
            rows.set_row_data(place, row);
        }
    }
}

fn close_gap(ui: &App, shared: &Shared, time: f64, depth: f32) {
    let mut state = lock(shared);
    let Some((before, after)) = hole(&state, time, depth) else {
        return;
    };
    record(ui, &mut state, false);
    ripple(ui, &mut state, after, before - after);
    drop(state);
    fit(ui, shared);
    refresh(ui, shared);
}

pub fn wire(ui: &App, state: &Shared) {
    let (weak, shared) = (ui.as_weak(), Arc::clone(state));
    ui.on_fit_timeline(move || {
        if let Some(ui) = weak.upgrade() {
            refit(&ui, &mut lock(&shared));
        }
    });

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

    let remove = |all: bool, pull: bool| {
        let (weak, shared) = (ui.as_weak(), Arc::clone(state));
        move || {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let state = lock(&shared);
            let links = chosen(&ui, &state);
            if (links.is_empty() && !all) || state.entries.is_empty() || ui.get_rendering() {
                return;
            }
            drop(state);
            let count = links.len();
            let now = move |ui: &App, shared: &Shared| {
                let mut state = lock(shared);
                record(ui, &mut state, false);
                let gone = state
                    .entries
                    .iter()
                    .filter(|entry| pull && links.contains(&entry.link));
                let mut gone: Vec<(f64, f64)> = gone
                    .map(|entry| (entry.at, entry.at + entry.kept()))
                    .collect();
                gone.sort_by(|a, b| a.0.total_cmp(&b.0));
                let mut spans: Vec<(f64, f64)> = Vec::new();
                for span in gone {
                    match spans.last_mut() {
                        Some(last) if span.0 <= last.1 + 0.001 => last.1 = last.1.max(span.1),
                        _ => spans.push(span),
                    }
                }
                discard(ui, &mut state, |entry| all || links.contains(&entry.link));
                for (start, end) in spans.into_iter().rev() {
                    ripple(ui, &mut state, end, start - end);
                }
                drop(state);
                fit(ui, shared);
                select(ui, shared, -1);
                refresh(ui, shared);
            };
            if !ui.get_prefs().confirm_delete {
                return now(&ui, &shared);
            }
            let text = match (all, count) {
                (true, _) => "Close all clips?".to_owned(),
                (false, 1) => "Remove this clip?".to_owned(),
                (false, count) => format!("Remove {count} clips?"),
            };
            crate::project::ask(&ui, "delete", text, Box::new(now));
        }
    };
    ui.on_remove_clip(remove(false, false));
    ui.on_ripple_clip(remove(false, true));
    ui.on_close_clips(remove(true, false));

    let (weak, shared) = (ui.as_weak(), Arc::clone(state));
    ui.on_range(move |index| {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        let state = lock(&shared);
        let span = selected(&ui).and_then(|place| {
            let (anchor, clicked) = (
                state.entries.get(place)?,
                state.entries.get(index as usize)?,
            );
            let end = |entry: &Entry| entry.at + entry.kept();
            Some((anchor.at.min(clicked.at), end(anchor).max(end(clicked))))
        });
        let Some((from, to)) = span else {
            drop(state);
            return select(&ui, &shared, index);
        };
        let inside =
            |entry: &&Entry| entry.at < to - 0.001 && from + 0.001 < entry.at + entry.kept();
        let links: Vec<u64> = state
            .entries
            .iter()
            .filter(inside)
            .map(|entry| entry.link)
            .collect();
        pick(&ui, &state, |entry| links.contains(&entry.link));
    });

    let (weak, shared) = (ui.as_weak(), Arc::clone(state));
    ui.on_select_track(move |audio, index| {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        let state = lock(&shared);
        let on_track =
            |entry: &&Entry| entry.audio.is_some() == audio && entry.track == index as usize;
        let links: Vec<u64> = state
            .entries
            .iter()
            .filter(on_track)
            .map(|entry| entry.link)
            .collect();
        if links.is_empty() {
            return;
        }
        pick(&ui, &state, |entry| links.contains(&entry.link));
        let first = state
            .entries
            .iter()
            .position(|entry| links.contains(&entry.link));
        drop(state);
        let rows = ui.get_clips();
        let held =
            selected(&ui).is_some_and(|place| rows.row_data(place).is_some_and(|row| row.picked));
        if !held {
            ui.set_selected(first.map_or(-1, |place| place as i32));
            load(&ui, &mut lock(&shared));
        }
    });

    let (weak, shared) = (ui.as_weak(), Arc::clone(state));
    ui.on_jump_clip(move |step| {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        let state = lock(&shared);
        let mut order: Vec<usize> = (0..state.entries.len()).collect();
        order.sort_by(|a, b| {
            let (a, b) = (&state.entries[*a], &state.entries[*b]);
            let kind = (a.audio.is_some(), a.track).cmp(&(b.audio.is_some(), b.track));
            a.at.total_cmp(&b.at).then(kind)
        });
        let mut seen = std::collections::HashSet::new();
        order.retain(|place| seen.insert(state.entries[*place].link));
        let link = selected(&ui).and_then(|index| state.entries.get(index));
        let link = link.map(|entry| entry.link);
        let same = |place: &usize| Some(state.entries[*place].link) == link;
        let now = order.iter().position(same);
        let next = match now {
            Some(now) => now as i32 + step,
            None if step > 0 => 0,
            None => order.len() as i32 - 1,
        };
        let Some(place) = usize::try_from(next).ok().and_then(|next| order.get(next)) else {
            return;
        };
        let place = *place as i32;
        drop(state);
        select(&ui, &shared, place);
    });

    let (weak, shared) = (ui.as_weak(), Arc::clone(state));
    ui.on_move_clips(move |by| {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        let Some(index) = selected(&ui) else {
            return;
        };
        let rows = ui.get_clips();
        let mut state = lock(&shared);
        let Some(entry) = state.entries.get(index) else {
            return;
        };
        let (link, track) = (entry.link, entry.track);
        let mut group = chosen(&ui, &state);
        if !group.contains(&link) {
            group = vec![link];
        }
        let linked = |other: &&Entry| group.contains(&other.link);
        let earliest = state.entries.iter().filter(linked).map(|other| other.at);
        let wanted = f64::from(by).max(-earliest.fold(f64::MAX, f64::min));
        let moved = clear(&state, &group, index, track, wanted);
        if moved.abs() < 0.0005 {
            return;
        }
        record(&ui, &mut state, true);
        for (place, other) in state.entries.iter().enumerate() {
            if let Some(mut row) = rows.row_data(place).filter(|_| group.contains(&other.link)) {
                row.at = (other.at + moved) as f32;
                rows.set_row_data(place, row);
            }
        }
        layout(&ui, &mut state);
        drop(state);
        fit(&ui, &shared);
        refresh(&ui, &shared);
    });

    let (weak, shared) = (ui.as_weak(), Arc::clone(state));
    ui.on_select_all(move || {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        let rows = ui.get_clips();
        for place in 0..rows.row_count() {
            if let Some(mut row) = rows.row_data(place).filter(|row| !row.picked) {
                row.picked = true;
                rows.set_row_data(place, row);
            }
        }
        if selected(&ui).is_none() {
            ui.set_selected(0);
            load(&ui, &mut lock(&shared));
        }
    });

    let (weak, shared) = (ui.as_weak(), Arc::clone(state));
    ui.on_lasso_start(move |add| {
        if let Some(ui) = weak.upgrade() {
            let mut state = lock(&shared);
            state.lasso = if add { chosen(&ui, &state) } else { Vec::new() };
        }
    });

    let (weak, shared) = (ui.as_weak(), Arc::clone(state));
    ui.on_lasso(move |from, to, top, bottom| {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        let state = lock(&shared);
        let bands = bands(&state);
        let (from, to) = (f64::from(from), f64::from(to));
        let hit = |entry: &&Entry| {
            let band = bands
                .iter()
                .find(|band| (band.0, band.1) == (entry.audio.is_some(), entry.track));
            entry.at < to
                && from < entry.at + entry.kept()
                && band.is_some_and(|band| band.2 < bottom && top < band.3)
        };
        let links: Vec<u64> = state
            .entries
            .iter()
            .filter(hit)
            .map(|entry| entry.link)
            .collect();
        pick(&ui, &state, |entry| {
            state.lasso.contains(&entry.link) || links.contains(&entry.link)
        });
    });

    let (weak, shared) = (ui.as_weak(), Arc::clone(state));
    ui.on_lasso_end(move || {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        let rows = ui.get_clips();
        let picked = |place: usize| rows.row_data(place).is_some_and(|row| row.picked);
        if selected(&ui).is_some_and(picked) {
            return;
        }
        let first = (0..rows.row_count()).find(|place| picked(*place));
        let first = first.map_or(-1, |place| place as i32);
        if ui.get_selected() == first {
            return;
        }
        ui.set_selected(first);
        load(&ui, &mut lock(&shared));
    });

    let shared = Arc::clone(state);
    ui.on_gap(move |time, depth| hole(&lock(&shared), f64::from(time), depth).is_some());

    let (weak, shared) = (ui.as_weak(), Arc::clone(state));
    ui.on_close_gap(move |time, depth| {
        if let Some(ui) = weak.upgrade() {
            close_gap(&ui, &shared, f64::from(time), depth);
        }
    });

    let (weak, shared) = (ui.as_weak(), Arc::clone(state));
    ui.on_gap_here(move || {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        let state = lock(&shared);
        let time = state.time(ui.get_position());
        let mut tracks = bands(&state);
        tracks.sort_by(|a, b| a.2.total_cmp(&b.2));
        let found = tracks.iter().map(|band| (band.2 + band.3) / 2.0);
        let depth = found
            .into_iter()
            .find(|depth| hole(&state, time, *depth).is_some());
        drop(state);
        if let Some(depth) = depth {
            close_gap(&ui, &shared, time, depth);
        }
    });

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
        let time = state.time(ui.get_position());
        // a part shorter than this is not worth a clip
        let least = 0.05;
        let under =
            |entry: &Entry| entry.at + least < time && time < entry.at + entry.kept() - least;
        // a split cuts every clip under the playhead, the other marks are for the selected clip
        // a start or an end is for the clip under the playhead when the selected clip is not there
        let held = |index: &usize| kind == "clear" || state.entries.get(*index).is_some_and(under);
        // a copy has no images, so the video under an image gets the mark
        let shown = |index: &&usize| !(ui.get_copying() && state.entries[**index].still);
        let top = |part: interpolini_core::Part| part.clips.iter().rev().find(shown).copied();
        let chosen = selected(&ui)
            .filter(held)
            .or_else(|| top(state.part(ui.get_position())?.1));
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
        // how far the clips can grow back at each end, before the next clip of their track
        let mut room = (f64::MAX, f64::MAX);
        for entry in targets.iter().map(|index| &state.entries[*index]) {
            let (start, end) = (entry.at, entry.at + entry.kept());
            let others = state.entries.iter().filter(|other| {
                let same = other.audio.is_some() == entry.audio.is_some();
                other.link != entry.link && same && other.track == entry.track
            });
            let ends = others.clone().map(|other| other.at + other.kept());
            let before = ends
                .filter(|time| *time <= start + 0.001)
                .fold(0.0, f64::max);
            let starts = others.map(|other| other.at);
            let after = starts
                .filter(|time| *time >= end - 0.001)
                .fold(f64::MAX, f64::min);
            let cut = (f64::from(entry.cut.0), 1.0 - f64::from(entry.cut.1));
            room.0 = room.0.min(start - before).min(cut.0 * entry.length());
            room.1 = room.1.min(after - end).min(cut.1 * entry.length());
        }
        let room = (room.0.max(0.0) as f32, room.1.max(0.0) as f32);
        record(&ui, &mut state, false);
        let first = state.ids + 1;
        for index in targets.into_iter().rev() {
            let Some(mut row) = model.row_data(index) else {
                continue;
            };
            let entry = state.entries[index].clone();
            let length = entry.length() as f32;
            let time = match kind.as_str() {
                "start" | "split" if ui.get_copying() => match snap(&state, &entry, time) {
                    Some(time) => time,
                    None => continue,
                },
                _ => time,
            };
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
                    (row.fade_out, second.fade_in, second.cross) = (0.0, 0.0, 0.0);
                    model.insert(index + 1, second);
                    state.ids += 1;
                    let mut copy = entry;
                    copy.saved.id = first + state.ids + copy.link;
                    copy.saved.link = first + copy.link;
                    state.entries.insert(index + 1, copy);
                }
                "clear" if !entry.still => {
                    row.at -= room.0;
                    row.cut_in -= room.0 / length;
                    row.cut_out += room.1 / length;
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
        let span = lock(&shared).span.max(0.001);
        ui.set_position((time / span).clamp(0.0, 1.0) as f32);
        ui.set_playing(false);
        refresh(&ui, &shared);
    });

    let (weak, shared) = (ui.as_weak(), Arc::clone(state));
    ui.on_grab(move |index, time, alone| {
        let mut state = lock(&shared);
        let Some(ui) = weak.upgrade() else {
            return;
        };
        // with alt the clip leaves the other clips of its file, and moves and cuts on its own
        if let Some(mut row) = ui.get_clips().row_data(index as usize).filter(|_| alone) {
            record(&ui, &mut state, false);
            state.ids += 1;
            row.link = state.ids as i32;
            state.entries[index as usize].link = state.ids;
            state.touched = Some(state.ids);
            ui.get_clips().set_row_data(index as usize, row);
            ui.set_link(state.ids as i32);
        }
        let held = state
            .entries
            .get(index as usize)
            .map(|entry| (entry.id, f64::from(time) - entry.at, entry.at));
        state.held = held;
    });

    let (weak, shared) = (ui.as_weak(), Arc::clone(state));
    ui.on_drag(move |time, depth, slack, cutting| {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        pause(&ui, &shared);
        let rows = ui.get_clips();
        let mut state = lock(&shared);
        let Some((id, offset, _)) = state.held else {
            return;
        };
        let Some(index) = state.entries.iter().position(|entry| entry.id == id) else {
            return;
        };
        state.cutting = cutting && !ui.get_copying();
        let entry = state.entries[index].clone();
        if state.touched.is_none() {
            record(&ui, &mut state, false);
            state.touched = Some(entry.link);
        }
        let mut at = (f64::from(time) - offset).max(0.0);
        // the start or the end of the clip goes onto a near edge: the playhead or another clip
        let playhead = state.time(ui.get_position());
        let others = state
            .entries
            .iter()
            .filter(|other| other.link != entry.link);
        let edges = others.flat_map(|other| [other.at, other.at + other.kept()]);
        let edges = edges.chain([0.0, playhead]);
        let near = edges.flat_map(|edge| [(edge - at, edge), (edge - at - entry.kept(), edge)]);
        let near = near.min_by(|a, b| a.0.abs().total_cmp(&b.0.abs()));
        let near = near.filter(|near| near.0.abs() < f64::from(slack));
        if let Some((near, _)) = near {
            at = (at + near).max(0.0);
        }
        ui.set_snapped(near.map_or(-1.0, |near| near.1 as f32));
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
        let moved = match ui.get_copying() || !cutting {
            true => clear(&state, &group, index, track, moved),
            false => moved,
        };
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
            ui.set_snapped(-1.0);
            let mut state = lock(&shared);
            let cutting = std::mem::take(&mut state.cutting);
            let moved = state.held.take().and_then(|(id, _, was)| {
                let held = state.entries.iter().find(|entry| entry.id == id);
                held.map(|entry| entry.at - was)
            });
            state.spare = None;
            if let Some(link) = state.touched.take() {
                let mut group = chosen(&ui, &state);
                if !group.contains(&link) {
                    group = vec![link];
                }
                // a copy has one file for each clip, so a clip does not cut into another one
                if ui.get_copying() && group.iter().any(|link| clashes(&state, *link)) {
                    drop(state);
                    revert(&ui, &shared);
                    return resume(&ui, &shared);
                }
                for link in group.into_iter().filter(|_| cutting) {
                    overwrite(&ui, &mut state, link, moved.unwrap_or(0.0));
                }
            }
            drop(state);
            fit(&ui, &shared);
            refresh(&ui, &shared);
            resume(&ui, &shared);
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
        let mut ends = HashMap::new();
        let mut moves = HashMap::new();
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
    ui.on_trimmed(move |index, slack, alone, cutting| {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        pause(&ui, &shared);
        // the other clips of the same file get the same cut
        let rows = ui.get_clips();
        let mut state = lock(&shared);
        state.cutting = cutting && !ui.get_copying();
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
        let was = if head {
            entry.at
        } else {
            entry.at + entry.kept()
        };
        let joined = |other: &&Entry| {
            let same = other.audio.is_some() == entry.audio.is_some() && other.track == entry.track;
            let meets = if head {
                other.at + other.kept()
            } else {
                other.at
            };
            other.link != link && same && (meets - was).abs() < 0.001
        };
        let next = state
            .entries
            .iter()
            .find(joined)
            .filter(|_| !alone && !ui.get_copying())
            .cloned();
        let playhead = state.time(ui.get_position());
        let others = state.entries.iter().filter(|other| other.link != link);
        let edges = others.flat_map(|other| [other.at, other.at + other.kept()]);
        let near = edges
            .chain([0.0, playhead])
            .map(|other| (other as f32 - edge, other as f32))
            .min_by(|a, b| a.0.abs().total_cmp(&b.0.abs()));
        let near = near.filter(|near| near.0.abs() < slack);
        ui.set_snapped(near.map_or(-1.0, |near| near.1));
        let moved = near.map_or(0.0, |near| near.0) / row.length;
        let walls = ui.get_copying() || !cutting;
        let mut room = (0.0, f64::MAX);
        let own = |member: &&Entry| member.link == link && walls;
        for member in state.entries.iter().filter(own) {
            let wall = |other: &&Entry| {
                let same = other.audio.is_some() == member.audio.is_some();
                other.link != link && same && other.track == member.track
            };
            for other in state.entries.iter().filter(wall) {
                let rolls = next.as_ref().is_some_and(|next| next.link == other.link);
                match other.at < member.at {
                    true if rolls => room.0 = f64::max(room.0, other.at + 0.1),
                    true => room.0 = f64::max(room.0, other.at + other.kept()),
                    false if rolls => room.1 = f64::min(room.1, other.at + other.kept() - 0.1),
                    false => room.1 = f64::min(room.1, other.at),
                }
            }
        }
        let video = |other: &&Entry| other.link == link && other.audio.is_none();
        let video = state.entries.iter().find(video);
        let key = video
            .filter(|_| head && ui.get_copying())
            .and_then(|video| {
                let speed = f64::from(video.speed);
                let (start, end) = (
                    row.cut_in * video.seconds as f32,
                    row.cut_out * video.seconds as f32,
                );
                let first =
                    ((room.0 - f64::from(row.at)) * speed + f64::from(start)).max(0.0) - 0.001;
                let key = video.key(f64::from(start), first, f64::from(end) - 0.1)?;
                Some((key / video.seconds) as f32 - row.cut_in)
            });
        let moved = key.unwrap_or(moved);
        match head {
            true if row.cut_in + moved >= 0.0 => {
                row.cut_in += moved;
                row.at += moved * row.length;
            }
            false if row.cut_out + moved <= 1.0 => row.cut_out += moved,
            _ => {}
        }
        let wall = room.0 as f32;
        if head && walls && row.at < wall {
            row.cut_in += (wall - row.at) / row.length;
            row.at = wall;
        }
        let most = row.cut_in + ((room.1 - f64::from(row.at)) / f64::from(row.length)) as f32;
        row.cut_out = row.cut_out.min(most);
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
        // the clip that touched this edge follows it as far as its source goes
        if let Some(next) = next {
            let edge = match head {
                true => row.at,
                false => row.at + (row.cut_out - row.cut_in) * row.length,
            };
            let moved = ((f64::from(edge) - was) / next.length()) as f32;
            let least = (0.1 / next.length()) as f32;
            let (from, to) = match head {
                true => (
                    next.cut.0,
                    (next.cut.1 + moved).clamp(next.cut.0 + least, 1.0),
                ),
                false => (
                    (next.cut.0 + moved).clamp(0.0, next.cut.1 - least),
                    next.cut.1,
                ),
            };
            let at = next.at + f64::from(from - next.cut.0) * next.length();
            for (other, entry) in state.entries.iter().enumerate() {
                let Some(mut near) = rows.row_data(other).filter(|_| entry.link == next.link)
                else {
                    continue;
                };
                (near.at, near.cut_in, near.cut_out) = (at as f32, from, to);
                rows.set_row_data(other, near);
            }
        }
        layout(&ui, &mut state);
    });

    let (weak, shared) = (ui.as_weak(), Arc::clone(state));
    ui.on_tune(move |index, name, value, alone| {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        let rows = ui.get_clips();
        let mut state = lock(&shared);
        let index = index as usize;
        let Some(clicked) = state.entries.get(index).cloned() else {
            return;
        };
        record(&ui, &mut state, true);
        for place in aimed(&ui, &state, index, alone) {
            let Some(mut row) = rows.row_data(place) else {
                continue;
            };
            // the gain is for one sound of a file, and for the sounds of the other selected files on its track
            let entry = &state.entries[place];
            let sound = entry.link != clicked.link && entry.audio.is_some();
            match name.as_str() {
                "gain" if place == index || (sound && entry.track == clicked.track) => {
                    row.gain = value
                }
                "gain" => continue,
                "fade in" => row.fade_in = value,
                "fade out" => row.fade_out = value,
                "crossfade" => row.cross = value,
                _ => continue,
            }
            rows.set_row_data(place, row);
        }
        drop(state);
        refresh(&ui, &shared);
    });

    let (weak, shared) = (ui.as_weak(), Arc::clone(state));
    ui.on_retime(move |index, speed| {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        let rows = ui.get_clips();
        let mut state = lock(&shared);
        record(&ui, &mut state, true);
        layout(&ui, &mut state);
        let places = aimed(&ui, &state, index as usize, false).into_iter();
        let mut links: Vec<u64> = places
            .filter(|place| !state.entries[*place].still)
            .map(|place| state.entries[place].link)
            .collect();
        links.sort_unstable();
        links.dedup();
        let first = |state: &State, link: u64| {
            let own = state.entries.iter().filter(|entry| entry.link == link);
            own.map(|entry| entry.at).fold(f64::MAX, f64::min)
        };
        links.sort_by(|a, b| first(&state, *b).total_cmp(&first(&state, *a)));
        for link in links {
            let members: Vec<usize> = (0..state.entries.len())
                .filter(|place| state.entries[*place].link == link)
                .collect();
            let end = |state: &State| {
                let ends = members
                    .iter()
                    .map(|place| state.entries[*place].at + state.entries[*place].kept());
                ends.fold(0.0, f64::max)
            };
            let old = end(&state);
            for place in &members {
                state.entries[*place].speed = speed;
                if let Some(mut row) = rows.row_data(*place) {
                    row.speed = speed;
                    rows.set_row_data(*place, row);
                }
            }
            let new = end(&state);
            ripple(&ui, &mut state, old, new - old);
        }
        drop(state);
        fit(&ui, &shared);
        refresh(&ui, &shared);
    });

    let (weak, shared) = (ui.as_weak(), Arc::clone(state));
    ui.on_curve(move |index, fade, curve, alone| {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        let rows = ui.get_clips();
        let mut state = lock(&shared);
        record(&ui, &mut state, false);
        for place in aimed(&ui, &state, index as usize, alone) {
            if let Some(mut row) = rows.row_data(place) {
                match fade.as_str() {
                    "fade in" => row.ease_in = curve.clone(),
                    "fade out" => row.ease_out = curve.clone(),
                    _ => row.ease_cross = curve.clone(),
                }
                rows.set_row_data(place, row);
            }
        }
        drop(state);
        refresh(&ui, &shared);
    });

    let shared = Arc::clone(state);
    ui.on_joint(move |index, head| {
        let state = lock(&shared);
        let Some(entry) = state.entries.get(index as usize) else {
            return -1;
        };
        let edge = if head {
            entry.at
        } else {
            entry.at + entry.kept()
        };
        let touches = |other: &Entry| {
            let same = other.audio.is_some() == entry.audio.is_some() && other.track == entry.track;
            let meets = if head {
                other.at + other.kept()
            } else {
                other.at
            };
            other.id != entry.id && same && (meets - edge).abs() < 0.001
        };
        let Some(other) = state.entries.iter().position(touches) else {
            return -1;
        };
        if head { index } else { other as i32 }
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
    ui.on_place_key(move || {
        if let Some(ui) = weak.upgrade() {
            record(&ui, &mut lock(&shared), true);
        }
    });

    let (weak, shared) = (ui.as_weak(), Arc::clone(state));
    ui.on_placed(move || {
        if let Some(ui) = weak.upgrade() {
            refresh(&ui, &shared);
        }
    });

    let (weak, shared) = (ui.as_weak(), Arc::clone(state));
    ui.on_trim_done(move || {
        if let Some(ui) = weak.upgrade() {
            ui.set_snapped(-1.0);
            let mut state = lock(&shared);
            let cutting = std::mem::take(&mut state.cutting);
            if let Some(link) = state.touched.take() {
                if ui.get_copying() && clashes(&state, link) {
                    drop(state);
                    revert(&ui, &shared);
                    return resume(&ui, &shared);
                }
                if cutting {
                    overwrite(&ui, &mut state, link, 0.0);
                }
            }
            drop(state);
            fit(&ui, &shared);
            refresh(&ui, &shared);
            resume(&ui, &shared);
        }
    });
}
