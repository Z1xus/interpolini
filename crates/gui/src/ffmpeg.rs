use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use interpolini_core::config;
use slint::ComponentHandle;

use crate::components::{Outcome, finish, text};
use crate::{App, Components};

const FFMPEG: [&str; 5] = ["avcodec", "avformat", "avutil", "swscale", "swresample"];

// the version of an ffmpeg library from its file name, as linux, windows and macos write it:
// libavcodec.so.62, avcodec-62.dll and libavcodec.62.dylib
fn version<'a>(name: &'a str, library: &str) -> Option<&'a str> {
    let rest = name.strip_prefix("lib").unwrap_or(name);
    let rest = rest.strip_prefix(library)?;
    let linux = rest.strip_prefix(".so.");
    let windows = || rest.strip_prefix('-')?.strip_suffix(".dll");
    let macos = || rest.strip_prefix('.')?.strip_suffix(".dylib");
    linux.or_else(windows).or_else(macos)
}

fn libraries(folder: &Path) -> Vec<PathBuf> {
    let files = fs::read_dir(folder).into_iter().flatten().flatten();
    let ffmpeg = |file: &fs::DirEntry| {
        let name = file.file_name();
        let name = name.to_string_lossy();
        let versions = FFMPEG.iter().filter_map(|library| version(&name, library));
        versions
            .into_iter()
            .any(|version| version.parse::<u32>().is_ok())
    };
    files.filter(ffmpeg).map(|file| file.path()).collect()
}

// the libraries that an earlier switch took out while the app had them open
pub fn clean() {
    let _ = fs::remove_dir_all(config::app_dir().join("ffmpeg-old"));
}

fn kept() -> PathBuf {
    config::app_dir().join("ffmpeg-bundled")
}

fn pick_folder(done: impl FnOnce(PathBuf) + Send + 'static) {
    std::thread::spawn(move || {
        if let Some(folder) = rfd::FileDialog::new().pick_folder() {
            done(folder);
        }
    });
}

// the app loads ffmpeg from its own folder first, and from the system when there is none
fn ffmpeg() -> &'static str {
    let (own, bundled) = (libraries(&config::app_dir()), libraries(&kept()));
    match (own.is_empty(), bundled.is_empty()) {
        (false, true) => "bundled",
        (false, false) => "custom",
        (true, _) => "system",
    }
}

fn use_ffmpeg(choice: &str, folder: Option<&Path>) -> Outcome {
    let app = config::app_dir();
    let own = libraries(&app);
    let bundled = match libraries(&kept()) {
        kept if kept.is_empty() => own.clone(),
        kept => kept,
    };
    let names = bundled.iter().filter_map(|library| library.file_name());
    let names: Vec<_> = names
        .map(|name| name.to_string_lossy().into_owned())
        .collect();
    // another ffmpeg must have the same library versions, or the app does not start
    let found = match (choice, folder) {
        ("system", _) => {
            let listed = Command::new("ldconfig").arg("-p").output();
            let listed = listed.or_else(|_| Command::new("/sbin/ldconfig").arg("-p").output());
            let listed = listed.map(|list| String::from_utf8_lossy(&list.stdout).into_owned());
            let listed = listed.unwrap_or_default();
            names.iter().all(|name| listed.contains(name.as_str()))
        }
        (_, Some(folder)) => names.iter().all(|name| folder.join(name).exists()),
        _ => true,
    };
    if !found {
        return Err("This FFmpeg is not the same version as the bundled one.".into());
    }
    // the bundled copy is kept, and then the folder of the app gets the libraries of the choice
    if own == bundled {
        fs::create_dir_all(kept()).map_err(text)?;
        for library in &own {
            let name = library.file_name().unwrap_or_default();
            fs::rename(library, kept().join(name)).map_err(text)?;
        }
    } else {
        // windows cannot remove a library that the app has open, so it moves away and goes at the next start
        let old = app.join("ffmpeg-old");
        fs::create_dir_all(&old).map_err(text)?;
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |time| time.as_millis());
        for library in &own {
            let name = library.file_name().unwrap_or_default().to_string_lossy();
            fs::rename(library, old.join(format!("{stamp}-{name}"))).map_err(text)?;
        }
    }
    for name in &names {
        match (choice, folder) {
            ("bundled", _) => fs::rename(kept().join(name), app.join(name)).map_err(text)?,
            (_, Some(folder)) => drop(fs::copy(folder.join(name), app.join(name)).map_err(text)?),
            _ => {}
        }
    }
    if choice == "bundled" {
        let _ = fs::remove_dir(kept());
    }
    Ok(())
}

pub fn show(window: &Components) {
    window.set_ffmpeg(ffmpeg().into());
    let bundled = libraries(&config::app_dir()).len() + libraries(&kept()).len();
    // only linux has an ffmpeg of the system that fits
    window.set_ffmpeg_system(cfg!(target_os = "linux"));
    window.set_ffmpeg_fixed(bundled == 0);
}

pub fn wire(ui: &App, window: &Components) {
    let weak = ui.as_weak();
    window.on_restart_app(move || {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        // with settings that are not saved the app only asks about them, and does not start again
        if !ui.get_unsaved()
            && let Ok(app) = std::env::current_exe()
        {
            let _ = Command::new(app).spawn();
        }
        ui.invoke_quit();
    });

    let (weak, shown) = (ui.as_weak(), window.as_weak());
    window.on_use_ffmpeg(move |choice| {
        if let (Some(ui), Some(window)) = (weak.upgrade(), shown.upgrade()) {
            let outcome = use_ffmpeg(&choice, None);
            window.set_restart(outcome.is_ok());
            finish(&ui, &window, outcome);
        }
    });

    let (weak, shown) = (ui.as_weak(), window.as_weak());
    window.on_choose_ffmpeg(move || {
        let (weak, shown) = (weak.clone(), shown.clone());
        pick_folder(move |folder| {
            let outcome = use_ffmpeg("custom", Some(&folder));
            let _ = weak.upgrade_in_event_loop(move |ui| {
                if let Some(window) = shown.upgrade() {
                    window.set_restart(outcome.is_ok());
                    finish(&ui, &window, outcome);
                }
            });
        });
    });
}
