use std::cell::RefCell;
use std::fs::{self, File};
use std::io::{self, Cursor};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use interpolini_core::config;
use sha2::{Digest, Sha256};
use slint::ComponentHandle;

use crate::components::{Outcome, Report, download, hex, present, restart, text};
use crate::{App, Theme, Update, themes};

const RELEASES: &str = "https://github.com/Z1xus/interpolini/releases";
const DAMAGED: &str = "The download is damaged. Try again.";
const CURRENT: &str = env!("CARGO_PKG_VERSION");

fn package() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => Some("interpolini-linux-x86_64"),
        ("windows", "x86_64") => Some("interpolini-windows-x86_64"),
        ("macos", "aarch64") => Some("interpolini-macos-arm64"),
        ("macos", "x86_64") => Some("interpolini-macos-x86_64"),
        _ => None,
    }
}

fn place(relative: &Path) -> Option<PathBuf> {
    let app = config::app_dir();
    if cfg!(target_os = "macos") {
        // the program is in Contents/MacOS of the bundle, which can have another name
        let inside = relative.strip_prefix("interpolini.app").ok()?;
        return Some(app.ancestors().nth(2)?.join(inside));
    }
    Some(app.join(relative))
}

fn numbers(version: &str) -> Option<Vec<u32>> {
    let parts = version.trim_start_matches('v').split('.');
    parts.map(|part| part.parse().ok()).collect()
}

// github sends the latest release page to the page of its tag
fn newest() -> Option<String> {
    let config = ureq::Agent::config_builder()
        .max_redirects(0)
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_secs(10)))
        .build();
    let page = format!("{RELEASES}/latest");
    let response = ureq::Agent::new_with_config(config)
        .get(&page)
        .call()
        .ok()?;
    let location = response.headers().get("location")?.to_str().ok()?;
    let version = location.rsplit_once("/tag/")?.1.trim_start_matches('v');
    (numbers(version)? > numbers(CURRENT)?).then(|| version.to_owned())
}

pub fn check(ui: &App) {
    if !ui.get_updates() || package().is_none() {
        return;
    }
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        if let Some(version) = newest() {
            let _ = weak.upgrade_in_event_loop(move |ui| ui.set_update(version.into()));
        }
    });
}

pub fn clean() {
    let _ = fs::remove_dir_all(config::app_dir().join("update-old"));
}

fn beside(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    name.into()
}

fn apply(zip: &Path, sha: &str) -> Outcome {
    let bytes = fs::read(zip).map_err(text)?;
    if hex(&Sha256::digest(&bytes)) != sha {
        return Err(DAMAGED.into());
    }
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).map_err(text)?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |time| time.as_millis());
    let old = config::app_dir().join("update-old").join(stamp.to_string());
    let mut files: Vec<(PathBuf, PathBuf, PathBuf)> = Vec::new();
    let mut unpack = || -> Outcome {
        for index in 0..archive.len() {
            let mut entry = archive.by_index(index).map_err(text)?;
            // the first part of the path is the folder of the package
            let relative: Option<PathBuf> = entry
                .enclosed_name()
                .map(|path| path.components().skip(1).collect());
            let Some(relative) = relative.filter(|_| entry.is_file()) else {
                continue;
            };
            let Some(target) = place(&relative) else {
                continue;
            };
            let new = beside(&target, ".new");
            files.push((target.clone(), new.clone(), old.join(relative)));
            if let Some(folder) = target.parent() {
                fs::create_dir_all(folder).map_err(text)?;
            }
            let mut file = File::create(&new).map_err(text)?;
            io::copy(&mut entry, &mut file).map_err(text)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = entry.unix_mode().unwrap_or(0o644);
                fs::set_permissions(&new, fs::Permissions::from_mode(mode)).map_err(text)?;
            }
        }
        Ok(())
    };
    let unpacked = unpack();
    let mut moves: Vec<(&Path, &Path)> = Vec::new();
    let swapped = unpacked.and_then(|()| {
        for (target, new, away) in &files {
            if target.exists() {
                if let Some(folder) = away.parent() {
                    fs::create_dir_all(folder).map_err(text)?;
                }
                fs::rename(target, away).map_err(text)?;
                moves.push((target, away));
            }
            fs::rename(new, target).map_err(text)?;
            moves.push((new, target));
        }
        Ok(())
    });
    if swapped.is_err() {
        for (from, to) in moves.into_iter().rev() {
            let _ = fs::rename(to, from);
        }
        for (_, new, _) in &files {
            let _ = fs::remove_file(new);
        }
    }
    swapped
}

pub fn helper(arguments: &[String]) -> ! {
    let code = match arguments {
        [zip, sha] => match apply(Path::new(zip), sha) {
            Ok(()) => 0,
            Err(error) if error == DAMAGED => 3,
            Err(_) => 4,
        },
        _ => 4,
    };
    std::process::exit(code)
}

fn writable(folder: &Path) -> bool {
    let probe = folder.join(".update-test");
    let writable = File::create(&probe).is_ok();
    let _ = fs::remove_file(probe);
    writable
}

fn elevate(zip: &Path, sha: &str) -> Outcome {
    let exe = config::exe();
    let mut command = if cfg!(windows) {
        // powershell asks for the permission, and an argument with spaces needs its own quotes
        let quote = |path: &Path| path.to_string_lossy().replace('\'', "''");
        let script = format!(
            "exit (Start-Process -FilePath '{}' -ArgumentList '--update','\"{}\"','{sha}' -Verb RunAs -Wait -PassThru).ExitCode",
            quote(exe),
            quote(zip)
        );
        let mut command = Command::new("powershell");
        command.args(["-NoProfile", "-NonInteractive", "-Command", &script]);
        command
    } else if cfg!(target_os = "macos") {
        let shell = |path: &Path| format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"));
        let line = format!("{} --update {} {sha}", shell(exe), shell(zip));
        let line = line.replace('\\', "\\\\").replace('"', "\\\"");
        let script = format!("do shell script \"{line}\" with administrator privileges");
        let mut command = Command::new("osascript");
        command.args(["-e", &script]);
        command
    } else {
        let mut command = Command::new("pkexec");
        command.arg(exe).arg("--update").arg(zip).arg(sha);
        command
    };
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // without a console window
        command.creation_flags(0x0800_0000);
    }
    match command.status().map(|status| status.code()) {
        Ok(Some(0)) => Ok(()),
        Ok(Some(3)) => Err(DAMAGED.into()),
        Ok(Some(4)) => Err("Couldn't install the update.".into()),
        Ok(_) => Err("Administrator permission is required.".into()),
        Err(_) => Err("Move interpolini to a folder you own, then try again.".into()),
    }
}

fn install(version: &str, report: Report, stop: &AtomicBool) -> Outcome {
    let package = package().ok_or("This system can't update itself.")?;
    let base = format!("{RELEASES}/download/v{version}");
    let sums = ureq::get(format!("{base}/SHA256SUMS"))
        .call()
        .map_err(text)?;
    let sums = sums.into_body().read_to_string().map_err(text)?;
    let file = format!("{package}.zip");
    let sha = sums
        .lines()
        .find(|line| line.ends_with(&file))
        .and_then(|line| line.split_whitespace().next())
        .ok_or("This update has no download for this system.")?;
    // must not exist yet, all users can share the folder for temporary files
    let folder = std::env::temp_dir().join(format!("interpolini-update-{}", std::process::id()));
    let _ = fs::remove_dir_all(&folder);
    fs::create_dir(&folder).map_err(text)?;
    let zip = folder.join(&file);
    let downloaded = download(&format!("{base}/{file}"), sha, &zip, report, stop);
    let outcome = downloaded.and_then(|()| {
        report(1.0, "Installing…".into());
        match writable(&config::app_dir()) {
            true => apply(&zip, sha),
            false => elevate(&zip, sha),
        }
    });
    let _ = fs::remove_dir_all(folder);
    outcome
}

fn attach(ui: &App, window: &Update, stop: &Arc<AtomicBool>) {
    let cancel = Arc::clone(stop);
    window.on_cancel(move || cancel.store(true, Ordering::Relaxed));

    let (weak, shown, stop) = (ui.as_weak(), window.as_weak(), Arc::clone(stop));
    window.on_install(move || {
        let (Some(ui), Some(window)) = (weak.upgrade(), shown.upgrade()) else {
            return;
        };
        window.set_busy(true);
        window.set_done(false);
        window.set_note("".into());
        window.set_progress("".into());
        window.set_fraction(0.0);
        stop.store(false, Ordering::Relaxed);
        let (version, weak, shown, stop) = (
            ui.get_update().to_string(),
            weak.clone(),
            shown.clone(),
            Arc::clone(&stop),
        );
        std::thread::spawn(move || {
            let told = shown.clone();
            let report = move |done: f32, progress: String| {
                let _ = told.upgrade_in_event_loop(move |window| {
                    window.set_fraction(done);
                    window.set_progress(progress.into());
                });
            };
            let outcome = install(&version, &report, &stop);
            let _ = weak.upgrade_in_event_loop(move |ui| {
                let Some(window) = shown.upgrade() else {
                    return;
                };
                window.set_busy(false);
                window.set_done(outcome.is_ok());
                window.set_note(outcome.err().unwrap_or_default().into());
                if window.get_done() && !ui.get_rendering() && !ui.get_unsaved() {
                    restart(&ui);
                }
            });
        });
    });

    let weak = ui.as_weak();
    window.on_restart(move || {
        if let Some(ui) = weak.upgrade() {
            restart(&ui);
        }
    });
}

pub fn wire(ui: &App) {
    clean();
    check(ui);
    let made: Rc<RefCell<Option<Update>>> = Rc::default();
    let stop = Arc::new(AtomicBool::new(false));
    let weak = ui.as_weak();
    ui.on_show_update(move || {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        let mut made = made.borrow_mut();
        if made.is_none() {
            *made = Update::new()
                .ok()
                .inspect(|window| attach(&ui, window, &stop));
        }
        let Some(window) = made.as_ref() else {
            return;
        };
        window.set_version(ui.get_update());
        window.set_current(CURRENT.into());
        themes::paint(&window.global::<Theme>(), &ui.get_theme(), ui.get_light());
        present(window);
    });
}
