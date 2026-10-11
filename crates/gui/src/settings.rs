use std::path::PathBuf;
use std::sync::Arc;

use interpolini_core::config::{
    self, Algorithm, Backend, Codec, Config, Container, Encoder, Engine, Method, Rate, Resolution,
    Speed, Tuning, Weighting,
};
use slint::ComponentHandle;

use crate::clips::pick;
use crate::history::record;
use crate::state::{Shared, State, lock};
use crate::view::{label, models, refresh, select, selected, unsaved};
use crate::{App, Settings, themes};

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
    } = config;
    let look = LOOKS.iter().find(|look| look.0 == interpolation.algorithm);
    Settings {
        dedup: dedup.enabled,
        dedup_threshold: dedup.threshold,
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
        quality: output.quality as f32,
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
    } = config;
    let look = LOOKS.iter().find(|look| settings.algorithm == look.1);
    dedup.enabled = settings.dedup;
    dedup.threshold = settings.dedup_threshold;
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
    output.quality = settings.quality as u32;
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
    let text = format!(
        "theme: {}\nlight: {}\nseparate: {}\nrail: {}\ndeck: {}\nupdates: {}\nvolume: {}\n",
        ui.get_theme(),
        yes(ui.get_light()),
        yes(ui.get_separate()),
        ui.get_rail(),
        ui.get_deck(),
        yes(ui.get_updates()),
        ui.get_volume()
    );
    let _ = std::fs::write(preferences(), text);
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
        // without a selection, or with the scope on all clips, every clip gets these settings
        // with more clips selected, each of them gets these settings
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

    let weak = ui.as_weak();
    ui.on_save_preferences(move || {
        if let Some(ui) = weak.upgrade() {
            save_preferences(&ui);
        }
    });
}
