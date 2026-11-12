use std::cell::RefCell;

use slint::platform::{Key, PointerEventButton, WindowEvent};
use slint::{
    ComponentHandle, Image, Model, ModelRc, PlatformError, Rgba8Pixel, SharedPixelBuffer,
    SharedString, VecModel,
};

use crate::{
    App, Clip, Code, Dialog, Download, Ghost, Lane, MaskTile, Masks, Mode, Prefs, Recent, Reveal,
    Sections, Settings, Tile,
};

pub const WINDOW: (u32, u32) = (1280, 760);
pub const DIALOG: (u32, u32) = (520, 443);
const THUMB: (u32, u32) = (160, 90);
const FRAME: (u32, u32) = (1280, 720);
const WAVE_ROWS: u32 = 64;
const MODELS: [&str; 6] = [
    "rife-v4.6",
    "rife-v4.15-lite",
    "rife-v4.22-lite",
    "rife-v4.25",
    "rife-v4.25-lite",
    "rife-v4.26",
];
const CLIP: &str = "240 (1).mp4";
const SOURCE_SECONDS: f32 = 1387.0 / 240.0;
const PLAYHEAD: f32 = 1.266_667;
const FIRST: (f32, f32) = (0.0, 0.5);
const ESTIMATE: &str = "about 38 MB";
// the code of blending.fps=120 and output.quality=18
const CODE: [&str; 4] = ["club", "fantasy", "dolphin", "dose"];
// the middle of the Share action in the config row
const SHARE: (f32, f32) = (1255.0, 103.0);
const SECOND: (f32, f32) = (0.5, 1.0);
// a tenth of 1080p, chat matches chatMask in assets.ts
const MASK: (u32, u32) = (192, 108);
const MASKS: [(&str, [u32; 4]); 3] = [
    ("chat", [0, 86, 25, 17]),
    ("fps", [91, 0, 10, 3]),
    ("hotbar", [82, 103, 28, 5]),
];
const MENU_AFTER: std::time::Duration = std::time::Duration::from_millis(50);
pub const ADD_MASK: (f32, f32) = (1015.0, 435.0);
pub const FIRST_MASK: (f32, f32) = (1015.0, 357.0);

struct Media {
    thumbs: Vec<Image>,
    wave: Image,
    original: Image,
    result: SharedPixelBuffer<Rgba8Pixel>,
}

thread_local! {
    static MEDIA: RefCell<Option<Media>> = const { RefCell::new(None) };
    static MENU: slint::Timer = slint::Timer::default();
}

fn sections(open: &str) -> Sections {
    Sections {
        dedup: open == "dedup",
        pre: open == "pre",
        interpolation: open == "interpolation",
        masking: open == "masking",
        blending: open == "blending",
        color: open == "color",
        upscale: open == "upscale",
        output: open == "output",
    }
}

const PANEL_END: (f32, f32) = (600.0, 2000.0);

pub fn reveal(app: &App) {
    let reveal = app.global::<Reveal>();
    reveal.set_y(PANEL_END.0);
    reveal.set_height(PANEL_END.1);
    reveal.set_count(reveal.get_count() + 1);
}

pub const REVEALED: [&str; 2] = ["masking", "output"];

pub fn open(app: &App, section: &str) {
    app.set_sections(sections(section));
    if REVEALED.contains(&section) {
        reveal(app);
    }
}

pub fn defaults() -> Settings {
    Settings {
        dedup: true,
        dedup_repeats: 100.0,
        pre_fps: "2x".into(),
        interpolate: true,
        engine: "svp".into(),
        interpolation_fps: "960".into(),
        speed: "faster".into(),
        tuning: "film".into(),
        algorithm: "standard".into(),
        half: true,
        backend: "vulkan".into(),
        model: "rife-v4.6".into(),
        mask: true,
        mask_method: "peel".into(),
        mask_peel: 6.0,
        mask_limit: 4.5,
        mask_edge: 5.0,
        mask_tolerance: 30.0,
        blend: true,
        blend_fps: 60,
        intensity: 1.0,
        gamma: 1.0,
        weighting: "equal".into(),
        custom: "[1, 2, 5]".into(),
        brightness: 1.0,
        contrast: 1.0,
        saturation: 1.0,
        resolution: "2160p".into(),
        method: "nearest".into(),
        codec: "h264".into(),
        encoder: "auto".into(),
        target: "quality".into(),
        quality: 20.0,
        size: 10,
        effort: "fast".into(),
        container: "mp4".into(),
        audio: "mix".into(),
        ..Default::default()
    }
}

fn prefs(projects: bool) -> Prefs {
    Prefs {
        projects,
        autosave: 5,
        ask_save: true,
        zoom: 1.0,
        snapping: true,
        scrolling: "system".into(),
        preset: "default".into(),
        ..Default::default()
    }
}

fn clip(media: &Media, audio: bool, at: f32, cuts: (f32, f32), link: i32) -> Clip {
    Clip {
        name: CLIP.into(),
        at,
        audio,
        link,
        length: SOURCE_SECONDS,
        kept: SOURCE_SECONDS * (cuts.1 - cuts.0),
        cut_in: cuts.0,
        cut_out: cuts.1,
        speed: 1.0,
        ratio: 16.0 / 9.0,
        zoom_x: 1.0,
        zoom_y: 1.0,
        thumbs: ModelRc::new(VecModel::from(media.thumbs.clone())),
        waves: ModelRc::new(VecModel::from(if audio {
            vec![media.wave.clone()]
        } else {
            Vec::new()
        })),
        ..Default::default()
    }
}

fn retimed(clip: &mut Clip, speed: f32) {
    clip.speed = speed;
    clip.kept = clip.length * (clip.cut_out - clip.cut_in) / speed;
}

fn wave(peaks: &[u8]) -> Image {
    let width = peaks.len() as u32;
    let mut pixels = SharedPixelBuffer::<Rgba8Pixel>::new(width, WAVE_ROWS);
    let bytes = pixels.make_mut_bytes();
    for (column, peak) in peaks.iter().enumerate() {
        let rows =
            ((f32::from(*peak) / 255.0 * WAVE_ROWS as f32 / 2.0).round() as usize).max(1) * 2;
        for row in (WAVE_ROWS as usize - rows) / 2..(WAVE_ROWS as usize + rows) / 2 {
            let at = (row * peaks.len() + column) * 4;
            bytes[at..at + 4].copy_from_slice(&[255; 4]);
        }
    }
    Image::from_rgba8(pixels)
}

fn half(frames: &[u8], right: bool) -> SharedPixelBuffer<Rgba8Pixel> {
    let mut pixels = SharedPixelBuffer::<Rgba8Pixel>::new(FRAME.0, FRAME.1);
    let row = (FRAME.0 * 4) as usize;
    for (y, target) in pixels.make_mut_bytes().chunks_exact_mut(row).enumerate() {
        let from = y * row * 2 + if right { row } else { 0 };
        target.copy_from_slice(&frames[from..from + row]);
    }
    pixels
}

fn transformed(picture: &SharedPixelBuffer<Rgba8Pixel>, item: &Clip) -> Image {
    let (width, height) = (FRAME.0 as f32, FRAME.1 as f32);
    let (full_width, full_height) = (item.zoom_x * width, item.zoom_y * height);
    let left = (0.5 + item.x) * width - full_width / 2.0;
    let top = (0.5 + item.y) * height - full_height / 2.0;
    let mut pixels = SharedPixelBuffer::<Rgba8Pixel>::new(FRAME.0, FRAME.1);
    let source = picture.as_bytes();
    for (index, target) in pixels
        .make_mut_bytes()
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .enumerate()
    {
        let u = ((index as u32 % FRAME.0) as f32 + 0.5 - left) / full_width;
        let v = ((index as u32 / FRAME.0) as f32 + 0.5 - top) / full_height;
        let inside = (item.crop_left..1.0 - item.crop_right).contains(&u)
            && (item.crop_top..1.0 - item.crop_bottom).contains(&v);
        let from = ((v * height) as usize * FRAME.0 as usize + (u * width) as usize) * 4;
        target.copy_from_slice(if inside {
            &source[from..from + 4]
        } else {
            &[0, 0, 0, 255]
        });
    }
    Image::from_rgba8(pixels)
}

fn clock(seconds: f32) -> SharedString {
    format!("{}:{:06.3}", seconds as u32 / 60, seconds % 60.0).into()
}

#[cfg(target_arch = "wasm32")]
pub fn position(app: &App, seconds: f32) {
    app.set_position(seconds / app.get_seconds());
    app.set_timecode(clock(seconds));
}

#[cfg(target_arch = "wasm32")]
pub fn source(app: &App, name: &str, seconds: f32) {
    let clips = app.get_clips();
    for (index, mut clip) in clips.iter().enumerate() {
        if clip.name.ends_with(".mp4") {
            let ratio = seconds / clip.length;
            clip.at *= ratio;
            clip.kept *= ratio;
            clip.length = seconds;
            clip.name = name.into();
            clips.set_row_data(index, clip);
        }
    }
    let span = clips
        .iter()
        .map(|clip| clip.at + clip.kept)
        .fold(0.0, f32::max);
    app.set_seconds(span);
    app.set_extent(span);
    app.set_duration(clock(span));
}

fn lanes(app: &App, video: &[f32], audio: &[f32]) {
    let lane = |name: String, y: f32, height: f32| Lane {
        name: name.into(),
        y,
        height,
        ..Default::default()
    };
    let mut top = 0.0;
    let mut rows = Vec::new();
    for (index, height) in video.iter().enumerate().rev() {
        rows.push(lane(format!("V{}", index + 1), top, *height));
        top += height;
    }
    rows.reverse();
    app.set_video(ModelRc::new(VecModel::from(rows)));
    let mut rows = Vec::new();
    for (index, height) in audio.iter().enumerate() {
        rows.push(lane(format!("A{}", index + 1), top, *height));
        top += height;
    }
    app.set_audio(ModelRc::new(VecModel::from(rows)));
    app.set_content(top);
}

fn timeline(app: &App, rows: Vec<Clip>) {
    let span = rows.iter().map(|row| row.at + row.kept).fold(0.0, f32::max);
    app.set_clips(ModelRc::new(VecModel::from(rows)));
    app.set_seconds(span);
    app.set_extent(span);
    app.set_end(1.0);
    app.set_position(PLAYHEAD / span);
    app.set_timecode(clock(PLAYHEAD));
    app.set_duration(clock(span));
    app.set_videos(
        app.get_clips()
            .iter()
            .filter(|clip| !clip.audio && !clip.still)
            .count() as i32,
    );
}

fn reset(app: &App, media: &Media) {
    let split = SOURCE_SECONDS * (FIRST.1 - FIRST.0);
    timeline(
        app,
        vec![
            clip(media, false, 0.0, FIRST, 0),
            clip(media, true, 0.0, FIRST, 0),
            clip(media, false, split, SECOND, 1),
            clip(media, true, split, SECOND, 1),
        ],
    );
    lanes(app, &[64.0], &[36.0]);
    app.set_selected(-1);
    app.set_link(-1);
    app.set_snapped(-1.0);
    app.set_ghosts(ModelRc::default());
    app.set_transform(false);
    app.set_framed(-1);
    app.set_view("result".into());
    app.set_held(false);
    app.set_scope("all clips".into());
    app.set_prefs(prefs(false));
    app.set_recent(ModelRc::default());
    app.set_closing("".into());
    app.set_preview(Image::from_rgba8(media.result.clone()));
    app.set_mode(Mode::Render);
    app.set_separate(false);
    app.set_left("".into());
    app.set_estimate(ESTIMATE.into());
    app.set_unsaved(false);
    app.set_rendering(false);
    app.set_progress(0.0);
    app.set_status("".into());
    let masks = app.global::<Masks>();
    masks.set_editing(false);
    masks.set_clip(-1);
    masks.set_tiles(ModelRc::default());
}

fn mask(part: [u32; 4]) -> Image {
    let mut pixels = SharedPixelBuffer::<Rgba8Pixel>::new(MASK.0, MASK.1);
    for (index, pixel) in pixels.make_mut_slice().iter_mut().enumerate() {
        let (x, y) = (index as u32 % MASK.0, index as u32 / MASK.0);
        let inside =
            (part[0]..part[0] + part[2]).contains(&x) && (part[1]..part[1] + part[3]).contains(&y);
        *pixel = Rgba8Pixel::new(255, 255, 255, if inside { 255 } else { 0 });
    }
    Image::from_rgba8(pixels)
}

fn masking(app: &App, name: &str) {
    let mut settings = app.get_settings();
    settings.mask_method = "image".into();
    settings.mask_image = MASKS[0].0.into();
    app.set_settings(settings);
    let at = match name {
        "mask-add-menu" => (ADD_MASK, PointerEventButton::Left),
        "mask-menu" => (FIRST_MASK, PointerEventButton::Right),
        _ => {
            let masks = app.global::<Masks>();
            let (width, height) = (MASK.0 as i32 * 10, MASK.1 as i32 * 10);
            masks.set_name(MASKS[0].0.into());
            masks.set_opened(MASKS[0].0.into());
            masks.set_width(width);
            masks.set_height(height);
            masks.set_tiles(ModelRc::new(VecModel::from(vec![Tile {
                x: 0,
                y: 0,
                width,
                height,
                picture: mask(MASKS[0].1),
            }])));
            masks.set_clip(0);
            masks.set_undoable(true);
            masks.set_editing(true);
            return open(app, "masking");
        }
    };
    open(app, "masking");
    // the gallery has no layout yet
    let weak = app.as_weak();
    MENU.with(|timer| {
        timer.start(slint::TimerMode::SingleShot, MENU_AFTER, move || {
            if let Some(app) = weak.upgrade() {
                click(&app, at.0, at.1);
            }
        });
    });
}

fn select(app: &App, index: i32, link: i32) {
    app.set_selected(index);
    app.set_link(link);
}

fn edit(app: &App, change: impl FnOnce(&mut Vec<Clip>)) {
    let mut rows: Vec<Clip> = app.get_clips().iter().collect();
    change(&mut rows);
    timeline(app, rows);
}

fn press(app: &App, keys: &[char]) {
    for key in keys {
        let text = SharedString::from(*key);
        let window = app.window();
        window.dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
        window.dispatch_event(WindowEvent::KeyReleased { text });
    }
}

fn click(app: &App, (x, y): (f32, f32), button: PointerEventButton) {
    let position = slint::LogicalPosition::new(x, y);
    let window = app.window();
    window.dispatch_event(WindowEvent::PointerMoved { position });
    window.dispatch_event(WindowEvent::PointerPressed { position, button });
    window.dispatch_event(WindowEvent::PointerReleased { position, button });
}

fn extra_tracks(app: &App, media: &Media) {
    lanes(app, &[40.0, 40.0], &[28.0, 28.0]);
    edit(app, |rows| {
        let mut picture = clip(media, false, 6.0, (0.0, 1.0), 2);
        picture.name = "logo.png".into();
        picture.still = true;
        picture.ratio = 1.0;
        picture.length = 5.0;
        picture.kept = 5.0;
        picture.track = 1;
        let mut music = clip(media, true, 0.0, (0.0, 1.0), 3);
        music.name = "music.mp3".into();
        music.length = rows.iter().map(|row| row.at + row.kept).fold(0.0, f32::max);
        music.kept = music.length;
        music.track = 1;
        rows.extend([picture, music]);
    });
}

fn faded(app: &App) {
    edit(app, |rows| {
        for (index, row) in rows.iter_mut().enumerate() {
            if index < 2 {
                row.fade_in = 0.3;
                row.cross_out = 0.2;
                row.ease_in = "smooth".into();
            } else {
                row.fade_out = 0.3;
                row.cross = 0.2;
                row.cross_in = 0.2;
                row.ease_out = "ease out".into();
            }
        }
    });
}

fn transform(app: &App, media: &Media, change: impl FnOnce(&mut Clip)) {
    select(app, 0, 0);
    app.set_transform(true);
    app.set_framed(0);
    let mut item = clip(media, false, 0.0, FIRST, 0);
    change(&mut item);
    app.set_preview(transformed(&media.result, &item));
    edit(app, |rows| rows[0] = Clip { ..item });
}

pub fn scene(app: &App, name: &str) -> Result<(), String> {
    MEDIA.with_borrow(|media| {
        let media = media.as_ref().ok_or("the window has no media")?;
        MENU.with(slint::Timer::stop);
        press(app, &[Key::Escape.into(), Key::Escape.into()]);
        reset(app, media);
        match name {
            "plain" | "components" | "components-install" | "projects-settings"
            | "general-settings" => {}
            "tracks" => extra_tracks(app, media),
            "moving" => {
                extra_tracks(app, media);
                select(app, 4, 2);
            }
            "track-menu" => {
                extra_tracks(app, media);
                select(app, 5, 3);
                press(app, &['t']);
            }
            "drop" => {
                let ghost = |audio| Ghost {
                    at: 3.0,
                    length: 7.0,
                    track: 0,
                    audio,
                };
                app.set_ghosts(ModelRc::new(VecModel::from(vec![
                    ghost(false),
                    ghost(true),
                ])));
            }
            "selected" => select(app, 0, 0),
            "scope-clip" => {
                select(app, 0, 0);
                app.set_scope("this clip".into());
            }
            "picked" => {
                edit(app, |rows| {
                    rows.iter_mut().for_each(|row| row.picked = true)
                });
                app.set_selected(0);
            }
            "snap" => {
                select(app, 2, 1);
                app.set_snapped(SOURCE_SECONDS * (FIRST.1 - FIRST.0));
            }
            "gap" => edit(app, |rows| {
                rows[2].at += 5.0;
                rows[3].at += 5.0;
            }),
            "speed" => {
                edit(app, |rows| {
                    retimed(&mut rows[0], 0.5);
                    retimed(&mut rows[1], 0.5);
                    rows[2].at = rows[0].kept;
                    rows[3].at = rows[0].kept;
                });
                select(app, 0, 0);
                press(app, &[Key::Menu.into()]);
            }
            "fades" => {
                faded(app);
                select(app, 2, 1);
                press(app, &[Key::Menu.into()]);
            }
            "lossless" => {
                faded(app);
                app.set_mode(Mode::Copy);
                app.set_left("fades".into());
                // the size of the source file, which a copy keeps
                app.set_estimate("about 154 MB".into());
            }
            "rendering" => {
                app.set_rendering(true);
                app.set_progress(0.42);
                app.set_status("24 fps  |  0:08 left".into());
                edit(app, |rows| {
                    rows[0].progress = 0.84;
                    rows[0].state = "84%".into();
                });
            }
            "share" => {
                app.set_unsaved(true);
                click(app, SHARE, PointerEventButton::Left);
                let code = app.global::<Code>();
                code.set_words(ModelRc::new(VecModel::from(
                    CODE.map(SharedString::from).to_vec(),
                )));
                code.set_about("2 changes from default".into());
            }
            "view-original" | "view-split" | "view-side" => {
                app.set_view(name.trim_start_matches("view-").into());
            }
            "held" => app.set_held(true),
            "zoomed" => app.set_prefs(Prefs {
                zoom: 2.0,
                ..prefs(false)
            }),
            "transform-move" => transform(app, media, |item| {
                item.x = 0.16;
                item.y = -0.1;
                item.zoom_x = 0.7;
                item.zoom_y = 0.7;
            }),
            "transform-scale" => transform(app, media, |item| {
                item.zoom_x = 0.6;
                item.zoom_y = 0.6;
            }),
            "transform-crop" => transform(app, media, |item| {
                item.crop_left = 0.12;
                item.crop_right = 0.2;
                item.crop_top = 0.08;
            }),
            "transform-stretch" => transform(app, media, |item| {
                item.zoom_x = 0.6;
                item.zoom_y = 0.95;
            }),
            "separate-menu" => {
                app.set_separate(true);
                press(app, &[Key::F10.into()]);
            }
            "file-menu" | "recent-menu" => {
                app.set_prefs(prefs(true));
                app.set_recent(ModelRc::new(VecModel::from(recent())));
                press(app, &[Key::F10.into()]);
                if name == "recent-menu" {
                    click(app, (68.0, 95.0), PointerEventButton::Left);
                }
            }
            "mask-add-menu" | "mask-menu" | "mask-editor" => masking(app, name),
            "recover" => {
                app.set_prefs(prefs(true));
                app.set_ask("Recover unsaved changes to \"Untitled\"?".into());
                app.set_closing("recover".into());
            }
            _ => return Err(format!("unknown scene {name}")),
        }
        Ok(())
    })
}

fn recent() -> Vec<Recent> {
    let entry = |name: &str, folder: &str| Recent {
        name: name.into(),
        folder: folder.into(),
        path: format!("/{folder}/{name}.lini").into(),
    };
    vec![
        entry("montage", "Videos"),
        entry("clutch", "Replays"),
        entry("intro", "Videos"),
    ]
}

fn thumbnails(bytes: &[u8]) -> Vec<Image> {
    let stride = (THUMB.0 * THUMB.1 * 4) as usize;
    bytes
        .chunks_exact(stride)
        .map(|rgba| {
            Image::from_rgba8(SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(
                rgba, THUMB.0, THUMB.1,
            ))
        })
        .collect()
}

#[cfg(target_arch = "wasm32")]
pub fn set_thumbnails(app: &App, bytes: &[u8]) {
    let thumbs = ModelRc::new(VecModel::from(thumbnails(bytes)));
    let clips = app.get_clips();
    for (index, mut clip) in clips.iter().enumerate() {
        if !clip.audio {
            clip.thumbs = thumbs.clone();
            clips.set_row_data(index, clip);
        }
    }
}

pub fn app(thumbs: &[u8], peaks: &[u8], frames: &[u8]) -> Result<App, PlatformError> {
    let thumbs = thumbnails(thumbs);
    let app = App::new()?;
    MEDIA.set(Some(Media {
        thumbs,
        wave: wave(peaks),
        original: Image::from_rgba8(half(frames, false)),
        result: half(frames, true),
    }));
    app.set_original(
        MEDIA
            .with_borrow(|media| media.as_ref().map(|media| media.original.clone()))
            .unwrap_or_default(),
    );
    let weak = app.as_weak();
    app.on_joint(move |index, head| {
        let rows = weak.upgrade().map(|app| app.get_clips());
        let Some(rows) = rows else { return -1 };
        let Some(this) = rows.row_data(index as usize) else {
            return -1;
        };
        let edge = if head { this.at } else { this.at + this.kept };
        let touching = |other: &Clip| {
            let near = if head {
                other.at + other.kept
            } else {
                other.at
            };
            other.audio == this.audio && other.track == this.track && (near - edge).abs() < 0.002
        };
        rows.iter()
            .position(|other| touching(&other))
            .map_or(-1, |place| place as i32)
    });
    open(&app, "interpolation");
    app.set_settings(defaults());
    app.set_models(ModelRc::new(VecModel::from(
        MODELS.map(SharedString::from).to_vec(),
    )));
    app.set_configs(ModelRc::new(VecModel::from(vec![SharedString::from(
        "built-in",
    )])));
    app.set_config("built-in".into());
    app.set_tick(1.0 / 60.0);
    app.set_details("1920x1080  |  240 to 60 fps".into());
    app.set_rate("240 to 60 fps".into());
    app.set_version(env!("VERSION").into());
    app.set_encoders_found(true);
    let masks = app.global::<Masks>();
    let tiles = MASKS.map(|(name, part)| MaskTile {
        name: name.into(),
        picture: mask(part),
        ..Default::default()
    });
    masks.set_list(ModelRc::new(VecModel::from(tiles.to_vec())));
    masks.on_found(|list, name| list.iter().any(|mask| mask.name == name));
    set_weights(&app, &[1.0; 17]);
    scene(&app, "plain").map_err(PlatformError::Other)?;
    Ok(app)
}

pub fn set_weights(app: &App, weights: &[f32]) {
    let peak = weights.iter().copied().fold(0.0, f32::max);
    let bars: Vec<f32> = weights.iter().map(|weight| weight / peak).collect();
    app.set_summary(format!("{} frames blended", bars.len()).into());
    app.set_weights(ModelRc::new(VecModel::from(bars)));
}

pub fn dialog() -> Result<Dialog, PlatformError> {
    let window = Dialog::new()?;
    window.set_prefs(prefs(true));
    window.set_ffmpeg("bundled".into());
    window.set_ffmpeg_system(true);
    window.set_tensorrt_shipped(true);
    let models = MODELS.map(|name| Download {
        name: name.into(),
        installed: name == "rife-v4.6" || name == "rife-v4.25",
    });
    window.set_models(ModelRc::new(VecModel::from(models.to_vec())));
    Ok(window)
}

pub fn tab(window: &Dialog, name: &str) {
    let section = match name {
        "components" | "components-install" => "Components",
        "projects-settings" => "Projects",
        _ => "General",
    };
    window.set_section(section.into());
    window.set_prefs(prefs(name != "projects-settings"));
    let installing = name == "components-install";
    window.set_busy(if installing { "tensorrt" } else { "" }.into());
    window.set_progress(
        if installing {
            "1210 of 3100 MB  |  24.1 MB/s  |  1:18 left"
        } else {
            ""
        }
        .into(),
    );
    window.set_fraction(if installing { 0.39 } else { 0.0 });
}
