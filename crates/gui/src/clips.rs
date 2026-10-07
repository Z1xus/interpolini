use std::path::PathBuf;
use std::sync::Arc;

use interpolini_core::config;
use interpolini_core::{Place, Probe, probe};
use slint::winit_030::{EventResult, WinitWindowAccessor, winit};
use slint::{ComponentHandle, Model, ModelRc, VecModel, Weak};

use crate::edit::overwrite;
use crate::history::record;
use crate::state::{Entry, STILL, Shared, lock};
use crate::view::{fit, lanes, refresh};
use crate::{App, Clip, preview};
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

pub type Probed = (PathBuf, interpolini_core::Result<Probe>);

// the files are opened on another thread, so the window stays live, and then go to the timeline
pub fn import(ui: Weak<App>, state: Shared, clips: Vec<PathBuf>, point: Option<(f32, f32)>) {
    std::thread::spawn(move || {
        let open = |clip: PathBuf| {
            let probed = probe(&clip);
            (clip, probed)
        };
        let clips: Vec<Probed> = clips.into_iter().map(open).collect();
        let _ = ui.upgrade_in_event_loop(move |ui| {
            let time = point.map_or(-1.0, |point| ui.invoke_drop_time(point.0, point.1));
            let place = point
                .filter(|_| time >= 0.0)
                .map(|point| (f64::from(time), ui.invoke_drop_depth(point.1)));
            add(&ui, &state, clips, place);
        });
    });
}

pub fn add(ui: &App, state: &Shared, clips: Vec<Probed>, place: Option<(f64, f32)>) {
    let mut place = place;
    let rows = ui.get_clips();
    let Some(rows) = rows.as_any().downcast_ref::<VecModel<Clip>>() else {
        return;
    };
    for (clip, probed) in clips {
        let probed = match probed {
            Ok(probed) => probed,
            Err(error) => {
                ui.set_failed(true);
                ui.set_status(format!("Couldn't open the file: {error}").into());
                continue;
            }
        };
        // a config in the folder of the clip comes first, then the settings on screen
        let found = config::entries(&clip);
        let local = found
            .into_iter()
            .find(|entry| entry.local && entry.name == "default");
        let mut guard = lock(state);
        record(ui, &mut guard, true);
        let (config, origin) = match local {
            Some(entry) => {
                let loaded = config::read(entry);
                (loaded.config, loaded.entry)
            }
            None => (guard.draft.config.clone(), guard.draft.origin.clone()),
        };
        let name = clip.file_name().unwrap_or_default().to_string_lossy();
        let at = place.map_or(guard.seconds(), |place| place.0);
        // an image starts as 5 seconds from the middle of its source
        let (seconds, cut) = match probed.still {
            true => (STILL, (0.5 - 2.5 / STILL as f32, 0.5 + 2.5 / STILL as f32)),
            false => (probed.seconds, (0.0, 1.0)),
        };
        let pictured = probed.fps > 0.0 || probed.still;
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
            rows.push(Clip {
                name: name.as_ref().into(),
                at: at as f32,
                track: track as i32,
                cut_in: cut.0,
                cut_out: cut.1,
                audio: audio.is_some(),
                link: link as i32,
                still: probed.still,
                ratio: probed.width as f32 / probed.height.max(1) as f32,
                zoom_x: 1.0,
                zoom_y: 1.0,
                thumbs: ModelRc::new(VecModel::from(vec![
                    slint::Image::default();
                    preview::THUMBS
                ])),
                ..Default::default()
            });
            let id = guard.ids;
            guard.entries.push(Entry {
                id,
                link,
                clip: clip.clone(),
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
                repeats: 0.0,
                measured: None,
                size: (probed.width, probed.height),
                place: Place::default(),
            });
        }
        if place.is_some() {
            overwrite(ui, &mut guard, link);
        }
        drop(guard);
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
        if pictured {
            preview::thumbs(ui.as_weak(), clip.clone(), config.clone(), places.clone());
        }
        if !probed.still {
            preview::waves(ui.as_weak(), clip, probed.seconds, places);
        }
    }
    fit(ui, state);
    refresh(ui, state);
}

// counts the repeated frames of each video whose cut changed, in the part that is kept
pub fn measure(ui: &App, state: &Shared) {
    let mut guard = lock(state);
    let video = |entry: &&mut Entry| entry.audio.is_none() && !entry.still;
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
        let threshold = entry.config.dedup.threshold;
        preview::repeats(
            ui.as_weak(),
            entry.clip.clone(),
            threshold,
            part,
            move |ui, repeats| {
                let mut state = lock(&shared);
                if let Some(entry) = state.entries.iter_mut().find(|entry| entry.id == id) {
                    entry.repeats = repeats;
                }
                ui.set_repeats(crate::view::repeats(ui, &state));
            },
        );
    }
}

pub fn pick(
    ui: &Weak<App>,
    name: &str,
    extensions: &'static [&'static str],
    done: impl FnOnce(&App, Vec<PathBuf>) + Send + 'static,
) {
    let dialog = rfd::FileDialog::new().add_filter(name, extensions);
    let ui = ui.clone();
    std::thread::spawn(move || {
        if let Some(files) = dialog.pick_files() {
            let _ = ui.upgrade_in_event_loop(move |ui| done(&ui, files));
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

// the toplevel exists only after the event loop has shown the window, so a timer looks for it
#[cfg(target_os = "linux")]
pub fn drops(ui: &App, state: &Shared) -> slint::Timer {
    let pump = slint::Timer::default();
    let (weak, shared) = (ui.as_weak(), Arc::clone(state));
    let dropped: drop::Dropped = Arc::new(move |clips, point| {
        let point = Some((point.0 as f32, point.1 as f32));
        import(weak.clone(), Arc::clone(&shared), clips, point);
    });
    let (weak, shared) = (ui.as_weak(), Arc::clone(state));
    let hovered: drop::Hovered = Arc::new(move |event| {
        // the files are probed here, off the window thread
        let files = match &event {
            drop::Hover::Files(clips) => Some(clips.iter().filter_map(|clip| probe(clip).ok())),
            _ => None,
        };
        let files: Option<Vec<_>> = files.map(|files| {
            let shape = |probed: Probe| match probed.still {
                true => (5.0, true, 0),
                false => (probed.seconds, probed.fps > 0.0, probed.tracks.len()),
            };
            files.map(shape).collect()
        });
        let shared = Arc::clone(&shared);
        let _ = weak.upgrade_in_event_loop(move |ui| match (event, files) {
            (_, Some(files)) => HOVER.set(files),
            (drop::Hover::At(x, y), _) => hover(&ui, &shared, Some((x as f32, y as f32))),
            _ => {
                HOVER.take();
                hover(&ui, &shared, None);
            }
        });
    });
    let weak = ui.as_weak();
    let mut drops = None;
    let mut iconed = false;
    // a compositor without these protocols must not be asked forever (fuck you gnome)
    let mut tries = 30;
    pump.start(
        slint::TimerMode::Repeated,
        std::time::Duration::from_millis(30),
        move || {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            tries = i32::max(tries - 1, 0);
            if !iconed && tries > 0 {
                iconed = icon::set(ui.window()).is_some();
            }
            match &mut drops {
                Some(drops) => drop::Drops::pump(drops),
                None if tries > 0 => {
                    let hovered = Arc::clone(&hovered);
                    drops = drop::Drops::listen(ui.window(), Arc::clone(&dropped), hovered)
                }
                None => {}
            }
        },
    );
    pump
}
