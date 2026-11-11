use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use interpolini_core::config;
use interpolini_core::mask::{self, Gray};
use slint::{ComponentHandle, ModelRc, Rgba8Pixel, SharedPixelBuffer, VecModel};

use crate::clips::pick;
use crate::preview::{self, say};
use crate::project::touch;
use crate::state::{Entry, Shared, State, lock};
use crate::view::{load, refresh, stale};
use crate::{App, MaskTile, Masks, Note, history, services};

const WIDE: u32 = 480;
const IMAGES: &[&str] = &["png", "jpg", "jpeg", "webp", "bmp"];

type Stamp = (String, Option<SystemTime>, u64);

thread_local! {
    static SEEN: RefCell<Option<Vec<Stamp>>> = const { RefCell::new(None) };
    static SMALL: RefCell<HashMap<String, Gray>> = RefCell::new(HashMap::new());
}

fn listing() -> Vec<Stamp> {
    let Ok(read) = std::fs::read_dir(config::app_dir().join("masks")) else {
        return Vec::new();
    };
    let stamp = |entry: std::fs::DirEntry| {
        let path = entry.path();
        let name = path.file_stem()?.to_str()?.trim().to_owned();
        let meta = std::fs::metadata(mask::path(&name)).ok();
        let meta = meta.filter(|meta| meta.is_file())?;
        png(&path).then(|| (name, meta.modified().ok(), meta.len()))
    };
    let mut found: Vec<Stamp> = read.flatten().filter_map(stamp).collect();
    found.sort_by_key(|stamp| (stamp.0.to_lowercase(), stamp.0.clone()));
    // a.png and a.PNG are one mask
    found.dedup_by(|a, b| a.0 == b.0);
    found
}

fn shrink(gray: &Gray, soft: f32) -> Option<Gray> {
    let high = (u64::from(WIDE) * u64::from(gray.height) / u64::from(gray.width)) as u32;
    let high = high.clamp(1, 2 * WIDE);
    let pixels = gray.fit(WIDE, high, soft).ok()?;
    Some(Gray {
        width: WIDE,
        height: high,
        pixels,
    })
}

fn picture(gray: &Gray) -> SharedPixelBuffer<Rgba8Pixel> {
    let mut pixels = SharedPixelBuffer::new(gray.width, gray.height);
    for (pixel, value) in pixels.make_mut_slice().iter_mut().zip(&gray.pixels) {
        *pixel = Rgba8Pixel::new(255, 255, 255, 255 - value);
    }
    pixels
}

pub fn tint(gray: &Gray, soft: f32) -> slint::Image {
    shrink(gray, soft)
        .map(|small| slint::Image::from_rgba8(picture(&small)))
        .unwrap_or_default()
}

pub fn scan(ui: &App, state: &Shared) {
    let found = listing();
    let before = SEEN.with_borrow_mut(|seen| seen.replace(found.clone()));
    if before.as_ref() == Some(&found) {
        return;
    }
    if let Some(before) = before {
        let gone = before.iter().filter(|stamp| !found.contains(stamp));
        let new = found.iter().filter(|stamp| !before.contains(stamp));
        preview::remask(gone.chain(new).map(|stamp| stamp.0.clone()).collect());
        refresh(ui, state);
    }
    let weak = ui.as_weak();
    services::background("masks", move |_| {
        let made: Vec<_> = found
            .iter()
            .map(|stamp| {
                let small = mask::load(&stamp.0)
                    .ok()
                    .and_then(|gray| shrink(&gray, 0.0));
                small.map(|small| (picture(&small), small))
            })
            .collect();
        let _ = weak.upgrade_in_event_loop(move |ui| {
            // a newer scan can finish first
            if SEEN.with_borrow(|seen| seen.as_ref() != Some(&found)) {
                return;
            }
            let mut small = HashMap::new();
            let mut tiles = Vec::new();
            for ((name, ..), made) in found.into_iter().zip(made) {
                let (pixels, gray) = made.unzip();
                tiles.push(MaskTile {
                    path: mask::path(&name).to_string_lossy().as_ref().into(),
                    picture: pixels.map(slint::Image::from_rgba8).unwrap_or_default(),
                    name: name.as_str().into(),
                });
                small.extend(gray.map(|gray| (name, gray)));
            }
            SMALL.set(small);
            let masks = ui.global::<Masks>();
            if !masks.get_editing() {
                masks.set_hover(slint::Image::default());
            }
            masks.set_list(ModelRc::new(VecModel::from(tiles)));
        });
    });
}

pub fn free(stem: &str) -> String {
    let mut names = (1..).map(|count| match count {
        1 => stem.to_string(),
        count => format!("{stem} {count}"),
    });
    names
        .find(|name| !mask::path(name).exists())
        .unwrap_or_default()
}

pub fn rename(old: &str, new: &str) -> std::io::Result<()> {
    if new.contains(['/', '\\']) {
        return Err(std::io::ErrorKind::InvalidInput.into());
    }
    std::fs::rename(mask::path(old), mask::path(new))
}

pub fn retarget(state: &mut State, old: &str, new: &str) -> bool {
    let swap = |entry: &mut Entry| {
        if entry.config.mask.image == old {
            entry.config.mask.image = new.into();
        }
    };
    let State { entries, draft, .. } = state;
    let used = entries
        .iter()
        .chain([&*draft])
        .any(|entry| entry.config.mask.image == old);
    entries.iter_mut().chain([draft]).for_each(swap);
    history::amend(swap);
    for preset in config::scan(&config::app_dir(), false) {
        let mut config = config::read(preset.clone()).config;
        if config.mask.image == old {
            config.mask.image = new.into();
            let _ = std::fs::write(&preset.path, config.to_ini());
            stale(&preset.path);
        }
    }
    used
}

pub fn import(ui: &App, state: &Shared, files: Vec<PathBuf>) {
    let mut chosen = None;
    for file in files {
        let stem = file.file_stem().unwrap_or_default().to_string_lossy();
        let name = free(match stem.trim() {
            "" => "mask",
            stem => stem,
        });
        if mask::read(&file).is_ok_and(|gray| crate::editor::write(&name, &gray).is_ok()) {
            chosen = Some(name);
            continue;
        }
        let shown = file.file_name().unwrap_or_default().to_string_lossy();
        say(ui, format!("Couldn't add {shown}"), Note::Warning, -1.0);
    }
    scan(ui, state);
    if let Some(name) = chosen {
        let mut settings = ui.get_settings();
        settings.mask_image = name.into();
        settings.mask = true;
        ui.set_settings(settings);
        ui.invoke_changed();
    }
}

fn png(file: &Path) -> bool {
    file.extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("png"))
}

pub fn image(file: &Path) -> bool {
    let extension = file.extension().and_then(|extension| extension.to_str());
    extension.is_some_and(|extension| IMAGES.contains(&extension.to_lowercase().as_str()))
}

pub fn wire(ui: &App, state: &Shared) {
    let masks = ui.global::<Masks>();
    let (weak, shared) = (ui.as_weak(), Arc::clone(state));
    masks.on_scan(move || {
        if let Some(ui) = weak.upgrade() {
            scan(&ui, &shared);
        }
    });
    let (weak, shared) = (ui.as_weak(), Arc::clone(state));
    masks.on_add(move || {
        let shared = Arc::clone(&shared);
        pick(&weak, "Images", IMAGES, move |ui, files| {
            import(ui, &shared, files)
        });
    });
    let (weak, shared) = (ui.as_weak(), Arc::clone(state));
    masks.on_remove(move |name| {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        if std::fs::remove_file(mask::path(&name)).is_err() {
            say(&ui, format!("Couldn't delete {name}"), Note::Warning, -1.0);
        }
        scan(&ui, &shared);
        let mut settings = ui.get_settings();
        if settings.mask_image == name && !mask::path(&name).exists() {
            settings.mask_image = "".into();
            ui.set_settings(settings);
            ui.invoke_changed();
        }
    });
    let (weak, shared) = (ui.as_weak(), Arc::clone(state));
    masks.on_rename(move |old, new| {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        let new = new.trim();
        if new.is_empty() || old == new {
            return;
        }
        let real = |name: &str| std::fs::canonicalize(mask::path(name)).ok();
        if mask::path(new).exists() && real(new) != real(&old) {
            return say(&ui, format!("\"{new}\" exists"), Note::Warning, -1.0);
        }
        if rename(&old, new).is_err() {
            return say(&ui, format!("Couldn't rename {old}"), Note::Warning, -1.0);
        }
        let mut state = lock(&shared);
        if retarget(&mut state, &old, new) {
            touch(&ui, &mut state);
        }
        load(&ui, &mut state);
        drop(state);
        scan(&ui, &shared);
    });
    let (weak, shared) = (ui.as_weak(), Arc::clone(state));
    masks.on_duplicate(move |name| {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        let stem = match name.rsplit_once(' ') {
            Some((stem, count)) if count.parse::<u32>().is_ok() => stem,
            _ => &name,
        };
        if std::fs::copy(mask::path(&name), mask::path(&free(stem))).is_err() {
            say(
                &ui,
                format!("Couldn't duplicate {name}"),
                Note::Warning,
                -1.0,
            );
        }
        scan(&ui, &shared);
    });
    let weak = ui.as_weak();
    masks.on_show(move |name| {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        let soft = ui.get_settings().mask_soft;
        let hover = crate::editor::tint(&ui).or_else(|| {
            SMALL.with_borrow(|small| small.get(name.as_str()).map(|gray| tint(gray, soft)))
        });
        ui.global::<Masks>().set_hover(hover.unwrap_or_default());
    });
    masks.on_found(|_, name| mask::path(name.trim()).is_file());
}
