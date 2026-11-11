use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use interpolini_core::{config, mask};
use slint::{ComponentHandle, ModelRc, Rgba8Pixel, SharedPixelBuffer, VecModel};

use crate::clips::pick;
use crate::preview::{self, say};
use crate::state::Shared;
use crate::view::refresh;
use crate::{App, MaskTile, Masks, Note, services};

const WIDE: u32 = 480;

type Stamp = (String, Option<SystemTime>, u64);

thread_local! {
    static SEEN: RefCell<Option<Vec<Stamp>>> = const { RefCell::new(None) };
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

fn picture(name: &str) -> Option<SharedPixelBuffer<Rgba8Pixel>> {
    let gray = mask::load(name).ok()?;
    let high = (u64::from(WIDE) * u64::from(gray.height) / u64::from(gray.width)) as u32;
    let high = high.clamp(1, 2 * WIDE);
    let small = gray.fit(WIDE, high, 0.0).ok()?;
    let mut pixels = SharedPixelBuffer::new(WIDE, high);
    for (pixel, value) in pixels.make_mut_slice().iter_mut().zip(small) {
        *pixel = Rgba8Pixel::new(255, 255, 255, 255 - value);
    }
    Some(pixels)
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
        let made: Vec<_> = found.iter().map(|stamp| picture(&stamp.0)).collect();
        let _ = weak.upgrade_in_event_loop(move |ui| {
            // a newer scan can finish first
            if SEEN.with_borrow(|seen| seen.as_ref() != Some(&found)) {
                return;
            }
            let tiles = found
                .into_iter()
                .zip(made)
                .map(|((name, ..), pixels)| MaskTile {
                    path: mask::path(&name).to_string_lossy().as_ref().into(),
                    picture: pixels.map(slint::Image::from_rgba8).unwrap_or_default(),
                    name: name.into(),
                });
            let masks = ui.global::<Masks>();
            masks.set_hover(slint::Image::default());
            masks.set_list(ModelRc::new(VecModel::from_iter(tiles)));
        });
    });
}

pub fn import(ui: &App, state: &Shared, files: Vec<PathBuf>) {
    let mut chosen = None;
    for file in files {
        let stem = file.file_stem().unwrap_or_default().to_string_lossy();
        let mut names = (1..).map(|count| match count {
            1 => stem.to_string(),
            count => format!("{stem} {count}"),
        });
        let Some(name) = names.find(|name| !mask::path(name).exists()) else {
            continue;
        };
        let target = mask::path(&name);
        let copied = target
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|_| std::fs::copy(&file, &target));
        if copied.is_ok() && mask::load(&name).is_ok() {
            chosen = Some(name);
            continue;
        }
        let _ = std::fs::remove_file(&target);
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

pub fn png(file: &Path) -> bool {
    file.extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("png"))
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
        pick(&weak, "PNG", &["png"], move |ui, files| {
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
    masks.on_found(|_, name| mask::path(name.trim()).is_file());
}
