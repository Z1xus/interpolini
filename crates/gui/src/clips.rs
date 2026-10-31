use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use interpolini_core::config::{self, Config};
use interpolini_core::{Place, Probe, keyframes, probe};
use slint::winit_030::{EventResult, WinitWindowAccessor, winit};
use slint::{ComponentHandle, Model, ModelRc, VecModel, Weak};

use crate::edit::overwrite;
use crate::history::record;
use crate::project::{even, placement};
use crate::state::{Entry, STILL, Saved, Shared, State, lock};
#[cfg(target_os = "linux")]
use crate::view::lanes;
use crate::view::{fit, refit, refresh, update};
use crate::{App, Clip, preview, services};
#[cfg(target_os = "linux")]
use crate::{Ghost, drop, icon};

pub const MEDIA: &[&str] = &[
    "mp4", "mkv", "mov", "webm", "avi", "m4v", "ts", "flv", "mp3", "wav", "flac", "ogg", "opus",
    "m4a", "aac", "png", "jpg", "jpeg", "webp", "bmp",
];

#[cfg(target_os = "linux")]
thread_local! {
    // the files of a drag that is over the window: seconds, has video, audio tracks
    pub static HOVER: std::cell::RefCell<Vec<(f64, bool, usize)>> = const { std::cell::RefCell::new(Vec::new()) };
}

#[cfg(target_os = "linux")]
pub fn hover(ui: &App, state: &Shared, point: Option<(f32, f32)>) {
    let time = point.map_or(-1.0, |point| ui.invoke_drop_time(point.0, point.1));
    let mut ghosts = Vec::new();
    let mut state = lock(state);
    if time < 0.0 {
        state.retreat();
    }
    if let Some((_, y)) = point.filter(|_| time >= 0.0) {
        let pictured = HOVER.with_borrow(|files| files.first().is_some_and(|file| file.1));
        let place = Some((f64::from(time), ui.invoke_drop_depth(y)));
        let (video, alone) = state.aim(place, pictured);
        let mut at = time;
        let ghost = |at: f32, length: f64, track: usize, audio: bool| Ghost {
            at,
            length: length as f32,
            track: track as i32,
            audio,
        };
        HOVER.with_borrow(|files| {
            for (seconds, pictured, tracks) in files {
                if *pictured {
                    ghosts.push(ghost(at, *seconds, video, false));
                }
                for stream in 0..*tracks {
                    let track = alone.filter(|_| !pictured).unwrap_or(stream);
                    ghosts.push(ghost(at, *seconds, track, true));
                }
                at += *seconds as f32;
            }
        });
    }
    lanes(ui, &state);
    ui.set_ghosts(ModelRc::new(VecModel::from(ghosts)));
}

pub struct Found {
    probe: Probe,
    bytes: u64,
    local: Option<(Config, Option<config::Entry>)>,
}

pub type Probed = (PathBuf, interpolini_core::Result<Found>);

fn inspect(clip: &Path) -> interpolini_core::Result<Found> {
    let probe = probe(clip)?;
    let bytes = std::fs::metadata(clip).map_or(0, |file| file.len());
    // a config in the folder of the clip comes first, then the settings on screen
    let local = config::entries(clip)
        .into_iter()
        .find(|entry| entry.local && entry.name == "default");
    let local = local.map(|entry| {
        let loaded = config::read(entry);
        (loaded.config, loaded.entry)
    });
    Ok(Found {
        probe,
        bytes,
        local,
    })
}

pub fn import(ui: Weak<App>, state: Shared, clips: Vec<PathBuf>, point: Option<(f32, f32)>) {
    if let Some(project) = clips.iter().find(|clip| crate::project::is_project(clip)) {
        let project = project.clone();
        let _ = ui.upgrade_in_event_loop(move |ui| crate::project::open_path(&ui, &state, project));
        return;
    }
    services::background(&services::key("import"), move |cancel| {
        let next = Arc::new(Mutex::new(None));
        let last = clips.len();
        for (index, clip) in clips.into_iter().enumerate() {
            if cancel.stopped() {
                return;
            }
            let probed = vec![(clip.clone(), inspect(&clip))];
            let (state, next) = (Arc::clone(&state), Arc::clone(&next));
            let _ = ui.upgrade_in_event_loop(move |ui| {
                let mut next = next.lock().unwrap_or_else(|poison| poison.into_inner());
                let place = next.take().unwrap_or_else(|| {
                    let time = point.map_or(-1.0, |point| ui.invoke_drop_time(point.0, point.1));
                    point
                        .filter(|_| time >= 0.0)
                        .map(|point| (f64::from(time), ui.invoke_drop_depth(point.1)))
                });
                *next = Some(add(&ui, &state, probed, place));
                if index + 1 == last {
                    fill(&ui, &state);
                    refit(&ui, &mut lock(&state));
                }
            });
        }
    });
}

pub fn add(
    ui: &App,
    state: &Shared,
    clips: Vec<Probed>,
    place: Option<(f64, f32)>,
) -> Option<(f64, f32)> {
    // a copy has no timeline, so a new clip does not cut into the clips that are there
    let mut place = place.filter(|_| !ui.get_copying());
    let rows = ui.get_clips();
    let Some(rows) = rows.as_any().downcast_ref::<VecModel<Clip>>() else {
        return place;
    };
    let mut warning = None;
    for (clip, found) in clips {
        let Found {
            probe: probed,
            bytes,
            local,
        } = match found {
            Ok(found) => found,
            Err(error) => {
                ui.set_failed(true);
                let name = clip.file_name().unwrap_or_default().to_string_lossy();
                ui.set_status(format!("Couldn't open {name}").into());
                ui.set_log(format!("{}{}: {error}\n", ui.get_log(), clip.display()).into());
                continue;
            }
        };
        let mut guard = lock(state);
        record(ui, &mut guard, true);
        let (config, origin) = match local {
            Some(local) => {
                local
                    .1
                    .iter()
                    .for_each(|entry| crate::view::stale(&entry.path));
                local
            }
            None => (guard.draft.config.clone(), guard.draft.origin.clone()),
        };
        let at = place.map_or(guard.seconds(), |place| place.0);
        // an image starts as 5 seconds from the middle of its source
        let (seconds, cut) = match probed.still {
            true => (STILL, (0.5 - 2.5 / STILL as f32, 0.5 + 2.5 / STILL as f32)),
            false => (probed.seconds, (0.0, 1.0)),
        };
        let pictured = probed.fps > 0.0 || probed.still;
        let size = (probed.width, probed.height);
        let mut shape = Place::default();
        if pictured && ui.get_prefs().projects {
            let project = &mut guard.project;
            let canvas = *project.look.size.get_or_insert(even(size));
            shape = placement(canvas, size, project.look.fit);
            if canvas != even(size) {
                warning.get_or_insert(format!(
                    "Clip is {}x{}, project is {}x{}",
                    size.0, size.1, canvas.0, canvas.1
                ));
            }
        }
        let (video, alone) = guard.aim(place, pictured);
        guard.spare = None;
        place = place.map(|place| (place.0 + seconds * f64::from(cut.1 - cut.0), place.1));
        guard.ids += 1;
        let link = guard.ids;
        // the video, if the file has one, and then each of its audio tracks
        let kinds = pictured
            .then_some(None)
            .into_iter()
            .chain((0..probed.tracks.len()).map(Some));
        for audio in kinds {
            guard.ids += 1;
            let track = match audio {
                None => video,
                // a sound file goes to the audio track under the pointer
                Some(stream) if probed.fps == 0.0 => alone.unwrap_or(stream),
                Some(stream) => stream,
            };
            if audio.is_some() && guard.tracks.audio.len() <= track {
                guard.tracks.audio.resize(track + 1, (36.0, false));
            }
            let entry = Entry {
                saved: Saved {
                    id: guard.ids,
                    link,
                    audio,
                    streams: probed.tracks.len(),
                    config: config.clone(),
                    origin: origin.clone(),
                    cut,
                    seconds,
                    fps: probed.fps,
                    at,
                    track,
                    still: probed.still,
                    size,
                    place: if audio.is_none() {
                        shape
                    } else {
                        Place::default()
                    },
                    ..Saved::default()
                },
                clip: clip.clone(),
                bytes,
                ..Entry::default()
            };
            rows.push(row(&entry));
            guard.entries.push(entry);
        }
        if place.is_some() {
            overwrite(ui, &mut guard, link, 0.0);
        }
        drop(guard);
    }
    fit(ui, state);
    refresh(ui, state);
    if let Some(text) = warning {
        preview::announce(ui, text, "");
    }
    place
}

pub fn row(entry: &Entry) -> Clip {
    let place = entry.place;
    let image = match entry.missing {
        true => crate::project::missing().image(),
        false => slint::Image::default(),
    };
    Clip {
        name: entry
            .clip
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .as_ref()
            .into(),
        state: if entry.missing { "Missing" } else { "" }.into(),
        at: entry.at as f32,
        track: entry.track as i32,
        cut_in: entry.cut.0,
        cut_out: entry.cut.1,
        speed: entry.speed,
        audio: entry.audio.is_some(),
        link: entry.link as i32,
        still: entry.still,
        ratio: entry.size.0 as f32 / entry.size.1.max(1) as f32,
        x: place.x,
        y: place.y,
        zoom_x: place.scale[0],
        zoom_y: place.scale[1],
        crop_left: place.crop[0],
        crop_top: place.crop[1],
        crop_right: place.crop[2],
        crop_bottom: place.crop[3],
        gain: entry.gain,
        fade_in: entry.fade.0,
        fade_out: entry.fade.1,
        cross: entry.cross,
        ease_in: entry.eases[0].name().into(),
        ease_out: entry.eases[1].name().into(),
        ease_cross: entry.eases[2].name().into(),
        thumbs: ModelRc::new(VecModel::from(vec![image; preview::THUMBS])),
        ..Default::default()
    }
}

pub fn fill(ui: &App, state: &Shared) {
    let rows = ui.get_clips();
    let guard = lock(state);
    let mut links: Vec<u64> = guard.entries.iter().map(|entry| entry.link).collect();
    links.dedup();
    for link in links {
        let own = |entry: &&Entry| entry.link == link;
        let mut entries = guard
            .entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| own(entry));
        let Some((first, any)) = entries.next().filter(|(_, entry)| !entry.missing) else {
            continue;
        };
        let picture = std::iter::once((first, any))
            .chain(entries)
            .find(|(_, entry)| entry.audio.is_none());
        let shared = Arc::clone(state);
        let places = move || {
            let state = lock(&shared);
            let own = state
                .entries
                .iter()
                .enumerate()
                .filter(|(_, entry)| entry.link == link);
            own.map(|(index, entry)| (entry.audio, index))
                .collect::<Vec<_>>()
        };
        let bare = |image: Option<slint::Image>| image.is_none_or(|image| image.size().width == 0);
        if let Some((_, picture)) = picture
            && !picture.still
            && picture.keys.is_empty()
        {
            keys(ui.as_weak(), Arc::clone(state), picture.clip.clone());
        }
        let sounds = guard.entries.iter().enumerate();
        let mut sounds = sounds.filter(|(_, entry)| own(entry) && entry.audio.is_some());
        let silent = sounds.any(|(index, _)| {
            rows.row_data(index)
                .is_some_and(|row| row.wave.size().width == 0)
        });
        if silent && !any.still {
            let key = format!("waves {link}");
            let places = places.clone();
            preview::waves(ui.as_weak(), &key, any.clip.clone(), any.seconds, places);
        }
        if let Some((index, picture)) = picture {
            let missing = rows
                .row_data(index)
                .is_some_and(|row| bare(row.thumbs.row_data(0)));
            if missing {
                let key = format!("thumbs {link}");
                preview::thumbs(ui.as_weak(), &key, picture.clip.clone(), places);
            }
        }
    }
}

// the keyframes for each zoom, a level keeps those that are 0.025 * 2^level seconds apart
fn levels(keys: &[f64]) -> ModelRc<ModelRc<f32>> {
    let mut levels = Vec::new();
    let mut gap = 0.025;
    loop {
        let mut last = f64::MIN;
        let mut kept = Vec::new();
        for key in keys {
            if key - last >= gap {
                last = *key;
                kept.push(*key as f32);
            }
        }
        let done = kept.len() < 2;
        levels.push(ModelRc::new(VecModel::from(kept)));
        if done {
            return ModelRc::new(VecModel::from(levels));
        }
        gap *= 2.0;
    }
}

fn keys(ui: Weak<App>, state: Shared, clip: PathBuf) {
    let key = format!("keys {}", clip.display());
    services::background(&key, move |cancel| {
        let Ok(keys) = keyframes(&clip, cancel) else {
            return;
        };
        let _ = ui.upgrade_in_event_loop(move |ui| {
            let marks = levels(&keys);
            let keys: Arc<[f64]> = keys.into();
            let mut guard = lock(&state);
            for (index, entry) in guard.entries.iter_mut().enumerate() {
                if entry.clip == clip && entry.audio.is_none() {
                    entry.keys = Arc::clone(&keys);
                    update(&ui, index, |row| row.keys = marks.clone());
                }
            }
            drop(guard);
            refresh(&ui, &state);
        });
    });
}

pub fn forget(state: &State, gone: &Entry) {
    services::with(|services| {
        services.differences.cancel(&format!("diff {}", gone.id));
        let pool = &services.background;
        if !state
            .entries
            .iter()
            .any(|other| other.link == gone.link && other.clip == gone.clip)
        {
            pool.cancel(&format!("thumbs {}", gone.link));
            pool.cancel(&format!("waves {}", gone.link));
        }
        if !state.entries.iter().any(|other| other.clip == gone.clip) {
            pool.cancel(&format!("keys {}", gone.clip.display()));
        }
    });
}

// measures the repeats of each video whose cut changed, in the part that is kept
pub fn measure(ui: &App, state: &Shared) {
    let mut guard = lock(state);
    let video = |entry: &&mut Entry| entry.audio.is_none() && !entry.still && !entry.missing;
    for entry in guard.entries.iter_mut().filter(video) {
        if entry.measured == Some(entry.cut) {
            continue;
        }
        entry.measured = Some(entry.cut);
        let (shared, id) = (Arc::clone(state), entry.id);
        let part = (
            f64::from(entry.cut.0) * entry.seconds,
            f64::from(entry.cut.1) * entry.seconds,
        );
        let key = format!("diff {id}");
        services::with(|services| services.differences.cancel(&key));
        preview::differences(
            ui.as_weak(),
            &key,
            entry.clip.clone(),
            part,
            move |ui, differences| {
                let mut state = lock(&shared);
                if let Some(entry) = state.entries.iter_mut().find(|entry| entry.id == id) {
                    entry.differences = differences.into();
                }
                drop(state);
                crate::view::refresh(ui, &shared);
            },
        );
    }
}

pub fn pick(
    ui: &Weak<App>,
    name: &str,
    extensions: &'static [&'static str],
    done: impl FnOnce(&App, Vec<PathBuf>) + 'static,
) {
    let dialog = rfd::AsyncFileDialog::new().add_filter(name, extensions);
    let ui = ui.clone();
    let _ = slint::spawn_local(async move {
        let files = dialog.pick_files().await;
        if let (Some(files), Some(ui)) = (files, ui.upgrade()) {
            done(
                &ui,
                files.iter().map(|file| file.path().to_owned()).collect(),
            );
        }
    });
}

pub fn wire(ui: &App, state: &Shared) {
    let (weak, shared) = (ui.as_weak(), Arc::clone(state));
    ui.on_add_clips(move || {
        let shared = Arc::clone(&shared);
        pick(&weak, "Media", MEDIA, move |ui, clips| {
            import(ui.as_weak(), shared, clips, None)
        });
    });

    let (weak, shared) = (ui.as_weak(), Arc::clone(state));
    ui.window().on_winit_window_event(move |_, event| {
        use winit::event::{ElementState, WindowEvent};
        match (event, weak.upgrade()) {
            (WindowEvent::DroppedFile(clip), Some(ui)) => {
                import(ui.as_weak(), Arc::clone(&shared), vec![clip.clone()], None)
            }
            // a click anywhere takes the keyboard away from a text field
            (
                WindowEvent::MouseInput {
                    state: ElementState::Pressed,
                    ..
                },
                Some(ui),
            ) => ui.invoke_blur(),
            _ => {}
        }
        EventResult::Propagate
    });
}

#[cfg(target_os = "linux")]
thread_local! {
    static LOOK: slint::Timer = slint::Timer::default();
}

// the toplevel exists only after the window shows, so a timer looks for it
#[cfg(target_os = "linux")]
pub fn drops(ui: &App, state: &Shared) {
    const EVERY: std::time::Duration = std::time::Duration::from_millis(30);
    let (weak, shared) = (ui.as_weak(), Arc::clone(state));
    let dropped: drop::Dropped = Arc::new(move |clips, point| {
        let point = Some((point.0 as f32, point.1 as f32));
        import(weak.clone(), Arc::clone(&shared), clips, point);
    });
    let (weak, shared) = (ui.as_weak(), Arc::clone(state));
    let hovered: drop::Hovered = Arc::new(move |event| {
        let (weak, shared) = (weak.clone(), Arc::clone(&shared));
        if let drop::Hover::Files(clips) = event {
            // the files are probed here, off the window thread
            services::background("hover", move |cancel| {
                let files = clips.iter().filter_map(|clip| probe(clip).ok());
                let shape = |probed: Probe| match probed.still {
                    true => (5.0, true, 0),
                    false => (probed.seconds, probed.fps > 0.0, probed.tracks.len()),
                };
                let files: Vec<_> = files.map(shape).collect();
                let cancel = cancel.clone();
                let _ = weak.upgrade_in_event_loop(move |_| {
                    if !cancel.stopped() {
                        HOVER.set(files);
                    }
                });
            });
            return;
        }
        if matches!(event, drop::Hover::Gone) {
            services::with(|services| services.background.cancel("hover"));
        }
        let _ = weak.upgrade_in_event_loop(move |ui| match event {
            drop::Hover::At(x, y) => hover(&ui, &shared, Some((x as f32, y as f32))),
            _ => {
                HOVER.take();
                hover(&ui, &shared, None);
            }
        });
    });
    let weak = ui.as_weak();
    let mut iconed = false;
    let mut listening = false;
    // a compositor without these protocols must not be asked forever (fuck you gnome)
    let mut tries = 30;
    LOOK.with(|look| {
        look.start(slint::TimerMode::Repeated, EVERY, move || {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            tries -= 1;
            iconed = iconed || icon::set(ui.window()).is_some();
            if !listening {
                let hovered = Arc::clone(&hovered);
                let drops = drop::Drops::listen(ui.window(), Arc::clone(&dropped), hovered);
                listening = drops.is_some();
                services::with(|services| services.drops = drops);
            }
            if tries == 0 || (iconed && listening) {
                LOOK.with(slint::Timer::stop);
            }
        });
    });
}
