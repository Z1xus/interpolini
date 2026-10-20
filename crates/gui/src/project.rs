use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, MutexGuard, OnceLock};
use std::time::Duration;

use interpolini_core::config::{self, Config};
use interpolini_core::{Place, Probe, Setup, probe};
use serde::{Deserialize, Serialize};
use slint::{ComponentHandle, Model, ModelRc, VecModel};

use crate::clips::{fill, row};
use crate::file::{self, Document, Media};
use crate::history::{self, record};
use crate::settings::save_preferences;
use crate::state::{Entry, Shared, State, Tracks, lock};
use crate::view::{fit, layout, refresh, select};
use crate::{App, ProjectSettings, Recent, Theme, themes};

const RECENT: usize = 8;
const EXTENSION: &str = "lini";

#[derive(Clone, Copy, Default, PartialEq, Debug, Serialize, Deserialize)]
pub enum Fit {
    #[serde(rename = "fill")]
    Fill,
    #[serde(rename = "original")]
    Original,
    #[default]
    #[serde(rename = "fit", other)]
    Inside,
}

impl Fit {
    const ALL: [(Self, &'static str); 3] = [
        (Self::Inside, "fit"),
        (Self::Fill, "fill"),
        (Self::Original, "original"),
    ];

    fn name(self) -> &'static str {
        Self::ALL
            .iter()
            .find(|fit| fit.0 == self)
            .map_or("fit", |fit| fit.1)
    }

    fn parse(text: &str) -> Self {
        Self::ALL
            .iter()
            .find(|fit| fit.1 == text)
            .map_or(Self::Inside, |fit| fit.0)
    }
}

#[derive(Clone, Default, PartialEq, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Look {
    pub size: Option<(u32, u32)>,
    pub chosen: bool,
    pub fps: Option<f64>,
    pub background: [u8; 3],
    pub fit: Fit,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct View {
    pub position: f32,
    pub selected: i32,
    pub scope: String,
    pub view: String,
    pub copying: bool,
}

impl Default for View {
    fn default() -> Self {
        Self {
            position: 0.0,
            selected: -1,
            scope: String::new(),
            view: String::new(),
            copying: false,
        }
    }
}

#[derive(Default)]
pub struct Project {
    pub path: Option<PathBuf>,
    pub look: Look,
    pub dirty: bool,
    pub refs: HashMap<PathBuf, Media>,
}

impl Project {
    pub fn name(&self) -> String {
        let stem = self.path.as_ref().and_then(|path| path.file_stem());
        stem.map_or("Untitled".into(), |stem| {
            stem.to_string_lossy().into_owned()
        })
    }

    pub fn setup(&self) -> Setup {
        Setup {
            size: self.look.size,
            fps: self.look.fps,
            background: self.look.background,
        }
    }

    pub fn auto(&self, size: (u32, u32)) -> Option<Place> {
        Some(placement(self.look.size?, size, self.look.fit))
    }
}

const SIDES: std::ops::RangeInclusive<u32> = 16..=16384;
const RATES: std::ops::RangeInclusive<f64> = 1.0..=1000.0;

pub fn even(size: (u32, u32)) -> (u32, u32) {
    ((size.0 & !1).max(2), (size.1 & !1).max(2))
}

pub fn placement(canvas: (u32, u32), size: (u32, u32), fit: Fit) -> Place {
    let (wide, high) = (f64::from(canvas.0), f64::from(canvas.1));
    let (width, height) = (f64::from(size.0.max(1)), f64::from(size.1.max(1)));
    let inside = (wide / width).min(high / height);
    let scale = match fit {
        _ if size.0 <= canvas.0 && size.1 <= canvas.1 => 1.0 / inside,
        Fit::Original => 1.0 / inside,
        Fit::Fill => (wide / width).max(high / height) / inside,
        Fit::Inside => 1.0,
    };
    Place {
        scale: [scale as f32; 2],
        ..Place::default()
    }
}

pub struct Missing {
    rgba: Vec<u8>,
    size: (u32, u32),
}

impl Missing {
    pub fn rgba(&self) -> &[u8] {
        &self.rgba
    }

    pub fn size(&self) -> (u32, u32) {
        self.size
    }

    pub fn image(&self) -> slint::Image {
        let pixels = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(
            &self.rgba,
            self.size.0,
            self.size.1,
        );
        slint::Image::from_rgba8(pixels)
    }
}

pub fn missing() -> &'static Missing {
    static MISSING: OnceLock<Missing> = OnceLock::new();
    MISSING.get_or_init(|| {
        let bytes = include_bytes!(concat!(env!("OUT_DIR"), "/art/media/missing.jpg"));
        let image = image::load_from_memory(bytes).map(|image| image.to_rgba8());
        let image = image.unwrap_or_else(|_| image::RgbaImage::new(2, 2));
        Missing {
            size: image.dimensions(),
            rgba: image.into_raw(),
        }
    })
}

#[derive(Default)]
pub struct Session {
    pub recent: Vec<PathBuf>,
    pub recovery: Option<PathBuf>,
}

type Then = Box<dyn FnOnce(&App, &Shared)>;

thread_local! {
    pub static SESSION: RefCell<Session> = RefCell::default();
    static THEN: RefCell<Option<Then>> = const { RefCell::new(None) };
    static SAVER: slint::Timer = slint::Timer::default();
    static WINDOW: RefCell<Option<ProjectSettings>> = const { RefCell::new(None) };
}

pub fn touch(ui: &App, state: &mut State) {
    if !state.project.dirty {
        state.project.dirty = true;
        title(ui, state);
    }
}

fn title(ui: &App, state: &State) {
    ui.set_modified(state.project.dirty);
    ui.set_project_name(state.project.name().into());
}

pub fn asks(ui: &App) -> bool {
    let prefs = ui.get_prefs();
    prefs.projects && prefs.ask_save && ui.get_modified()
}

fn status(ui: &App, text: String) {
    ui.set_failed(true);
    ui.set_status(text.into());
}

fn remember(ui: &App, path: &Path) {
    SESSION.with_borrow_mut(|session| {
        session.recent.retain(|other| other != path);
        session.recent.insert(0, path.to_owned());
        session.recent.truncate(RECENT);
    });
    recents(ui);
}

fn forget(ui: &App, path: &Path) {
    SESSION.with_borrow_mut(|session| session.recent.retain(|other| other != path));
    recents(ui);
}

pub fn recents(ui: &App) {
    let paths = SESSION.with_borrow(|session| session.recent.clone());
    let recent = |path: PathBuf| {
        let name = path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let folder = path.parent().and_then(Path::file_name).unwrap_or_default();
        Recent {
            name: name.into(),
            folder: folder.to_string_lossy().as_ref().into(),
            path: path.to_string_lossy().as_ref().into(),
        }
    };
    ui.set_recent(ModelRc::new(VecModel::from_iter(
        paths.into_iter().map(recent),
    )));
    save_preferences(ui);
}

fn recovery(project: &Project) -> PathBuf {
    match &project.path {
        Some(path) => {
            let mut name = path.as_os_str().to_owned();
            name.push(".rec");
            name.into()
        }
        None => config::app_dir().join("untitled.lini.rec"),
    }
}

pub fn is_project(path: &Path) -> bool {
    let kind = path.extension().and_then(|kind| kind.to_str());
    kind.is_some_and(|kind| kind.eq_ignore_ascii_case(EXTENSION))
}

pub fn clear(ui: &App, shared: &Shared) {
    let mut state = lock(shared);
    clear_recovery(ui, &state.project);
    state.project.dirty = false;
    title(ui, &state);
}

pub fn finish(ui: &App, shared: &Shared) {
    match ui.get_modified() {
        true => autosave(ui, shared),
        false => clear_recovery(ui, &lock(shared).project),
    }
}

fn discard(ui: &App) {
    let file = SESSION.with_borrow_mut(|session| session.recovery.take());
    file.into_iter()
        .for_each(|file| drop(std::fs::remove_file(file)));
    save_preferences(ui);
}

fn clear_recovery(ui: &App, project: &Project) {
    let _ = std::fs::remove_file(recovery(project));
    discard(ui);
}

fn exists(origin: &mut Option<config::Entry>) {
    *origin = origin.take().filter(|origin| origin.path.exists());
}

fn medium(project: &mut Project, entry: &Entry, target: Option<&Path>) -> Media {
    let known = project.refs.get(&entry.clip);
    let now = std::fs::metadata(&entry.clip).ok();
    let mut media = match (now, known) {
        (Some(now), Some(known))
            if known.bytes == now.len() && known.mtime == file::modified(&now) =>
        {
            known.clone()
        }
        (Some(_), _) => Media::read(&entry.clip, target)
            .unwrap_or_else(|_| known.cloned().unwrap_or_else(|| blank(entry))),
        (None, Some(known)) => known.clone(),
        (None, None) => blank(entry),
    };
    media.absolute = entry.clip.to_string_lossy().into_owned();
    match target {
        Some(target) => media.rebase(target),
        None => media.relative.clear(),
    }
    project.refs.insert(entry.clip.clone(), media.clone());
    media
}

fn blank(entry: &Entry) -> Media {
    Media {
        absolute: entry.clip.to_string_lossy().into_owned(),
        relative: String::new(),
        bytes: entry.bytes,
        mtime: 0,
        hash: 0,
    }
}

fn snapshot(ui: &App, state: &mut State, target: Option<&Path>) -> Document {
    layout(ui, state);
    let mut media: Vec<Media> = Vec::new();
    let mut places: HashMap<PathBuf, u32> = HashMap::new();
    let mut clips = Vec::new();
    let mut configs: Vec<Config> = Vec::new();
    let mut known = |config: &Config| {
        let at = configs.iter().position(|other| other == config);
        at.unwrap_or_else(|| {
            configs.push(config.clone());
            configs.len() - 1
        }) as u32
    };
    let entries = std::mem::take(&mut state.entries);
    for entry in &entries {
        let at = *places.entry(entry.clip.clone()).or_insert_with(|| {
            media.push(medium(&mut state.project, entry, target));
            media.len() as u32 - 1
        });
        clips.push(file::Clip {
            media: at,
            config: known(&entry.config),
            saved: entry.saved.clone(),
        });
    }
    state.entries = entries;
    let draft_config = known(&state.draft.config);
    Document {
        app: env!("CARGO_PKG_VERSION").into(),
        path: target.map_or(String::new(), |path| path.to_string_lossy().into_owned()),
        look: state.project.look.clone(),
        draft: state.draft.saved.clone(),
        draft_config,
        configs,
        media,
        clips,
        tracks: state.tracks.clone(),
        view: View {
            position: ui.get_position(),
            selected: ui.get_selected(),
            scope: ui.get_scope().into(),
            view: ui.get_view().into(),
            copying: ui.get_copying(),
        },
    }
}

fn reason(error: &std::io::Error, what: &str) -> String {
    match error.kind() {
        std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::ReadOnlyFilesystem => match what
        {
            "folder" => "the folder is read only".into(),
            _ => "you have no access to it".into(),
        },
        std::io::ErrorKind::NotFound => format!("the {what} is gone"),
        _ => error.to_string(),
    }
}

pub fn save(ui: &App, shared: &Shared, then: Option<Then>) {
    let path = lock(shared).project.path.clone();
    match path {
        Some(path) => store(ui, shared, path, then),
        None => choose(ui, shared, then),
    }
}

fn store(ui: &App, shared: &Shared, path: PathBuf, then: Option<Then>) {
    let document = snapshot(ui, &mut lock(shared), Some(&path));
    let written = file::encode(&document).map_err(|error| (error, false));
    let written = written.and_then(|bytes| {
        file::write(&path, &bytes).map_err(|error| {
            let kinds = [
                std::io::ErrorKind::PermissionDenied,
                std::io::ErrorKind::ReadOnlyFilesystem,
            ];
            (reason(&error, "folder"), kinds.contains(&error.kind()))
        })
    });
    if let Err((error, elsewhere)) = written {
        status(ui, format!("Couldn't save the project, {error}"));
        if elsewhere {
            choose(ui, shared, then);
        }
        return;
    }
    let mut state = lock(shared);
    clear_recovery(ui, &state.project);
    state.project.path = Some(path.clone());
    state.project.dirty = false;
    title(ui, &state);
    drop(state);
    ui.set_failed(false);
    remember(ui, &path);
    crate::settings::report(ui, Ok(()), "Saved");
    if let Some(then) = then {
        then(ui, shared);
    }
}

fn dialog() -> rfd::AsyncFileDialog {
    rfd::AsyncFileDialog::new().add_filter("interpolini project", &[EXTENSION])
}

pub fn choose(ui: &App, shared: &Shared, then: Option<Then>) {
    let state = lock(shared);
    let mut dialog = dialog().set_file_name(format!("{}.{EXTENSION}", state.project.name()));
    let folder = state.project.path.as_ref().and_then(|path| path.parent());
    let first = state.entries.first().and_then(|entry| entry.clip.parent());
    if let Some(folder) = folder.or(first).filter(|folder| folder.is_dir()) {
        dialog = dialog.set_directory(folder);
    }
    drop(state);
    let (weak, shared) = (ui.as_weak(), Arc::clone(shared));
    let _ = slint::spawn_local(async move {
        let Some(file) = dialog.save_file().await else {
            return;
        };
        let mut path = file.path().to_owned();
        if path.extension().is_none_or(|kind| kind != EXTENSION) {
            let mut name = path.into_os_string();
            name.push(format!(".{EXTENSION}"));
            path = name.into();
        }
        if let Some(ui) = weak.upgrade() {
            store(&ui, &shared, path, then);
        }
    });
}

fn guard(ui: &App, shared: &Shared, then: Then) {
    if ui.get_rendering() {
        return status(ui, "Stop the render first".into());
    }
    if !ui.get_modified() || !ui.get_prefs().projects {
        return then(ui, shared);
    }
    THEN.set(Some(then));
    question(ui, "switch");
}

pub fn question(ui: &App, kind: &str) {
    if matches!(kind, "project" | "switch") {
        let name = ui.get_project_name();
        ui.set_ask(format!("Save changes to \"{name}\"?").into());
    }
    ui.set_closing(kind.into());
}

pub fn ask(ui: &App, kind: &str, text: String, then: Then) {
    THEN.set(Some(then));
    ui.set_ask(text.into());
    ui.set_closing(kind.into());
}

pub fn answer(ui: &App, shared: &Shared, yes: bool) {
    let kind = ui.get_closing();
    ui.set_closing("".into());
    let then = THEN.take();
    let run = |then: Option<Then>| then.into_iter().for_each(|then| then(ui, shared));
    match (kind.as_str(), yes) {
        ("switch", true) => save(ui, shared, then),
        ("switch", false) => {
            clear(ui, shared);
            run(then);
        }
        ("recover", false) => discard(ui),
        ("recover" | "delete", true) => run(then),
        _ => {}
    }
}

fn reset(ui: &App, shared: &Shared) {
    let mut state = lock(shared);
    let gone = std::mem::take(&mut state.entries);
    for entry in &gone {
        crate::clips::forget(&state, entry);
    }
    state.tracks = Tracks::default();
    state.held = None;
    state.touched = None;
    state.spare = None;
    state.project = Project::default();
    let preset = config::load(Path::new(""), true, &ui.get_prefs().preset);
    state.draft.config = preset.config;
    state.draft.origin = preset.entry;
    title(ui, &state);
    drop(state);
    history::clear();
    ui.set_playing(false);
    ui.set_clips(ModelRc::new(VecModel::<crate::Clip>::default()));
    ui.set_position(0.0);
    crate::preview::announce(ui, String::new(), "");
}

pub fn new(ui: &App, shared: &Shared) {
    guard(
        ui,
        shared,
        Box::new(|ui, shared| {
            reset(ui, shared);
            fit(ui, shared);
            select(ui, shared, -1);
            refresh(ui, shared);
        }),
    );
}

pub fn open_path(ui: &App, shared: &Shared, path: PathBuf) {
    guard(
        ui,
        shared,
        Box::new(move |ui, shared| open(ui, shared, &path)),
    );
}

pub fn browse(ui: &App, shared: &Shared) {
    let (weak, shared) = (ui.as_weak(), Arc::clone(shared));
    let dialog = dialog();
    let _ = slint::spawn_local(async move {
        if let (Some(file), Some(ui)) = (dialog.pick_file().await, weak.upgrade()) {
            open_path(&ui, &shared, file.path().to_owned());
        }
    });
}

fn adopt(entry: &mut Entry, probe: &Probe) {
    let kept = (
        f64::from(entry.cut.0) * entry.seconds,
        f64::from(entry.cut.1) * entry.seconds,
    );
    if !entry.still {
        entry.seconds = probe.seconds;
        let fraction = |time: f64| (time / probe.seconds.max(0.001)).clamp(0.0, 1.0) as f32;
        entry.cut = (fraction(kept.0), fraction(kept.1));
    }
    entry.fps = probe.fps;
    entry.size = (probe.width, probe.height);
    entry.streams = probe.tracks.len();
}

enum Link {
    Same(PathBuf),
    Changed(PathBuf, Probe),
    Gone,
}

fn restore(ui: &App, shared: &Shared, document: Document, path: Option<PathBuf>, dirty: bool) {
    let Document {
        app: _,
        path: old,
        look,
        mut draft,
        draft_config,
        configs,
        media,
        clips,
        tracks,
        view,
    } = document;
    let look = Look {
        size: look
            .size
            .filter(|size| SIDES.contains(&size.0) && SIDES.contains(&size.1)),
        fps: look.fps.filter(|fps| RATES.contains(fps)),
        ..look
    };
    let (canvas, kind) = (look.size, look.fit);
    let clips = clips
        .into_iter()
        .filter(|clip| (clip.media as usize) < media.len());
    reset(ui, shared);
    let base = path.clone().unwrap_or_else(|| PathBuf::from(&old));
    let link = |medium: &Media| match file::find(medium, &base, &old) {
        None => Link::Gone,
        Some((found, false)) => Link::Same(found),
        Some((found, true)) => match probe(&found) {
            Ok(probed) => Link::Changed(found, probed),
            Err(_) => Link::Gone,
        },
    };
    let links: Vec<Link> = media.iter().map(link).collect();
    let place = |index: usize| match &links[index] {
        Link::Same(found) | Link::Changed(found, _) => found.clone(),
        Link::Gone => PathBuf::from(&media[index].absolute),
    };
    let mut refs = HashMap::new();
    for (index, medium) in media.iter().enumerate() {
        let fresh = match links[index] {
            Link::Gone => None,
            _ => Media::read(&place(index), path.as_deref()).ok(),
        };
        refs.insert(place(index), fresh.unwrap_or_else(|| medium.clone()));
    }
    let gone = links
        .iter()
        .filter(|link| matches!(link, Link::Gone))
        .count();
    let changes: Vec<String> = links
        .iter()
        .filter_map(|link| match link {
            Link::Changed(found, _) => Some(found.file_name()?.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect();
    let mut state = lock(shared);
    for file::Clip {
        media: index,
        config,
        mut saved,
    } in clips
    {
        let index = index as usize;
        let found = place(index);
        exists(&mut saved.origin);
        saved.config = configs.get(config as usize).cloned().unwrap_or_default();
        let mut entry = Entry {
            bytes: refs[&found].bytes,
            clip: found,
            missing: matches!(links[index], Link::Gone),
            saved,
            ..Entry::default()
        };
        if let Link::Changed(_, probed) = &links[index] {
            let was = entry.size;
            adopt(&mut entry, probed);
            rescale(&mut entry, was, canvas, kind);
        }
        state.ids = state.ids.max(entry.id).max(entry.link);
        state.entries.push(entry);
    }
    exists(&mut draft.origin);
    draft.config = configs
        .get(draft_config as usize)
        .cloned()
        .unwrap_or_default();
    state.draft.saved = draft;
    state.tracks = Tracks {
        video: if tracks.video.is_empty() {
            Tracks::default().video
        } else {
            tracks.video
        },
        audio: if tracks.audio.is_empty() {
            Tracks::default().audio
        } else {
            tracks.audio
        },
    };
    state.project = Project {
        path,
        look,
        dirty,
        refs,
    };
    title(ui, &state);
    let rows: Vec<crate::Clip> = state.entries.iter().map(row).collect();
    drop(state);
    ui.set_clips(ModelRc::new(VecModel::from(rows)));
    ui.set_scope(view.scope.into());
    ui.set_view(view.view.into());
    ui.set_copying(view.copying);
    fit(ui, shared);
    ui.set_position(view.position.clamp(0.0, 1.0));
    fill(ui, shared);
    select(ui, shared, view.selected);
    refresh(ui, shared);
    let text = match (gone, changes.len()) {
        (0, 0) => String::new(),
        (1, _) => "1 file is missing".to_owned(),
        (1.., _) => format!("{gone} files are missing"),
        (_, 1) => format!("{} changed", changes[0]),
        (_, count) => {
            format!("{count} files changed")
        }
    };
    if !text.is_empty() {
        crate::preview::announce(ui, text, if gone > 0 { "Locate" } else { "" });
    }
}

pub fn open(ui: &App, shared: &Shared, path: &Path) {
    let path = std::path::absolute(path).unwrap_or_else(|_| path.to_owned());
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) => {
            if error.kind() == std::io::ErrorKind::NotFound {
                forget(ui, &path);
            }
            return status(
                ui,
                format!("Couldn't open {name}, {}", reason(&error, "file")),
            );
        }
    };
    match file::decode(&bytes) {
        Ok(document) => {
            ui.set_failed(false);
            ui.set_status("".into());
            restore(ui, shared, document, Some(path.clone()), false);
            remember(ui, &path);
        }
        Err(error) => status(ui, format!("Couldn't open {name}, {error}")),
    }
}

pub fn recover(ui: &App) -> bool {
    let Some(file) = SESSION.with_borrow(|session| session.recovery.clone()) else {
        return false;
    };
    let read = |file: &Path| {
        std::fs::read(file)
            .ok()
            .and_then(|bytes| file::decode(&bytes).ok())
    };
    let Some(document) = read(&file) else {
        discard(ui);
        return false;
    };
    let original = (!document.path.is_empty()).then(|| PathBuf::from(&document.path));
    let name = original.as_ref().map_or("Untitled".into(), |path| {
        path.file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned()
    });
    let then: Then = Box::new(move |ui, shared| {
        if let Some(document) = read(&file) {
            restore(ui, shared, document, original, true);
        }
    });
    ask(
        ui,
        "recover",
        format!("Recover unsaved changes to \"{name}\"?"),
        then,
    );
    true
}

fn autosave(ui: &App, shared: &Shared) {
    let prefs = ui.get_prefs();
    if !prefs.projects || !ui.get_modified() || ui.get_rendering() {
        return;
    }
    let mut state = lock(shared);
    let file = recovery(&state.project);
    let path = state.project.path.clone();
    let document = snapshot(ui, &mut state, path.as_deref());
    drop(state);
    let Ok(bytes) = file::encode(&document) else {
        return;
    };
    if file::write(&file, &bytes).is_ok() {
        SESSION.with_borrow_mut(|session| session.recovery = Some(file));
        save_preferences(ui);
    }
}

pub fn schedule(ui: &App, shared: &Shared) {
    let prefs = ui.get_prefs();
    let (weak, shared) = (ui.as_weak(), Arc::clone(shared));
    SAVER.with(|saver| {
        saver.stop();
        if prefs.projects && prefs.autosave > 0 {
            let every = Duration::from_secs(60 * prefs.autosave as u64);
            saver.start(slint::TimerMode::Repeated, every, move || {
                if let Some(ui) = weak.upgrade() {
                    autosave(&ui, &shared);
                }
            });
        }
    });
}

fn hex(color: [u8; 3]) -> String {
    format!("#{:02x}{:02x}{:02x}", color[0], color[1], color[2])
}

fn color(text: &str) -> Option<[u8; 3]> {
    let digits = text.trim().trim_start_matches('#');
    if digits.len() != 6 {
        return None;
    }
    let value = u32::from_str_radix(digits, 16).ok()?;
    Some([(value >> 16) as u8, (value >> 8) as u8, value as u8])
}

fn show_size(window: &ProjectSettings, project: &Project) {
    let size = project
        .look
        .size
        .map(|size| (size.0.to_string(), size.1.to_string()));
    let (wide, high) = size.unwrap_or_default();
    let choice = match project.look.size {
        None => "Auto",
        Some((1280, 720)) => "720p",
        Some((1920, 1080)) => "1080p",
        Some((2560, 1440)) => "1440p",
        Some((3840, 2160)) => "4K",
        _ => "",
    };
    window.set_wide(wide.into());
    window.set_high(high.into());
    window.set_size_choice(choice.into());
}

fn show_rate(window: &ProjectSettings, project: &Project) {
    let rate = project
        .look
        .fps
        .map_or(String::new(), |fps| fps.to_string());
    let usual = ["24", "25", "30", "50", "60"].contains(&rate.as_str());
    window.set_rate_choice(
        match (project.look.fps, usual) {
            (None, _) => "Auto",
            (_, true) => rate.as_str(),
            _ => "",
        }
        .into(),
    );
    window.set_rate(rate.into());
}

fn show_color(window: &ProjectSettings, project: &Project) {
    let text = hex(project.look.background);
    window.set_color_choice(
        match text.as_str() {
            "#000000" => "Black",
            "#808080" => "Gray",
            "#ffffff" => "White",
            _ => "",
        }
        .into(),
    );
    window.set_color_text(text.into());
    let [red, green, blue] = project.look.background;
    window.set_swatch(slint::Color::from_rgb_u8(red, green, blue));
}

fn show_look(window: &ProjectSettings, project: &Project) {
    show_size(window, project);
    show_rate(window, project);
    show_color(window, project);
    window.set_fit(project.look.fit.name().into());
    window.set_fit_choice(
        match project.look.fit {
            Fit::Inside => "Fit",
            Fit::Fill => "Fill",
            Fit::Original => "Original size",
        }
        .into(),
    );
}

fn retarget(ui: &App, state: &mut State, before: (Option<(u32, u32)>, Fit)) {
    let project = &state.project;
    let (size, fit) = (project.look.size, project.look.fit);
    let Some(canvas) = size else {
        return;
    };
    let rows = ui.get_clips();
    for (index, entry) in state
        .entries
        .iter_mut()
        .enumerate()
        .filter(|entry| entry.1.audio.is_none())
    {
        let old = before.0.map(|size| placement(size, entry.size, before.1));
        if entry.place != Place::default() && Some(entry.place) != old {
            continue;
        }
        entry.place = placement(canvas, entry.size, fit);
        if let Some(mut row) = rows.row_data(index) {
            (row.zoom_x, row.zoom_y) = (entry.place.scale[0], entry.place.scale[1]);
            rows.set_row_data(index, row);
        }
    }
}

// a field with a value that does not fit keeps its text, so the user can finish typing
fn change(ui: &App, shared: &Shared, window: &ProjectSettings) {
    let mut state = lock(shared);
    let project = &mut state.project;
    let before = (project.look.size, project.look.fit);
    let side = |text: &str| {
        let side = text.trim().parse::<u32>().ok();
        side.filter(|side| SIDES.contains(side))
    };
    let (wide, high) = (window.get_wide(), window.get_high());
    let auto = wide.trim().is_empty() && high.trim().is_empty();
    let size = side(&wide).zip(side(&high)).map(even);
    if (size.is_some() || auto) && size != project.look.size {
        project.look.size = size;
        project.look.chosen = size.is_some();
    }
    if size.is_some() || auto {
        show_size(window, project);
    }
    let rate = window.get_rate();
    let fps = rate.trim().parse::<f64>().ok();
    let fps = fps.filter(|fps| RATES.contains(fps));
    if fps.is_some() || rate.trim().is_empty() {
        project.look.fps = fps;
        show_rate(window, project);
    }
    if let Some(background) = color(&window.get_color_text()) {
        project.look.background = background;
        show_color(window, project);
    }
    project.look.fit = Fit::parse(&window.get_fit());
    retarget(ui, &mut state, before);
    touch(ui, &mut state);
    drop(state);
    fit(ui, shared);
    refresh(ui, shared);
}

fn present(ui: &App, shared: &Shared) {
    WINDOW.with_borrow_mut(|made| {
        let make = || {
            ProjectSettings::new().ok().inspect(|window| {
                let (weak, shared, shown) = (ui.as_weak(), Arc::clone(shared), window.as_weak());
                window.on_changed(move || {
                    if let (Some(ui), Some(window)) = (weak.upgrade(), shown.upgrade()) {
                        change(&ui, &shared, &window);
                    }
                });
            })
        };
        let Some(window) = crate::components::reuse(made, make) else {
            return;
        };
        show_look(window, &lock(shared).project);
        themes::paint(&window.global::<Theme>(), &ui.get_theme(), ui.get_light());
        crate::components::present(window);
    });
}

fn target(ui: &App, state: &State) -> Option<usize> {
    let missing = |index: &usize| state.entries[*index].missing;
    let chosen = crate::view::selected(ui).filter(missing);
    chosen.or_else(|| (0..state.entries.len()).find(missing))
}

pub fn locate(ui: &App, shared: &Shared) {
    let index = target(ui, &lock(shared));
    if let Some(index) = index {
        replace(ui, shared, index);
    }
}

pub fn replace(ui: &App, shared: &Shared, index: usize) {
    let state = lock(shared);
    let entry = &state.entries[index];
    let id = entry.id;
    let name = entry.clip.file_name().unwrap_or_default().to_string_lossy();
    let verb = if entry.missing { "Locate" } else { "Replace" };
    let folder = entry.clip.parent().filter(|folder| folder.is_dir());
    let folder = folder.or(state.project.path.as_ref().and_then(|path| path.parent()));
    let mut dialog = rfd::AsyncFileDialog::new().set_title(format!("{verb} {name}"));
    if let Some(folder) = folder.filter(|folder| folder.is_dir()) {
        dialog = dialog.set_directory(folder);
    }
    let (old, missing) = (entry.clip.clone(), entry.missing);
    drop(state);
    let (weak, shared) = (ui.as_weak(), Arc::clone(shared));
    let _ = slint::spawn_local(async move {
        if let (Some(file), Some(ui)) = (dialog.pick_file().await, weak.upgrade()) {
            match missing {
                true => relink(&ui, &shared, &old, file.path()),
                false => swap(&ui, &shared, id, file.path()),
            }
        }
    });
}

fn show(ui: &App, shared: &Shared, state: MutexGuard<State>) {
    let rows = ui.get_clips();
    for (place, entry) in state.entries.iter().enumerate() {
        if let Some(old) = rows.row_data(place) {
            let blank = row(entry);
            rows.set_row_data(
                place,
                crate::Clip {
                    picked: old.picked,
                    progress: old.progress,
                    ..blank
                },
            );
        }
    }
    drop(state);
    fit(ui, shared);
    fill(ui, shared);
    select(ui, shared, ui.get_selected());
    refresh(ui, shared);
}

fn rescale(entry: &mut Entry, was: (u32, u32), canvas: Option<(u32, u32)>, kind: Fit) {
    let Some(canvas) = canvas.filter(|_| entry.audio.is_none()) else {
        return;
    };
    let (was, now) = (
        placement(canvas, was, kind),
        placement(canvas, entry.size, kind),
    );
    for side in 0..2 {
        entry.place.scale[side] *= now.scale[side] / was.scale[side];
    }
}

fn swap(ui: &App, shared: &Shared, id: u64, file: &Path) {
    let Ok(probed) = probe(file) else {
        return status(ui, "Couldn't read that file".into());
    };
    let mut state = lock(shared);
    let Some(link) = state
        .entries
        .iter()
        .find(|entry| entry.id == id)
        .map(|entry| entry.link)
    else {
        return;
    };
    layout(ui, &mut state);
    let own = |entry: &&Entry| entry.link == link;
    let parts = || state.entries.iter().filter(own);
    let sounds = parts()
        .filter_map(|entry| entry.audio)
        .max()
        .map_or(0, |last| last + 1);
    let picture = parts().any(|entry| entry.audio.is_none());
    let still = parts().any(|entry| entry.still);
    let start = parts().map(|entry| f64::from(entry.cut.0) * entry.seconds);
    let long = start.fold(0.0, f64::max) < probed.seconds - 0.1;
    let same = (probed.fps > 0.0 || probed.still) == picture && probed.still == still;
    if !same || probed.tracks.len() < sounds || !(long || still) {
        drop(state);
        return status(ui, "Couldn't use that file".into());
    }
    let old: Vec<Entry> = parts().cloned().collect();
    record(ui, &mut state, false);
    let fresh = Media::read(file, state.project.path.as_deref()).ok();
    let canvas = state.project.look.size;
    let kind = state.project.look.fit;
    state
        .project
        .refs
        .extend(fresh.map(|fresh| (file.to_owned(), fresh)));
    let bytes = std::fs::metadata(file).map_or(0, |meta| meta.len());
    let mut shorter = false;
    for entry in state.entries.iter_mut().filter(|entry| entry.link == link) {
        let (kept, size) = (entry.kept(), entry.size);
        entry.clip = file.to_owned();
        entry.missing = false;
        entry.bytes = bytes;
        entry.keys = Arc::default();
        entry.differences = Arc::default();
        entry.measured = None;
        adopt(entry, &probed);
        shorter |= entry.kept() < kept - 0.01;
        rescale(entry, size, canvas, kind);
    }
    old.iter()
        .for_each(|gone| crate::clips::forget(&state, gone));
    show(ui, shared, state);
    if shorter {
        crate::preview::announce(ui, "The clip is shorter now".into(), "");
    }
}

fn relink(ui: &App, shared: &Shared, missing: &Path, file: &Path) {
    let mut state = lock(shared);
    layout(ui, &mut state);
    let index = file::index(file.parent().unwrap_or(Path::new("")));
    let mut moves = vec![(missing.to_owned(), file.to_owned())];
    let mut others: Vec<PathBuf> = state
        .entries
        .iter()
        .filter(|entry| entry.missing)
        .map(|entry| entry.clip.clone())
        .collect();
    others.sort();
    others.dedup();
    for other in others.into_iter().filter(|other| other != missing) {
        let known = state.project.refs.get(&other);
        let found = known.and_then(|known| file::pick(&index, known));
        moves.extend(found.map(|found| (other, found)));
    }
    let canvas = state.project.look.size;
    let kind = state.project.look.fit;
    let mut done = 0;
    for (old, new) in moves {
        let Ok(probed) = probe(&new) else {
            continue;
        };
        if done == 0 {
            record(ui, &mut state, false);
        }
        let target = state.project.path.clone();
        let fresh = Media::read(&new, target.as_deref()).ok();
        state.project.refs.remove(&old);
        if let Some(fresh) = fresh {
            state.project.refs.insert(new.clone(), fresh);
        }
        for entry in state.entries.iter_mut().filter(|entry| entry.clip == old) {
            entry.clip = new.clone();
            entry.missing = false;
            entry.bytes = std::fs::metadata(&new).map_or(0, |meta| meta.len());
            entry.keys = Arc::default();
            let was = entry.size;
            adopt(entry, &probed);
            rescale(entry, was, canvas, kind);
        }
        done += 1;
    }
    if done == 0 {
        drop(state);
        return status(ui, "Couldn't read that file".into());
    }
    show(ui, shared, state);
    let still = ui.get_missing();
    let text = match (done, still) {
        (1, 0) => "Found the file".to_owned(),
        (done, 0) => format!("Found {done} files"),
        (done, still) => format!("Found {done}, {still} still missing"),
    };
    crate::preview::announce(ui, text, if still > 0 { "Locate" } else { "" });
}

pub fn wire(ui: &App, state: &Shared) {
    let hook = |run: fn(&App, &Shared)| {
        let (weak, shared) = (ui.as_weak(), Arc::clone(state));
        move || {
            if let Some(ui) = weak.upgrade() {
                run(&ui, &shared);
            }
        }
    };
    ui.on_new_project(hook(new));
    ui.on_open_project(hook(browse));
    ui.on_save_project(hook(|ui, shared| save(ui, shared, None)));
    ui.on_save_project_as(hook(|ui, shared| choose(ui, shared, None)));
    ui.on_show_project(hook(present));
    ui.on_locate(hook(locate));
    let (weak, shared) = (ui.as_weak(), Arc::clone(state));
    ui.on_replace_clip(move |index| {
        if let Some(ui) = weak.upgrade() {
            replace(&ui, &shared, index as usize);
        }
    });
    let (weak, shared) = (ui.as_weak(), Arc::clone(state));
    ui.on_open_recent(move |path| {
        if let Some(ui) = weak.upgrade() {
            open_path(&ui, &shared, PathBuf::from(path.as_str()));
        }
    });
    let weak = ui.as_weak();
    ui.on_clear_recent(move || {
        if let Some(ui) = weak.upgrade() {
            SESSION.with_borrow_mut(|session| session.recent.clear());
            recents(&ui);
        }
    });
}
