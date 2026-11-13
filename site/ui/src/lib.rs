use std::cell::{Cell, RefCell};
use std::time::Duration;

use i_slint_backend_winit::Backend;
use slint::winit_030::WinitWindowAccessor;
use slint::winit_030::winit::platform::web::WindowAttributesExtWebSys;
use slint::{Color, ComponentHandle, Global, PhysicalSize, Timer, TimerMode};
use wasm_bindgen::prelude::*;
use web_sys::HtmlCanvasElement;

mod demo;

slint::include_modules!();

const SUPERSAMPLE: f32 = 2.0;
const MAX_DENSITY: f32 = 4.0;
const REVEAL_EVERY: Duration = Duration::from_millis(200);
const REFIT_AFTER: Duration = Duration::from_millis(100);

struct Palette {
    void: u32,
    rack: u32,
    raised: u32,
    seam: u32,
    ink: u32,
    muted: u32,
    faint: u32,
    accent: u32,
    ok: u32,
}

const DARK: Palette = Palette {
    void: 0x0a0a0a,
    rack: 0x131313,
    raised: 0x1c1c1c,
    seam: 0x2a2a2a,
    ink: 0xececec,
    muted: 0x9c9c9c,
    faint: 0x8a8a8a,
    accent: 0xe8483a,
    ok: 0x6fbf8b,
};
const LIGHT: Palette = Palette {
    void: 0xf4f4f4,
    rack: 0xffffff,
    raised: 0xf0f0f0,
    seam: 0xd6d6d6,
    ink: 0x161616,
    muted: 0x5c5c5c,
    faint: 0x6e6e6e,
    accent: 0xc93426,
    ok: 0x1a7f37,
};

thread_local! {
    static CANVAS: RefCell<Option<HtmlCanvasElement>> = const { RefCell::new(None) };
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
    static DIALOG: RefCell<Option<Dialog>> = const { RefCell::new(None) };
    static SCENE: RefCell<String> = const { RefCell::new(String::new()) };
    static REDUCED: Cell<bool> = const { Cell::new(false) };
    static REVEAL: Timer = Timer::default();
}

fn color(rgb: u32) -> Color {
    Color::from_rgb_u8((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
}

fn paint<C: ComponentHandle>(component: &C, dark: bool)
where
    for<'a> Theme<'a>: Global<'a, C>,
{
    let palette = if dark { DARK } else { LIGHT };
    let theme = component.global::<Theme>();
    theme.set_void(color(palette.void));
    theme.set_rack(color(palette.rack));
    theme.set_raised(color(palette.raised));
    theme.set_seam(color(palette.seam));
    theme.set_ink(color(palette.ink));
    theme.set_muted(color(palette.muted));
    theme.set_faint(color(palette.faint));
    theme.set_accent(color(palette.accent));
    theme.set_ok(color(palette.ok));
}

fn calm<C: ComponentHandle>(component: &C, reduced: bool)
where
    for<'a> Theme<'a>: Global<'a, C>,
{
    component
        .global::<Theme>()
        .set_motion(if reduced { 0.0 } else { 1.0 });
}

fn paint_app(app: &App, dark: bool) {
    paint(app, dark);
    app.set_light(!dark);
}

fn number<T: std::str::FromStr>(value: &str) -> Result<T, JsValue> {
    value
        .parse()
        .map_err(|_| JsValue::from(format!("{value} is not a number")))
}

fn look(value: &str) -> Result<&'static str, JsValue> {
    match value {
        "2" => Ok("sharp"),
        "13" => Ok("standard"),
        "23" => Ok("smooth"),
        _ => Err(format!("{value} is not a look").into()),
    }
}

fn apply(settings: &mut Settings, key: &str, value: &str) -> Result<(), JsValue> {
    match key {
        "dedup.enabled" => settings.dedup = value == "yes",
        "dedup.repeats" => settings.dedup_repeats = number(value)?,
        "pre-interpolation.enabled" => settings.pre = value == "yes",
        "pre-interpolation.fps" => settings.pre_fps = value.into(),
        "interpolation.enabled" => settings.interpolate = value == "yes",
        "interpolation.engine" => settings.engine = value.into(),
        "interpolation.fps" => settings.interpolation_fps = value.into(),
        "interpolation.speed" => settings.speed = value.into(),
        "interpolation.tuning" => settings.tuning = value.into(),
        "interpolation.algorithm" => settings.algorithm = look(value)?.into(),
        "interpolation.half size" => settings.half = value == "yes",
        "rife.backend" => settings.backend = value.into(),
        "rife.model" => settings.model = value.into(),
        "mask.enabled" => settings.mask = value == "yes",
        "mask.method" => settings.mask_method = value.into(),
        "mask.peel limit" => settings.mask_peel = number(value)?,
        "mask.limit" => settings.mask_limit = number(value)?,
        "mask.edge" => settings.mask_edge = number(value)?,
        "mask.tolerance" => settings.mask_tolerance = number(value)?,
        "mask.image" => settings.mask_image = value.into(),
        "mask.soft edge" => settings.mask_soft = number(value)?,
        "blending.enabled" => settings.blend = value == "yes",
        "blending.fps" => settings.blend_fps = number(value)?,
        "blending.intensity" => settings.intensity = number(value)?,
        "blending.gamma" => settings.gamma = number(value)?,
        "blending.weighting" => settings.weighting = value.into(),
        "color.enabled" => settings.grade = value == "yes",
        "color.brightness" => settings.brightness = number(value)?,
        "color.contrast" => settings.contrast = number(value)?,
        "color.saturation" => settings.saturation = number(value)?,
        "color.hue" => settings.hue = number(value)?,
        "color.lut" => settings.lut = value.into(),
        "upscale.enabled" => settings.upscale = value == "yes",
        "upscale.resolution" => settings.resolution = value.into(),
        "upscale.method" => settings.method = value.into(),
        "output.encoder" => settings.encoder = value.into(),
        "output.target" => settings.target = value.into(),
        "output.quality" => settings.quality = number(value)?,
        _ => return Err(format!("unknown setting {key}").into()),
    }
    Ok(())
}

fn wake() -> Result<(), JsValue> {
    slint::invoke_from_event_loop(|| ()).map_err(|error| error.to_string().into())
}

#[wasm_bindgen(start)]
fn start() -> Result<(), JsValue> {
    let backend = Backend::builder()
        .with_renderer_name("femtovg")
        .with_spawn_event_loop(true)
        .with_window_attributes_hook(|attributes| {
            attributes
                .with_canvas(CANVAS.take())
                .with_active(false)
                .with_prevent_default(false)
        })
        .build()
        .map_err(|error| error.to_string())?;
    slint::platform::set_platform(Box::new(backend)).map_err(|error| error.to_string())?;
    slint::run_event_loop_until_quit().map_err(|error| error.to_string())?;
    Ok(())
}

fn canvas(id: &str) -> Result<HtmlCanvasElement, String> {
    web_sys::window()
        .and_then(|window| window.document())
        .and_then(|document| document.get_element_by_id(id))
        .and_then(|element| element.dyn_into::<HtmlCanvasElement>().ok())
        .ok_or_else(|| format!("no canvas with id {id}"))
}

fn when_created<C: ComponentHandle + 'static>(component: &C, ready: fn()) -> Result<(), JsValue> {
    let weak = component.as_weak();
    slint::spawn_local(async move {
        if let Some(component) = weak.upgrade()
            && component.window().winit_window().await.is_ok()
        {
            ready();
        }
    })
    .map(drop)
    .map_err(|error| error.to_string().into())
}

#[wasm_bindgen]
pub fn mount(canvas_id: &str, thumbs: &[u8], peaks: &[u8], frames: &[u8]) -> Result<(), JsValue> {
    CANVAS.set(Some(canvas(canvas_id)?));
    let app = demo::app(thumbs, peaks, frames).map_err(|error| error.to_string())?;
    paint_app(&app, true);
    app.show().map_err(|error| error.to_string())?;
    let created = when_created(&app, fit);
    APP.set(Some(app));
    created
}

#[wasm_bindgen]
pub fn mount_dialog(canvas_id: &str) -> Result<(), JsValue> {
    CANVAS.set(Some(canvas(canvas_id)?));
    let window = demo::dialog().map_err(|error| error.to_string())?;
    paint(&window, true);
    calm(&window, REDUCED.get());
    SCENE.with_borrow(|scene| demo::tab(&window, scene));
    window.show().map_err(|error| error.to_string())?;
    let created = when_created(&window, fit);
    DIALOG.set(Some(window));
    created
}

#[wasm_bindgen]
pub fn unmount() -> Result<(), JsValue> {
    if let Some(app) = APP.take() {
        app.hide().map_err(|error| error.to_string())?;
    }
    if let Some(window) = DIALOG.take() {
        window.hide().map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn size<C: ComponentHandle>(component: &C, logical: (u32, u32), density: f32) {
    let window = component.window();
    window.dispatch_event(slint::platform::WindowEvent::ScaleFactorChanged {
        scale_factor: density,
    });
    window.set_size(PhysicalSize::new(
        (logical.0 as f32 * density) as u32,
        (logical.1 as f32 * density) as u32,
    ));
}

#[wasm_bindgen]
pub fn fit() {
    let ratio = web_sys::window().map_or(1.0, |window| window.device_pixel_ratio() as f32);
    let density = (SUPERSAMPLE * ratio).min(MAX_DENSITY);
    APP.with_borrow(|app| app.iter().for_each(|app| size(app, demo::WINDOW, density)));
    DIALOG.with_borrow(|window| {
        window
            .iter()
            .for_each(|window| size(window, demo::DIALOG, density))
    });
}

#[wasm_bindgen]
pub fn open(section: &str) -> Result<(), JsValue> {
    APP.with_borrow(|app| app.as_ref().map(|app| demo::open(app, section)));
    // the panel can get its layout after the first reveal
    REVEAL.with(|timer| {
        if demo::REVEALED.contains(&section) {
            timer.start(TimerMode::Repeated, REVEAL_EVERY, || {
                APP.with_borrow(|app| app.as_ref().map(demo::reveal));
            });
        } else {
            timer.stop();
        }
    });
    wake()
}

#[wasm_bindgen]
pub fn scene(name: &str) -> Result<(), JsValue> {
    SCENE.set(name.into());
    APP.with_borrow(|app| app.as_ref().map(|app| demo::scene(app, name)).transpose())?;
    DIALOG.with_borrow(|window| window.iter().for_each(|window| demo::tab(window, name)));
    // a scene can change the minimum size, and slint then resizes the window from a stale size
    Timer::single_shot(REFIT_AFTER, fit);
    wake()
}

#[wasm_bindgen]
pub fn theme(dark: bool) -> Result<(), JsValue> {
    APP.with_borrow(|app| app.iter().for_each(|app| paint_app(app, dark)));
    DIALOG.with_borrow(|window| window.iter().for_each(|window| paint(window, dark)));
    wake()
}

#[wasm_bindgen]
pub fn motion(reduced: bool) -> Result<(), JsValue> {
    REDUCED.set(reduced);
    APP.with_borrow(|app| {
        app.iter().for_each(|app| {
            calm(app, reduced);
            app.global::<System>().set_smooth_scroll(!reduced);
        })
    });
    DIALOG.with_borrow(|window| window.iter().for_each(|window| calm(window, reduced)));
    wake()
}

#[wasm_bindgen]
pub fn configure(
    settings: Vec<String>,
    weights: &[f32],
    source: f32,
    output: f32,
    source_name: String,
    seconds: f32,
) -> Result<(), JsValue> {
    let mut changed = demo::defaults();
    for entry in &settings {
        let applied = entry
            .split_once('=')
            .ok_or_else(|| JsValue::from(format!("{entry} is not key=value")))
            .and_then(|(key, value)| apply(&mut changed, key, value));
        if let Err(error) = applied {
            web_sys::console::error_1(&error);
        }
    }
    APP.with_borrow(|app| {
        if let Some(app) = app {
            app.set_settings(changed);
            app.set_rate(format!("{source} to {output} fps").into());
            app.set_details(format!("1920x1080  |  {source} to {output} fps").into());
            demo::source(app, &source_name, seconds);
            demo::set_weights(app, weights);
        }
    });
    wake()
}

#[wasm_bindgen]
pub fn position(seconds: f32) -> Result<(), JsValue> {
    APP.with_borrow(|app| {
        if let Some(app) = app {
            demo::position(app, seconds);
        }
    });
    wake()
}

#[wasm_bindgen]
pub fn thumbnails(bytes: &[u8]) -> Result<(), JsValue> {
    APP.with_borrow(|app| {
        if let Some(app) = app {
            demo::set_thumbnails(app, bytes);
        }
    });
    wake()
}
