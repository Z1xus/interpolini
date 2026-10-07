#![windows_subsystem = "windows"]

mod clips;
mod components;
#[cfg(target_os = "linux")]
mod drop;
mod edit;
mod ffmpeg;
mod history;
#[cfg(target_os = "linux")]
mod icon;
mod playback;
mod preview;
mod render;
mod settings;
mod sound;
mod state;
mod themes;
mod tracks;
mod view;

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use interpolini_core::config::{self, Config};
use slint::{ComponentHandle, ModelRc, VecModel};

use settings::{preferences, settings, smooth_scroll};
use state::{Entry, Shared, State, Tracks, lock};
use view::{fit, select};

slint::include_modules!();

const VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), " (", env!("COMMIT"), ")");

fn details(ui: &App, state: &Shared) -> String {
    let config = lock(state).current(ui).config.to_ini();
    let about = interpolini_core::about();
    let (status, clip) = (ui.get_status(), ui.get_details());
    format!("interpolini {VERSION}\n{about}\nclip: {clip}\nstatus: {status}\n\n{config}")
}

fn main() -> Result<(), slint::PlatformError> {
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
    let theme = value("theme: ").unwrap_or("interpolini");
    ui.set_theme(theme.into());
    ui.set_light(value("light: ") == Some("yes"));
    ui.set_separate(value("separate: ") == Some("yes"));
    let size = |key: &str| value(key).and_then(|size| size.parse::<f32>().ok());
    ui.set_rail(size("rail: ").unwrap_or(340.0));
    ui.set_deck(size("deck: ").unwrap_or(0.0));
    ui.set_light_available(themes::has_light(theme));
    themes::apply(&ui, theme, ui.get_light());

    const SHORTCUTS: [(&str, &str); 19] = [
        ("Play or pause", "Space, K"),
        ("Back or forward 1 second", "J, L"),
        ("Step 1 frame", "Left, Right"),
        ("Move 1 second", "Shift Left, Right"),
        ("Set start or end", "I, O"),
        ("Clear start and end", "X"),
        ("Split", "S"),
        ("Remove clip", "Delete"),
        ("Undo, redo", "Ctrl Z, Ctrl Y"),
        ("Zoom the timeline", "Ctrl wheel, Ctrl + and -"),
        ("Scroll the timeline", "Shift wheel"),
        ("Track height", "Alt wheel"),
        ("Zoom the preview", "Ctrl wheel"),
        ("Move the preview", "Middle button"),
        ("Stretch", "Shift, drag an edge"),
        ("Reset the transform", "Double click"),
        ("Separate video and audio", "Alt, drag"),
        ("Select more clips", "Ctrl click"),
        ("Go to start or end", "Home, End"),
    ];
    const CREDITS: [(&str, &str); 12] = [
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
    ];
    // made when it first shows, a window that waits hidden is in the task bar of the desktop
    let made: std::cell::RefCell<Option<Sheet>> = Default::default();
    let weak = ui.as_weak();
    ui.on_show_sheet(move |heading| {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        let mut made = made.borrow_mut();
        if made.is_none() {
            *made = Sheet::new().ok();
        }
        let Some(sheet) = made.as_ref() else {
            return;
        };
        let lines: &[(&str, &str)] = if heading == "Credits" {
            &CREDITS
        } else {
            &SHORTCUTS
        };
        // slint sorts the fields of the row by name, so the note comes first
        let rows = lines.iter().map(|line| (line.1.into(), line.0.into()));
        sheet.set_rows(ModelRc::new(VecModel::from_iter(rows)));
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
    let (preview, previewer) = preview::spawn(ui.as_weak());
    let loaded = config::load(std::path::Path::new(""), true, "default");
    let state: Shared = Arc::new(Mutex::new(State {
        entries: Vec::new(),
        draft: Entry {
            id: 0,
            link: 0,
            clip: PathBuf::new(),
            audio: None,
            streams: 0,
            config: loaded.config,
            origin: loaded.entry,
            cut: (0.0, 1.0),
            seconds: 0.0,
            fps: 0.0,
            at: 0.0,
            track: 0,
            still: false,
            repeats: 0.0,
            measured: None,
            size: (0, 0),
            place: interpolini_core::Place::default(),
        },
        tracks: Tracks {
            video: vec![64.0],
            audio: vec![(36.0, false), (36.0, false)],
        },
        preview: Some(preview),
        ids: 0,
        held: None,
        touched: None,
        spare: None,
        cancel: Arc::default(),
        render: None,
    }));
    fit(&ui, &state);
    select(&ui, &state, -1);

    let (weak, shared) = (ui.as_weak(), Arc::clone(&state));
    ui.on_select(move |index| {
        if let Some(ui) = weak.upgrade() {
            select(&ui, &shared, index);
        }
    });

    clips::wire(&ui, &state);
    edit::wire(&ui, &state);
    components::wire(&ui);
    tracks::wire(&ui, &state);
    playback::wire(&ui, &state);
    settings::wire(&ui, &state);
    history::wire(&ui, &state);
    render::wire(&ui, &state);

    // a render that runs and settings that are not saved stop the close, and the window asks about them
    let question = |ui: &App| match (ui.get_rendering(), ui.get_unsaved()) {
        (true, _) => "render",
        (false, true) => "save",
        (false, false) => "",
    };
    let weak = ui.as_weak();
    let close = move || {
        let ask = weak.upgrade().map_or("", |ui| {
            ui.set_closing(question(&ui).into());
            question(&ui)
        });
        ask.is_empty()
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
        // after the question about the render comes the one about the settings
        if ui.get_closing() == "render" && ui.get_unsaved() {
            ui.set_closing("save".into());
            return;
        }
        if save {
            settings::save(&ui, &shared, None);
            if ui.get_failed() {
                ui.set_closing("".into());
                return;
            }
        }
        let _ = slint::quit_event_loop();
    });

    let (weak, shared) = (ui.as_weak(), Arc::clone(&state));
    ui.on_copy_details(move || {
        if let Some(ui) = weak.upgrade() {
            ui.invoke_copy(details(&ui, &shared).into());
            ui.set_copied(true);
            let weak = ui.as_weak();
            slint::Timer::single_shot(std::time::Duration::from_millis(1500), move || {
                if let Some(ui) = weak.upgrade() {
                    ui.set_copied(false);
                }
            });
        }
    });

    let clips = std::env::args().skip(1).map(PathBuf::from).collect();
    clips::import(ui.as_weak(), Arc::clone(&state), clips, None);
    #[cfg(target_os = "linux")]
    let _pump = clips::drops(&ui, &state);
    ui.run()?;

    // the gpu libraries crash when the process exits under a running job
    let render = {
        let mut state = lock(&state);
        state.cancel.store(true, Ordering::Relaxed);
        state.preview = None;
        state.render.take()
    };
    for thread in render.into_iter().chain([previewer]) {
        let _ = thread.join();
    }
    Ok(())
}
