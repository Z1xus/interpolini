use std::path::{Path, PathBuf};
use std::sync::Arc;

use interpolini_core::config::{
    self, Algorithm, Backend, Codec, Config, Container, Effort, Encoder, Engine, Masking, Method,
    Rate, Resolution, Speed, Target, Tuning, Weighting,
};
use interpolini_core::{budget, words};
use slint::{ComponentHandle, Model, ModelRc, VecModel};

use crate::clips::pick;
use crate::history::record;
use crate::project::SESSION;
use crate::state::{Shared, State, lock};
use crate::view::{label, models, refresh, select, selected, unsaved};
use crate::{App, Code, Preferences, Prefs, Settings, themes};

const LOOKS: [(Algorithm, &str); 3] = [
    (Algorithm::Sharp, "sharp"),
    (Algorithm::Standard, "standard"),
    (Algorithm::Smooth, "smooth"),
];

pub fn settings(config: &Config) -> Settings {
    let Config {
        dedup,
        pre,
        interpolation,
        rife,
        mask,
        blending,
        color,
        upscale,
        output,
        ..
    } = config;
    let look = LOOKS.iter().find(|look| look.0 == interpolation.algorithm);
    Settings {
        dedup: dedup.enabled,
        dedup_repeats: dedup.repeats,
        pre: pre.enabled,
        pre_fps: pre.fps.to_string().into(),
        interpolate: interpolation.enabled,
        engine: interpolation.engine.name().into(),
        interpolation_fps: interpolation.fps.to_string().into(),
        speed: interpolation.speed.name().into(),
        tuning: interpolation.tuning.name().into(),
        algorithm: look.map_or("", |look| look.1).into(),
        half: interpolation.half,
        backend: rife.backend.name().into(),
        model: rife.model.as_str().into(),
        mask: mask.enabled,
        mask_method: mask.method.name().into(),
        mask_peel: mask.peel,
        mask_limit: mask.limit,
        mask_edge: mask.edge,
        mask_tolerance: mask.tolerance,
        blend: blending.enabled,
        blend_fps: blending.fps as i32,
        intensity: blending.intensity,
        gamma: blending.gamma,
        weighting: blending.weighting.name().into(),
        custom: config::list(&blending.custom).into(),
        grade: color.enabled,
        brightness: color.brightness,
        contrast: color.contrast,
        saturation: color.saturation,
        hue: color.hue,
        lut: color.lut.as_str().into(),
        upscale: upscale.enabled,
        resolution: upscale.resolution.name().into(),
        method: upscale.method.name().into(),
        codec: output.codec.name().into(),
        encoder: output.encoder.name().into(),
        target: output.target.name().into(),
        quality: output.quality as f32,
        size: output.size as i32,
        effort: output.effort.name().into(),
        container: output.container.name().into(),
        audio: output.audio.name().into(),
        options: output.options.as_str().into(),
    }
}

pub fn apply(settings: &Settings, config: &mut Config) {
    let Config {
        dedup,
        pre,
        interpolation,
        rife,
        mask,
        blending,
        color,
        upscale,
        output,
        ..
    } = config;
    let look = LOOKS.iter().find(|look| settings.algorithm == look.1);
    dedup.enabled = settings.dedup;
    dedup.repeats = settings.dedup_repeats;
    pre.enabled = settings.pre;
    pre.fps = Rate::parse(&settings.pre_fps).unwrap_or(pre.fps);
    interpolation.enabled = settings.interpolate;
    interpolation.engine = Engine::parse(&settings.engine).unwrap_or(interpolation.engine);
    interpolation.fps = Rate::parse(&settings.interpolation_fps).unwrap_or(interpolation.fps);
    interpolation.speed = Speed::parse(&settings.speed).unwrap_or(interpolation.speed);
    interpolation.tuning = Tuning::parse(&settings.tuning).unwrap_or(interpolation.tuning);
    interpolation.algorithm = look.map_or(interpolation.algorithm, |look| look.0);
    interpolation.half = settings.half;
    rife.backend = Backend::parse(&settings.backend).unwrap_or(rife.backend);
    rife.model = settings.model.to_string();
    mask.enabled = settings.mask;
    mask.method = Masking::parse(&settings.mask_method).unwrap_or(mask.method);
    mask.peel = settings.mask_peel;
    mask.limit = settings.mask_limit;
    mask.edge = settings.mask_edge;
    mask.tolerance = settings.mask_tolerance;
    blending.enabled = settings.blend;
    blending.fps = settings.blend_fps as u32;
    blending.intensity = settings.intensity;
    blending.gamma = settings.gamma;
    blending.weighting = Weighting::parse(&settings.weighting).unwrap_or(blending.weighting);
    if let Some(custom) = config::curve(&settings.custom) {
        blending.custom = custom;
    }
    color.enabled = settings.grade;
    color.brightness = settings.brightness;
    color.contrast = settings.contrast;
    color.saturation = settings.saturation;
    color.hue = settings.hue;
    color.lut = settings.lut.to_string();
    upscale.enabled = settings.upscale;
    upscale.resolution = Resolution::parse(&settings.resolution).unwrap_or(upscale.resolution);
    upscale.method = Method::parse(&settings.method).unwrap_or(upscale.method);
    output.codec = Codec::parse(&settings.codec).unwrap_or(output.codec);
    output.encoder = Encoder::parse(&settings.encoder).unwrap_or(output.encoder);
    output.target = Target::parse(&settings.target).unwrap_or(output.target);
    output.quality = settings.quality as u32;
    output.size = settings.size as u32;
    output.effort = Effort::parse(&settings.effort).unwrap_or(output.effort);
    output.container = Container::parse(&settings.container).unwrap_or(output.container);
    output.audio = config::Audio::parse(&settings.audio).unwrap_or(output.audio);
    output.options = settings.options.to_string();
}

pub fn report(ui: &App, result: std::io::Result<()>, done: &str) {
    ui.set_failed(result.is_err());
    if let Err(error) = result {
        ui.set_status(format!("Couldn't save: {error}").into());
        return;
    }
    ui.set_status("".into());
    ui.set_done(done.into());
    let weak = ui.as_weak();
    slint::Timer::single_shot(std::time::Duration::from_millis(1500), move || {
        if let Some(ui) = weak.upgrade() {
            ui.set_done("".into());
        }
    });
}

pub fn save(ui: &App, state: &Shared, name: Option<&str>) {
    let mut guard = lock(state);
    let entry = guard.current(ui);
    let target = match (name, &entry.origin) {
        (None, Some(origin)) => origin.clone(),
        (name, _) => {
            let name = name.unwrap_or("default").trim().to_lowercase();
            let allowed = |letter: &char| letter.is_alphanumeric() || "-_ ".contains(*letter);
            let name: String = name.chars().filter(allowed).collect();
            if name.is_empty() {
                return;
            }
            config::Entry {
                path: config::path(&config::app_dir(), &name),
                name,
                local: false,
            }
        }
    };
    let result = std::fs::write(&target.path, entry.config.to_ini());
    if result.is_ok() {
        entry.origin = Some(target);
    }
    drop(guard);
    select(ui, state, ui.get_selected());
    report(ui, result, "Saved");
}

pub fn preferences() -> PathBuf {
    config::app_dir().join("interpolini-app.ini")
}

pub fn save_preferences(ui: &App) {
    let yes = |value: bool| if value { "yes" } else { "no" };
    let sections = ui.get_sections();
    let open = [
        ("dedup", sections.dedup),
        ("pre", sections.pre),
        ("interpolation", sections.interpolation),
        ("masking", sections.masking),
        ("blending", sections.blending),
        ("color", sections.color),
        ("upscale", sections.upscale),
        ("output", sections.output),
    ];
    let open: Vec<&str> = open
        .iter()
        .filter(|open| open.1)
        .map(|open| open.0)
        .collect();
    let prefs = ui.get_prefs();
    let mut text = format!(
        "theme: {}\nlight: {}\nseparate: {}\nrail: {}\ndeck: {}\npreview: {}\nresolution: {}\nupdates: {}\nvolume: {}\nsections: {}\nprojects: {}\nautosave: {}\nasksave: {}\nreopen: {}\nconfirm: {}\nmemory: {}\nfolder: {}\npreset: {}\n",
        ui.get_theme(),
        yes(ui.get_light()),
        yes(ui.get_separate()),
        ui.get_rail(),
        ui.get_deck(),
        yes(!ui.get_preview_off()),
        ui.get_resolution(),
        yes(ui.get_updates()),
        ui.get_volume(),
        open.join(" "),
        yes(prefs.projects),
        prefs.autosave,
        yes(prefs.ask_save),
        yes(prefs.reopen),
        yes(prefs.confirm_delete),
        prefs.memory,
        prefs.folder,
        prefs.preset,
    );
    SESSION.with_borrow(|session| {
        for path in &session.recent {
            text.push_str(&format!("recent: {}\n", path.display()));
        }
        if let Some(path) = &session.recovery {
            text.push_str(&format!("recovery: {}\n", path.display()));
        }
    });
    let _ = std::fs::write(preferences(), text);
}

pub fn load_prefs(ui: &App, saved: &str) {
    let value = |key: &str| saved.lines().find_map(|line| line.strip_prefix(key));
    let lines = |key: &'static str| saved.lines().filter_map(move |line| line.strip_prefix(key));
    let number = |key: &str, other: i32| {
        value(key)
            .and_then(|text| text.parse().ok())
            .unwrap_or(other)
    };
    ui.set_prefs(Prefs {
        projects: value("projects: ") == Some("yes"),
        autosave: number("autosave: ", 5).clamp(0, 60),
        ask_save: value("asksave: ") != Some("no"),
        reopen: value("reopen: ") == Some("yes"),
        confirm_delete: value("confirm: ") == Some("yes"),
        memory: number("memory: ", 0).max(0),
        folder: value("folder: ").unwrap_or_default().into(),
        preset: value("preset: ").unwrap_or("default").into(),
    });
    SESSION.with_borrow_mut(|session| {
        session.recent = lines("recent: ").map(PathBuf::from).collect();
        session.recovery = value("recovery: ").map(PathBuf::from);
    });
    limit(ui);
}

pub fn presets() -> ModelRc<slint::SharedString> {
    let found = config::entries(Path::new(""));
    let names = found.into_iter().filter(|entry| !entry.local);
    ModelRc::new(VecModel::from_iter(
        names.map(|entry| slint::SharedString::from(entry.name)),
    ))
}

fn limit(ui: &App) {
    budget::limit(ui.get_prefs().memory as usize * (1 << 20));
}

pub fn attach(ui: &App, window: &Preferences) {
    let (weak, shown) = (ui.as_weak(), window.as_weak());
    window.on_changed(move || {
        if let (Some(ui), Some(window)) = (weak.upgrade(), shown.upgrade()) {
            ui.set_prefs(window.get_prefs());
            ui.set_updates(window.get_updates());
            ui.invoke_prefs_changed();
        }
    });
    let shown = window.as_weak();
    window.on_choose_folder(move || {
        let shown = shown.clone();
        let _ = slint::spawn_local(async move {
            let folder = rfd::AsyncFileDialog::new().pick_folder().await;
            if let (Some(folder), Some(window)) = (folder, shown.upgrade()) {
                let mut prefs = window.get_prefs();
                prefs.folder = folder.path().to_string_lossy().as_ref().into();
                window.set_prefs(prefs);
                window.invoke_changed();
            }
        });
    });
}

pub fn smooth_scroll() -> bool {
    // kde keeps this in kdeglobals and no other desktop has the setting
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let text = home.and_then(|home| std::fs::read_to_string(home.join(".config/kdeglobals")).ok());
    !text.is_some_and(|text| text.lines().any(|line| line.trim() == "SmoothScroll=false"))
}

pub fn wire(ui: &App, state: &Shared) {
    let weak = ui.as_weak();
    ui.on_resized(move || {
        if let Some(ui) = weak.upgrade() {
            save_preferences(&ui);
        }
    });

    let (weak, shared) = (ui.as_weak(), Arc::clone(state));
    ui.on_changed(move || {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        let mut state = lock(&shared);
        record(&ui, &mut state, true);
        let entry = state.current(&ui);
        apply(&ui.get_settings(), &mut entry.config);
        models(&ui, &mut entry.config.rife);
        // a value that is not valid goes back to the last good one
        ui.set_settings(settings(&entry.config));
        ui.set_unsaved(unsaved(entry));
        let chosen = crate::edit::chosen(&ui, &state);
        let config = state.current(&ui).config.clone();
        let videos = state
            .entries
            .iter_mut()
            .filter(|entry| entry.audio.is_none());
        for other in videos.filter(|entry| chosen.contains(&entry.link)) {
            other.config = config.clone();
        }
        let entry = state.current(&ui);
        if selected(&ui).is_none() || ui.get_scope() == "all clips" {
            let config = entry.config.clone();
            let State { entries, draft, .. } = &mut *state;
            let videos = entries.iter_mut().filter(|entry| entry.audio.is_none());
            for other in videos.chain([draft]) {
                other.config = config.clone();
            }
        }
        drop(state);
        refresh(&ui, &shared);
    });

    let weak = ui.as_weak();
    ui.on_choose_lut(move || {
        pick(&weak, "LUT", &["cube"], |ui, files| {
            let mut settings = ui.get_settings();
            settings.lut = files[0].to_string_lossy().as_ref().into();
            settings.grade = true;
            ui.set_settings(settings);
            ui.invoke_changed();
        });
    });

    let (weak, shared) = (ui.as_weak(), Arc::clone(state));
    ui.on_choose_config(move |name| {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        let mut state = lock(&shared);
        let entry = state.current(&ui);
        let found = config::entries(&entry.clip);
        if let Some(found) = found
            .into_iter()
            .find(|entry| label(entry) == name.as_str())
        {
            let loaded = config::read(found);
            entry.config = loaded.config;
            entry.origin = loaded.entry;
        }
        drop(state);
        select(&ui, &shared, ui.get_selected());
    });

    let (weak, shared) = (ui.as_weak(), Arc::clone(state));
    ui.on_save_config(move || {
        if let Some(ui) = weak.upgrade() {
            save(&ui, &shared, None);
        }
    });

    let (weak, shared) = (ui.as_weak(), Arc::clone(state));
    ui.on_new_config(move |name| {
        if let Some(ui) = weak.upgrade() {
            save(&ui, &shared, Some(&name));
        }
    });

    let (weak, shared) = (ui.as_weak(), Arc::clone(state));
    ui.on_delete_config(move || {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        let mut state = lock(&shared);
        let entry = state.current(&ui);
        let Some(origin) = entry.origin.take() else {
            return;
        };
        let result = std::fs::remove_file(&origin.path);
        let loaded = config::load(&entry.clip, false, "default");
        entry.config = loaded.config;
        entry.origin = loaded.entry;
        drop(state);
        select(&ui, &shared, ui.get_selected());
        report(&ui, result, "Deleted");
    });

    let weak = ui.as_weak();
    ui.on_choose_theme(move |name| {
        if let Some(ui) = weak.upgrade() {
            ui.set_light_available(themes::has_light(&name));
            themes::apply(&ui, &name, ui.get_light());
            save_preferences(&ui);
        }
    });

    let weak = ui.as_weak();
    ui.on_preview_theme(move |name| {
        if let Some(ui) = weak.upgrade() {
            themes::apply(&ui, &name, ui.get_light());
        }
    });

    let (weak, shared) = (ui.as_weak(), Arc::clone(state));
    ui.on_prefs_changed(move || {
        if let Some(ui) = weak.upgrade() {
            limit(&ui);
            crate::project::schedule(&ui, &shared);
            save_preferences(&ui);
        }
    });

    let weak = ui.as_weak();
    ui.on_save_preferences(move || {
        if let Some(ui) = weak.upgrade() {
            save_preferences(&ui);
        }
    });

    let code = ui.global::<Code>();
    let (weak, shared) = (ui.as_weak(), Arc::clone(state));
    code.on_share(move || {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        let mut config = lock(&shared).current(&ui).config.clone();
        let words = words::encode(&config).into_iter().map(Into::into);
        let code = ui.global::<Code>();
        code.set_words(ModelRc::new(VecModel::from_iter(words)));
        let lut = !config.color.lut.is_empty();
        config.color.lut.clear();
        let about = match words::changes(&Config::default(), &config).len() {
            0 => "Same as default".to_owned(),
            1 => "1 change from default".to_owned(),
            count => format!("{count} changes from default"),
        };
        let about = if lut {
            about + ", without the LUT"
        } else {
            about
        };
        code.set_about(about.into());
    });

    let weak = ui.as_weak();
    code.on_copy(move || {
        if let Some(ui) = weak.upgrade() {
            let words: Vec<String> = ui
                .global::<Code>()
                .get_words()
                .iter()
                .map(Into::into)
                .collect();
            ui.invoke_copy(words.join(" ").into());
        }
    });

    let (weak, shared) = (ui.as_weak(), Arc::clone(state));
    code.on_check(move |text| {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        let (result, valid, bad) = match decoded(&ui, &shared, &text) {
            _ if text.trim().is_empty() => (String::new(), false, false),
            Err(words::Wrong::Typing) => (String::new(), false, false),
            Err(words::Wrong::Short) => ("Not a full code yet".to_owned(), false, false),
            Err(words::Wrong::Word(word)) => (format!("\"{word}\" isn't a code word"), false, true),
            Err(words::Wrong::Code) => (
                "This code doesn't work. Check the words.".to_owned(),
                false,
                true,
            ),
            Ok((current, config)) => {
                let changes: Vec<String> = words::changes(&current, &config)
                    .into_iter()
                    .map(|(section, key, value)| change(section, key, &value))
                    .collect();
                let listed = match changes.len() {
                    0..=8 => changes.join(", "),
                    count => format!("{} and {} more", changes[..6].join(", "), count - 6),
                };
                let clip = selected(&ui).is_some() && ui.get_scope() == "this clip";
                let place = if clip { " on this clip" } else { "" };
                let model = &config.rife.model;
                let rife = config.interpolation.enabled
                    && config.interpolation.engine == Engine::Rife
                    || config.pre.enabled;
                let missing = rife && !config::models(config.rife.backend).contains(model);
                let missing = if missing {
                    format!(". {model} isn't installed.")
                } else {
                    String::new()
                };
                match changes.len() {
                    0 => ("Matches your settings".to_owned(), false, false),
                    _ => (format!("Sets {listed}{place}{missing}"), true, false),
                }
            }
        };
        let code = ui.global::<Code>();
        code.set_bad(bad);
        code.set_result(result.into());
        code.set_valid(valid);
    });

    let (weak, shared) = (ui.as_weak(), Arc::clone(state));
    code.on_apply(move |text| {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        if let Ok((_, config)) = decoded(&ui, &shared, &text) {
            record(&ui, &mut lock(&shared), false);
            ui.set_settings(settings(&config));
            ui.invoke_changed();
        }
    });
}

fn decoded(ui: &App, state: &Shared, text: &str) -> Result<(Config, Config), words::Wrong> {
    let mut config = words::decode(text)?;
    let current = lock(state).current(ui).config.clone();
    config.color.lut = current.color.lut.clone();
    Ok((current, config))
}

fn change(section: &str, key: &str, value: &str) -> String {
    let on = if value == "yes" { "on" } else { "off" };
    match (section, key) {
        ("dedup", "enabled") => format!("dedup {on}"),
        ("dedup", "repeats") => format!("repeats replaced {value}%"),
        ("pre-interpolation", "enabled") => format!("pre-interpolation {on}"),
        ("pre-interpolation", "fps") => format!("pre-interpolation {value} fps"),
        ("interpolation", "enabled") => format!("interpolation {on}"),
        ("interpolation", "fps") => format!("interpolation {value} fps"),
        ("interpolation", "algorithm") => {
            let look = LOOKS.iter().find(|look| look.0.name() == value);
            format!("look {}", look.map_or(value, |look| look.1))
        }
        ("interpolation", "half size") => format!("half-size analysis {on}"),
        ("mask", "enabled") => format!("masking {on}"),
        ("mask", "method") => format!("masking {value}"),
        ("mask", "peel limit") => format!("peel limit {value}"),
        ("blending", "enabled") => format!("blending {on}"),
        ("blending", "fps") => format!("blending {value} fps"),
        ("color", "enabled") => format!("color {on}"),
        ("upscale", "enabled") => format!("upscale {on}"),
        ("upscale", "method") => format!("upscale {value}"),
        ("output", "size") => format!("size {value} MB"),
        ("output", "audio") => format!("audio tracks {value}"),
        ("output", "options") if value.is_empty() => "no options".to_owned(),
        (_, key) => format!("{key} {value}"),
    }
}
