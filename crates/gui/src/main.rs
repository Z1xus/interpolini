#![windows_subsystem = "windows"]

mod clips;
mod components;
#[cfg(target_os = "linux")]
mod drop;
mod edit;
mod estimate;
mod ffmpeg;
mod file;
mod history;
#[cfg(target_os = "linux")]
mod icon;
mod playback;
mod preview;
mod project;
mod render;
mod services;
mod settings;
mod sound;
mod state;
mod themes;
mod tracks;
mod update;
mod view;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use interpolini_core::config::{self, Config};
use slint::{ComponentHandle, ModelRc, VecModel};

use settings::{preferences, settings, smooth_scroll};
use state::{Entry, Saved, Shared, State, Tracks, lock};
use view::{fit, select};

slint::include_modules!();

const VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), " (", env!("COMMIT"), ")");

// the system is asked on another thread, the answers take a moment
fn details(ui: &App, state: &Shared) -> impl FnOnce() -> String + Send + use<> {
    let config = lock(state).current(ui).config.to_ini();
    let (status, clip, log) = (ui.get_status(), ui.get_details(), ui.get_log());
    move || {
        let about = interpolini_core::about();
        format!("interpolini {VERSION}\n{about}\nclip: {clip}\nstatus: {status}\n{log}\n{config}")
    }
}

fn main() -> Result<(), slint::PlatformError> {
    let arguments: Vec<String> = std::env::args().collect();
    if arguments
        .get(1)
        .is_some_and(|argument| argument == "--update")
    {
        update::helper(&arguments[2..]);
    }
    #[cfg(windows)]
    config::mark();
    config::create();
    let _ = slint::set_xdg_app_id("interpolini");
    let ui = App::new()?;
    ui.global::<System>().set_smooth_scroll(smooth_scroll());
    ui.set_clips(ModelRc::new(VecModel::<Clip>::default()));
    ui.set_settings(settings(&Config::default()));
    ui.set_version(VERSION.into());
    ui.set_config("default".into());
    // themes with the same first word are one family, like github and github dimmed
    let first = |name: &'static str| name.split(' ').next().unwrap_or(name);
    let mut families: Vec<(&'static str, Vec<&'static str>)> = Vec::new();
    for (name, ..) in themes::THEMES {
        let alike = themes::THEMES
            .iter()
            .filter(|other| first(other.0) == first(name));
        let family = if alike.count() > 1 { first(name) } else { name };
        match families.iter_mut().find(|other| other.0 == family) {
            Some(found) => found.1.push(name),
            None => families.push((family, vec![name])),
        }
    }
    let text = |texts: Vec<&str>| {
        let texts = texts.into_iter().map(slint::SharedString::from);
        ModelRc::new(VecModel::from_iter(texts))
    };
    let families = families.into_iter().map(|(name, themes)| {
        let label = |theme: &&'static str| match theme.strip_prefix(name).map(str::trim) {
            Some("") | None => "default",
            Some(kind) => kind,
        };
        Family {
            name: name.into(),
            labels: text(themes.iter().map(label).collect()),
            themes: text(themes),
        }
    });
    ui.set_themes(ModelRc::new(VecModel::from_iter(families)));
    let saved = std::fs::read_to_string(preferences()).unwrap_or_default();
    let value = |key: &str| saved.lines().find_map(|line| line.strip_prefix(key));
    settings::load_prefs(&ui, &saved);
    let theme = value("theme: ").unwrap_or("interpolini");
    ui.set_theme(theme.into());
    ui.set_light(value("light: ") == Some("yes"));
    ui.set_separate(value("separate: ") == Some("yes"));
    ui.set_preview_off(value("preview: ") == Some("no"));
    ui.window()
        .set_maximized(value("maximized: ") == Some("yes"));
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        interpolini_core::find_encoders();
        let _ = weak.upgrade_in_event_loop(|ui| ui.set_encoders_found(true));
    });
    let sizes = ["Full", "1/2", "1/4"];
    ui.set_resolution(
        value("resolution: ")
            .filter(|size| sizes.contains(size))
            .unwrap_or("Auto")
            .into(),
    );
    ui.set_updates(value("updates: ") != Some("no"));
    let size = |key: &str| value(key).and_then(|size| size.parse::<f32>().ok());
    ui.set_rail(size("rail: ").unwrap_or(340.0));
    ui.set_deck(size("deck: ").unwrap_or(0.0));
    ui.set_volume(size("volume: ").unwrap_or(1.0).clamp(0.0, 1.0));
    if let Some(open) = value("sections: ") {
        let open = |name: &str| open.split(' ').any(|section| section == name);
        ui.set_sections(Sections {
            dedup: open("dedup"),
            pre: open("pre"),
            interpolation: open("interpolation"),
            masking: open("masking"),
            blending: open("blending"),
            color: open("color"),
            upscale: open("upscale"),
            output: open("output"),
        });
    }
    sound::level(ui.get_volume());
    ui.set_light_available(themes::has_light(theme));
    themes::apply(&ui, theme, ui.get_light());

    const SHORTCUTS: [&[(&str, &str)]; 2] = [
        &[
            ("Project", ""),
            ("New, open", "Ctrl N, O"),
            ("Save, save as", "Ctrl S, Ctrl Shift S"),
            ("Playback", ""),
            ("Play or pause", "Space, K"),
            ("Back or forward 1 second", "J, L"),
            ("Step 1 frame", "Left, Right"),
            ("Move 1 second", "Shift Left, Right"),
            ("Step to a keyframe, in lossless cut", "Alt Left, Right"),
            ("Volume", "Up, Down"),
            ("Mute", "M"),
            ("Go to start or end", "Home, End"),
            ("Preview", ""),
            ("Show the original", "Hold `"),
            ("Stretch", "Shift, drag an edge"),
            ("Save the frame", "Ctrl Shift E"),
        ],
        &[
            ("Timeline", ""),
            ("Zoom the timeline", "Ctrl wheel, Ctrl + and -"),
            ("Scroll the timeline", "Shift wheel"),
            ("Track height", "Alt wheel"),
            ("Separate video and audio", "Alt, drag"),
            ("Trim one clip of two", "Alt, drag an edge"),
            ("Change only one clip of a file", "Alt right click"),
            ("Editing", ""),
            ("Set start or end", "I, O"),
            ("Restore full length", "X"),
            ("Split", "S, Ctrl K, Ctrl B"),
            ("Remove clip", "Delete, Backspace"),
            ("Undo, redo", "Ctrl Z, Ctrl Y"),
            ("Select more clips", "Ctrl click"),
        ],
    ];
    const CREDITS: [&[(&str, &str)]; 1] = [&[
        ("interpolini", "GPL-3.0"),
        ("open-svpflow", "Apache-2.0"),
        ("FFmpeg, x264, x265", "GPL"),
        ("RIFE", "MIT"),
        ("rife-ncnn-vulkan (nihui, TNTwise)", "MIT"),
        ("ncnn", "BSD-3-Clause"),
        ("vs-mlrt (TensorRT models)", "GPL-3.0"),
        ("TensorRT, CUDA", "NVIDIA"),
        ("Slint", "GPL-3.0"),
        ("smoothie-rs", "GPL-3.0"),
        ("GitHub Primer", "MIT"),
        ("Battle Angel Alita (ctt art)", "Yukito Kishiro"),
    ]];
    // made when it first shows, a window that waits hidden is in the task bar of the desktop
    let made: std::cell::RefCell<Option<Sheet>> = Default::default();
    let weak = ui.as_weak();
    ui.on_show_sheet(move |heading| {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        let mut made = made.borrow_mut();
        let Some(sheet) = components::reuse(&mut made, || Sheet::new().ok()) else {
            return;
        };
        let columns: &[&[(&str, &str)]] = if heading == "Credits" {
            &CREDITS
        } else {
            &SHORTCUTS
        };
        // slint sorts the fields of the row by name, so the note comes first
        let column = |lines: &[(&str, &str)]| {
            let rows = lines.iter().map(|line| (line.1.into(), line.0.into()));
            ModelRc::new(VecModel::from_iter(rows))
        };
        sheet.set_left(column(columns[0]));
        sheet.set_right(column(columns.get(1).copied().unwrap_or_default()));
        sheet.set_note(match heading.as_str() {
            "Credits" => "See the licenses folder for all license texts.".into(),
            _ => "".into(),
        });
        sheet.set_heading(heading);
        themes::paint(&sheet.global::<Theme>(), &ui.get_theme(), ui.get_light());
        let _ = sheet.show();
        #[cfg(target_os = "linux")]
        {
            let sheet = sheet.as_weak();
            slint::Timer::single_shot(std::time::Duration::from_millis(200), move || {
                sheet.upgrade().and_then(|sheet| icon::set(sheet.window()));
            });
        }
    });
    services::start(ui.as_weak());
    let loaded = config::load(std::path::Path::new(""), true, &ui.get_prefs().preset);
    let state: Shared = Arc::new(Mutex::new(State {
        entries: Vec::new(),
        draft: Entry {
            saved: Saved {
                config: loaded.config,
                origin: loaded.entry,
                ..Saved::default()
            },
            ..Entry::default()
        },
        tracks: Tracks::default(),
        project: project::Project::default(),
        snap: false,
        ids: 0,
        held: None,
        touched: None,
        spare: None,
    }));
    fit(&ui, &state);
    select(&ui, &state, -1);

    let (weak, shared) = (ui.as_weak(), Arc::clone(&state));
    ui.on_select(move |index| {
        if let Some(ui) = weak.upgrade() {
            select(&ui, &shared, index);
        }
    });

    let (weak, shared) = (ui.as_weak(), Arc::clone(&state));
    ui.on_moded(move || {
        if let Some(ui) = weak.upgrade() {
            ui.set_transform(false);
            ui.set_playing(false);
            view::refresh(&ui, &shared);
        }
    });

    clips::wire(&ui, &state);
    edit::wire(&ui, &state);
    components::wire(&ui);
    update::wire(&ui);
    tracks::wire(&ui, &state);
    playback::wire(&ui, &state);
    settings::wire(&ui, &state);
    history::wire(&ui, &state);
    render::wire(&ui, &state);
    project::wire(&ui, &state);
    project::recents(&ui);
    project::schedule(&ui, &state);

    const QUESTIONS: [&str; 3] = ["render", "save", "project"];
    let question = |ui: &App, from: usize| {
        let open = |name: &str| match name {
            "render" => ui.get_rendering(),
            "save" => ui.get_unsaved(),
            _ => project::asks(ui),
        };
        QUESTIONS[from..]
            .iter()
            .find(|name| open(name))
            .copied()
            .unwrap_or("")
    };
    let weak = ui.as_weak();
    let close = move || {
        weak.upgrade().is_none_or(|ui| {
            let ask = question(&ui, 0);
            project::question(&ui, ask);
            if ask.is_empty() {
                settings::save_preferences(&ui);
            }
            ask.is_empty()
        })
    };
    let ask = close.clone();
    ui.window().on_close_requested(move || match ask() {
        true => slint::CloseRequestResponse::HideWindow,
        false => slint::CloseRequestResponse::KeepWindowShown,
    });
    ui.on_quit(move || {
        if close() {
            let _ = slint::quit_event_loop();
        }
    });
    let (weak, shared) = (ui.as_weak(), Arc::clone(&state));
    ui.on_close_answer(move |save| {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        let current = ui.get_closing();
        let Some(at) = QUESTIONS.iter().position(|name| current == *name) else {
            return project::answer(&ui, &shared, save);
        };
        match current.as_str() {
            "save" if save => {
                settings::save(&ui, &shared, None);
                if ui.get_failed() {
                    ui.set_closing("".into());
                    return;
                }
            }
            "project" if save => {
                ui.set_closing("".into());
                let quit = |_: &App, _: &Shared| drop(slint::quit_event_loop());
                return project::save(&ui, &shared, Some(Box::new(quit)));
            }
            "project" => project::clear(&ui, &shared),
            _ => {}
        }
        let next = question(&ui, at + 1);
        project::question(&ui, next);
        if next.is_empty() {
            let _ = slint::quit_event_loop();
        }
    });

    ui.on_reveal(|path| reveal(PathBuf::from(path.as_str())));

    ui.on_open(|url| services::open(url.as_str()));

    let (weak, shared) = (ui.as_weak(), Arc::clone(&state));
    ui.on_copy_details(move || {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        let (details, weak) = (details(&ui, &shared), weak.clone());
        services::task("details", move |_| {
            let details = details();
            let _ = weak.upgrade_in_event_loop(move |ui| {
                ui.invoke_copy(details.into());
                ui.set_copied(true);
                let weak = ui.as_weak();
                slint::Timer::single_shot(std::time::Duration::from_millis(1500), move || {
                    if let Some(ui) = weak.upgrade() {
                        ui.set_copied(false);
                    }
                });
            });
        });
    });

    let arguments: Vec<PathBuf> = std::env::args().skip(1).map(PathBuf::from).collect();
    let (projects, clips): (Vec<_>, Vec<_>) = arguments
        .into_iter()
        .partition(|path| project::is_project(path));
    let last = project::SESSION.with_borrow(|session| session.recent.first().cloned());
    if let Some(first) = projects.first() {
        project::open_path(&ui, &state, first.clone());
    } else if !clips.is_empty() || !project::recover(&ui) {
        let again = last.filter(|_| ui.get_prefs().reopen && ui.get_prefs().projects);
        if let Some(last) = again.filter(|last| clips.is_empty() && last.exists()) {
            project::open_path(&ui, &state, last);
        }
    }
    clips::import(ui.as_weak(), Arc::clone(&state), clips, None);
    #[cfg(target_os = "linux")]
    clips::drops(&ui, &state);
    ui.run()?;
    project::finish(&ui, &state);

    // the gpu libraries crash when the process exits under a running job
    let left = services::stop();
    debug_assert_eq!(left, 0);
    interpolini_core::shutdown();
    Ok(())
}

// the folder opens with the file selected
#[cfg(windows)]
fn reveal(path: PathBuf) {
    use std::os::windows::process::CommandExt;
    let select = format!("/select,\"{}\"", path.display());
    services::child(std::process::Command::new("explorer").raw_arg(select), None);
}

#[cfg(target_os = "macos")]
fn reveal(path: PathBuf) {
    services::child(std::process::Command::new("open").arg("-R").arg(path), None);
}

#[cfg(all(unix, not(target_os = "macos")))]
fn reveal(path: PathBuf) {
    use std::os::unix::ffi::OsStrExt;
    use std::process::{Command, Stdio};
    let unreserved = |byte: &u8| byte.is_ascii_alphanumeric() || b"-._~/".contains(byte);
    let uri: String = path
        .as_os_str()
        .as_bytes()
        .iter()
        .map(|byte| match unreserved(byte) {
            true => char::from(*byte).to_string(),
            false => format!("%{byte:02X}"),
        })
        .collect();
    // the file managers of linux select a file through this call, a folder opens when it fails
    let folder = path.parent().unwrap_or(&path).to_owned();
    let mut command = Command::new("dbus-send");
    command
        .args([
            "--session",
            "--print-reply",
            "--dest=org.freedesktop.FileManager1",
            "/org/freedesktop/FileManager1",
            "org.freedesktop.FileManager1.ShowItems",
        ])
        .arg(format!("array:string:file://{uri}"))
        .arg("string:")
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    services::child(&mut command, Some(folder));
}
