use std::cell::RefCell;
use std::fs::File;
use std::io::BufWriter;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use interpolini_core::config::{self, Config};
use interpolini_core::mask::{self, Gray};
use slint::{ComponentHandle, ModelRc, Rgba8Pixel, SharedPixelBuffer, VecModel};

use crate::preview::{self, say};
use crate::project::{ask, title, touch};
use crate::state::{Entry, Shared, State, lock};
use crate::view::{load, refresh};
use crate::{App, Masks, Note, Tile, history, masks};

const TILE: u32 = 256;
// no mask file can have this name
pub const DRAFT: &str = "/";

struct Step {
    part: [u32; 4],
    pixels: Vec<u8>,
}

struct Editor {
    gray: Gray,
    before: (String, bool),
    recorded: Instant,
    dirty: bool,
    shown: Instant,
    lost: bool,
    start: Vec<u8>,
    part: Option<[u32; 4]>,
    last: (f32, f32),
    undo: Vec<Step>,
    redo: Vec<Step>,
    tiles: Rc<VecModel<Tile>>,
}

thread_local! {
    static EDITOR: RefCell<Option<Editor>> = const { RefCell::new(None) };
}

fn target(ui: &App, state: &State) -> Option<usize> {
    let shown = state.part(ui.get_position()).map(|part| part.1.clips);
    let shown = shown.unwrap_or_default();
    let picture = |index: &usize| !state.entries[*index].still && !state.entries[*index].missing;
    if ui.global::<Masks>().get_editing() {
        let drawn =
            |index: &usize| picture(index) && state.entries[*index].config.mask.image == DRAFT;
        return shown.into_iter().rev().find(drawn);
    }
    match state.video(ui).filter(|_| ui.get_scope() == "this clip") {
        Some(own) => shown.contains(&own).then_some(own),
        None => shown.into_iter().rev().find(picture),
    }
}

pub fn aim(ui: &App, state: &State) {
    let clip = target(ui, state).map_or(-1, |index| index as i32);
    ui.global::<Masks>().set_clip(clip);
}

fn tile(gray: &Gray, x: u32, y: u32) -> Tile {
    let (width, height) = (TILE.min(gray.width - x), TILE.min(gray.height - y));
    let mut pixels = SharedPixelBuffer::<Rgba8Pixel>::new(width, height);
    let lines = pixels.make_mut_slice().chunks_mut(width as usize);
    for (row, line) in lines.enumerate() {
        let start = (y as usize + row) * gray.width as usize + x as usize;
        let values = &gray.pixels[start..start + width as usize];
        for (pixel, value) in line.iter_mut().zip(values) {
            *pixel = Rgba8Pixel::new(255, 255, 255, 255 - value);
        }
    }
    Tile {
        x: x as i32,
        y: y as i32,
        width: width as i32,
        height: height as i32,
        picture: slint::Image::from_rgba8(pixels),
    }
}

impl Editor {
    fn show(&self, part: [u32; 4]) {
        let columns = self.gray.width.div_ceil(TILE);
        for row in part[1] / TILE..part[3].div_ceil(TILE) {
            for column in part[0] / TILE..part[2].div_ceil(TILE) {
                let index = (row * columns + column) as usize;
                let tile = tile(&self.gray, column * TILE, row * TILE);
                slint::Model::set_row_data(&*self.tiles, index, tile);
            }
        }
    }

    fn paint(
        &mut self,
        from: (f32, f32),
        to: (f32, f32),
        value: u8,
        inside: impl Fn(f32, f32) -> bool,
    ) {
        let (width, height) = (self.gray.width as f32, self.gray.height as f32);
        let part = [
            from.0.min(to.0).floor().clamp(0.0, width) as u32,
            from.1.min(to.1).floor().clamp(0.0, height) as u32,
            from.0.max(to.0).ceil().clamp(0.0, width) as u32,
            from.1.max(to.1).ceil().clamp(0.0, height) as u32,
        ];
        if part[0] >= part[2] || part[1] >= part[3] {
            return;
        }
        let wide = self.gray.width as usize;
        for y in part[1]..part[3] {
            for x in part[0]..part[2] {
                if inside(x as f32 + 0.5, y as f32 + 0.5) {
                    self.gray.pixels[y as usize * wide + x as usize] = value;
                }
            }
        }
        self.part = Some(match self.part {
            Some(was) => [
                was[0].min(part[0]),
                was[1].min(part[1]),
                was[2].max(part[2]),
                was[3].max(part[3]),
            ],
            None => part,
        });
        self.show(part);
    }

    fn brush(&mut self, to: (f32, f32), size: f32, value: u8) {
        // a smaller brush can miss every pixel
        let (from, radius) = (self.last, (size / 2.0).max(0.75));
        self.last = to;
        let (dx, dy) = (to.0 - from.0, to.1 - from.1);
        let length = dx * dx + dy * dy;
        let near = |x: f32, y: f32| {
            let along = match length > 0.0 {
                true => (((x - from.0) * dx + (y - from.1) * dy) / length).clamp(0.0, 1.0),
                false => 0.0,
            };
            let (x, y) = (x - from.0 - along * dx, y - from.1 - along * dy);
            x * x + y * y <= radius * radius
        };
        let low = (from.0.min(to.0) - radius, from.1.min(to.1) - radius);
        let high = (from.0.max(to.0) + radius, from.1.max(to.1) + radius);
        self.paint(low, high, value, near);
    }

    fn begin(&mut self) {
        self.start.clone_from(&self.gray.pixels);
        self.part = None;
    }

    fn end(&mut self) -> bool {
        let Some(part) = self.part.take() else {
            return false;
        };
        let wide = self.gray.width as usize;
        let rows = (part[1]..part[3])
            .map(|y| y as usize * wide)
            .map(|start| start + part[0] as usize..start + part[2] as usize);
        if rows
            .clone()
            .all(|row| self.start[row.clone()] == self.gray.pixels[row])
        {
            return false;
        }
        let pixels = rows.flat_map(|row| &self.start[row]).copied().collect();
        self.undo.push(Step { part, pixels });
        self.redo.clear();
        let most = 16 * self.gray.pixels.len();
        while self
            .undo
            .iter()
            .map(|step| step.pixels.len())
            .sum::<usize>()
            > most
        {
            self.undo.remove(0);
            self.lost = true;
        }
        true
    }

    fn travel(&mut self, back: bool) -> bool {
        let (from, to) = match back {
            true => (&mut self.undo, &mut self.redo),
            false => (&mut self.redo, &mut self.undo),
        };
        let Some(mut step) = from.pop() else {
            return false;
        };
        let [left, top, right, bottom] = step.part;
        let (wide, part) = (self.gray.width as usize, (right - left) as usize);
        for (row, y) in (top..bottom).enumerate() {
            let start = y as usize * wide + left as usize;
            let mask = &mut self.gray.pixels[start..start + part];
            mask.swap_with_slice(&mut step.pixels[row * part..(row + 1) * part]);
        }
        let shown = step.part;
        to.push(step);
        self.show(shown);
        true
    }
}

fn halo(ui: &App, gray: &Gray) -> slint::Image {
    let soft = ui.get_settings().mask_soft;
    match soft > 0.0 {
        true => masks::tint(gray, soft),
        false => slint::Image::default(),
    }
}

pub fn tint(ui: &App) -> Option<slint::Image> {
    EDITOR.with_borrow(|editor| editor.as_ref().map(|editor| halo(ui, &editor.gray)))
}

fn publish(ui: &App, shared: &Shared, editor: &Editor) {
    let masks = ui.global::<Masks>();
    masks.set_undoable(!editor.undo.is_empty());
    masks.set_redoable(!editor.redo.is_empty());
    mask::hold(Some((DRAFT.into(), editor.gray.clone())));
    preview::remask(vec![DRAFT.into()]);
    refresh(ui, shared);
}

fn change(ui: &App, shared: &Shared, change: impl FnOnce(&mut Editor) -> bool) {
    EDITOR.with_borrow_mut(|editor| {
        let Some(editor) = editor.as_mut() else {
            return;
        };
        let done = change(editor);
        if done || editor.shown.elapsed() >= Duration::from_millis(30) {
            editor.shown = Instant::now();
            ui.global::<Masks>().set_hover(halo(ui, &editor.gray));
        }
        if done {
            publish(ui, shared, editor);
        }
    });
}

pub fn travel(ui: &App, shared: &Shared, back: bool) -> bool {
    let open = EDITOR.with_borrow(Option::is_some);
    change(ui, shared, |editor| {
        editor.part.is_none() && editor.travel(back)
    });
    open
}

pub fn unsaved() -> bool {
    EDITOR.with_borrow(|editor| {
        editor
            .as_ref()
            .is_some_and(|editor| !editor.undo.is_empty() || editor.part.is_some() || editor.lost)
    })
}

pub fn question(ui: &App) -> String {
    match ui.global::<Masks>().get_name().trim() {
        "" => "Save the mask?".into(),
        name => format!("Save the mask \"{name}\"?"),
    }
}

pub fn left(ui: &App, shared: &Shared, again: impl FnOnce(&App) + 'static) -> bool {
    if unsaved() {
        // one click can ask twice
        if ui.get_closing() != "leave" {
            ask(ui, "leave", question(ui), Box::new(move |ui, _| again(ui)));
        }
        return false;
    }
    close(ui, shared, false);
    true
}

pub fn settled(config: &Config) -> Config {
    let mut config = config.clone();
    if config.mask.image == DRAFT {
        let before =
            EDITOR.with_borrow(|editor| editor.as_ref().map(|editor| editor.before.clone()));
        (config.mask.image, config.mask.enabled) = before.unwrap_or_default();
    }
    config
}

fn open(ui: &App, shared: &Shared, opened: Option<String>) {
    if EDITOR.with_borrow(Option::is_some) {
        return;
    }
    let state = lock(shared);
    let shown = target(ui, &state);
    let own = state.video(ui).filter(|_| ui.get_scope() == "this clip");
    let Some(clip) = shown.or(own).or_else(|| state.base()) else {
        return;
    };
    let part = state
        .parts()
        .into_iter()
        .find(|part| part.clips.contains(&clip));
    let moved = part
        .filter(|_| shown.is_none())
        .map(|part| part.start / state.span.max(0.001));
    let (width, height) = state.entries[clip].size;
    drop(state);
    let gray = match &opened {
        Some(name) => match mask::load(name) {
            Ok(gray) => gray,
            Err(_) => return say(ui, format!("Couldn't open {name}"), Note::Warning, -1.0),
        },
        None if width > 0 && height > 0 => Gray {
            width,
            height,
            pixels: vec![255; width as usize * height as usize],
        },
        None => return,
    };
    let name = opened.clone().unwrap_or_else(|| masks::free("mask"));
    let mut settings = ui.get_settings();
    let before = (settings.mask_image.to_string(), settings.mask);
    let tiles: Vec<Tile> = (0..gray.height.div_ceil(TILE))
        .flat_map(|row| (0..gray.width.div_ceil(TILE)).map(move |column| (column, row)))
        .map(|(column, row)| tile(&gray, column * TILE, row * TILE))
        .collect();
    let tiles = Rc::new(VecModel::from(tiles));
    let masks = ui.global::<Masks>();
    masks.set_hover(halo(ui, &gray));
    masks.set_width(gray.width as i32);
    masks.set_height(gray.height as i32);
    masks.set_tiles(ModelRc::from(Rc::clone(&tiles)));
    masks.set_opened(opened.unwrap_or_default().into());
    masks.set_name(name.as_str().into());
    masks.set_size((gray.height as f32 / 27.0).round().max(1.0));
    masks.set_editing(true);
    masks.set_undoable(false);
    masks.set_redoable(false);
    let mut sections = ui.get_sections();
    sections.masking = true;
    ui.set_sections(sections);
    mask::hold(Some((DRAFT.into(), gray.clone())));
    (settings.mask_image, settings.mask) = (DRAFT.into(), true);
    ui.set_settings(settings);
    let mut state = lock(shared);
    let dirty = state.project.dirty;
    history::record(ui, &mut state, false);
    drop(state);
    ui.invoke_changed();
    EDITOR.set(Some(Editor {
        recorded: history::recorded(),
        dirty,
        shown: Instant::now(),
        lost: false,
        start: Vec::new(),
        part: None,
        last: (0.0, 0.0),
        undo: Vec::new(),
        redo: Vec::new(),
        gray,
        before,
        tiles,
    }));
    if let Some(position) = moved {
        ui.set_position(position as f32);
        ui.invoke_seek();
    }
}

pub fn write(name: &str, gray: &Gray) -> std::io::Result<()> {
    if name.contains(['/', '\\']) {
        return Err(std::io::ErrorKind::InvalidInput.into());
    }
    let path = mask::path(name);
    std::fs::create_dir_all(config::app_dir().join("masks"))?;
    let part = path.with_extension("png.part");
    let encode = || {
        let file = BufWriter::new(File::create(&part)?);
        let mut encoder = png::Encoder::new(file, gray.width, gray.height);
        encoder.set_color(png::ColorType::Grayscale);
        let mut writer = encoder.write_header().map_err(std::io::Error::other)?;
        writer
            .write_image_data(&gray.pixels)
            .and_then(|_| writer.finish())
            .map_err(std::io::Error::other)
    };
    let written = encode().and_then(|_| std::fs::rename(&part, &path));
    if written.is_err() {
        let _ = std::fs::remove_file(&part);
    }
    written
}

pub fn close(ui: &App, shared: &Shared, save: bool) {
    if EDITOR.with_borrow(Option::is_none) {
        return;
    }
    let masks = ui.global::<Masks>();
    let name = match masks.get_name().trim() {
        "" => masks::free("mask"),
        name => name.to_owned(),
    };
    if save {
        let old = masks.get_opened().to_string();
        if !old.is_empty() && old != name {
            if mask::path(&old).exists() && masks::rename(&old, &name).is_err() {
                return say(ui, format!("Couldn't save {name}"), Note::Warning, -1.0);
            }
            masks.set_opened(name.as_str().into());
            masks::retarget(&mut lock(shared), &old, &name);
            EDITOR.with_borrow_mut(|editor| {
                if let Some(editor) = editor.as_mut().filter(|editor| editor.before.0 == old) {
                    editor.before.0.clone_from(&name);
                }
            });
        }
        let written =
            EDITOR.with_borrow(|editor| editor.as_ref().map(|editor| write(&name, &editor.gray)));
        if let Some(Err(_)) = written {
            return say(ui, format!("Couldn't save {name}"), Note::Warning, -1.0);
        }
    }
    let Some(editor) = EDITOR.take() else {
        return;
    };
    masks.set_editing(false);
    masks.set_tiles(ModelRc::default());
    masks.set_hover(slint::Image::default());
    let (image, enabled) = match save {
        true => (name, true),
        false => editor.before,
    };
    let settle = |entry: &mut Entry| {
        let mask = &mut entry.config.mask;
        if mask.image == DRAFT {
            (mask.image, mask.enabled) = (image.clone(), enabled);
        }
    };
    let mut state = lock(shared);
    let State { entries, draft, .. } = &mut *state;
    entries.iter_mut().chain([draft]).for_each(settle);
    history::amend(settle);
    if save {
        touch(ui, &mut state);
    } else if history::recorded() == editor.recorded {
        // later changes keep the step
        history::forget();
        state.project.dirty = editor.dirty;
        title(ui, &state);
    }
    load(ui, &mut state);
    drop(state);
    mask::hold(None);
    preview::remask(vec![DRAFT.into()]);
    masks::scan(ui, shared);
    refresh(ui, shared);
}

pub fn wire(ui: &App, state: &Shared) {
    let masks = ui.global::<Masks>();
    let (weak, shared) = (ui.as_weak(), Arc::clone(state));
    masks.on_draw(move || {
        if let Some(ui) = weak.upgrade() {
            open(&ui, &shared, None);
        }
    });
    let (weak, shared) = (ui.as_weak(), Arc::clone(state));
    masks.on_edit(move |name| {
        if let Some(ui) = weak.upgrade() {
            open(&ui, &shared, Some(name.into()));
        }
    });
    let (weak, shared) = (ui.as_weak(), Arc::clone(state));
    masks.on_stroke(move |x, y, erase, first| {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        let size = ui.global::<Masks>().get_size();
        change(&ui, &shared, |editor| {
            if first {
                editor.begin();
                editor.last = (x, y);
            }
            editor.brush((x, y), size, if erase { 255 } else { 0 });
            false
        });
    });
    let (weak, shared) = (ui.as_weak(), Arc::clone(state));
    masks.on_shape(move |x0, y0, x1, y1, erase, ellipse| {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        let middle = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
        let radius = ((x1 - x0).abs() / 2.0, (y1 - y0).abs() / 2.0);
        let (low, high) = ((x0.min(x1), y0.min(y1)), (x0.max(x1), y0.max(y1)));
        let inside = |x: f32, y: f32| match ellipse {
            true => {
                ((x - middle.0) / radius.0).powi(2) + ((y - middle.1) / radius.1).powi(2) <= 1.0
            }
            false => x >= low.0 && x <= high.0 && y >= low.1 && y <= high.1,
        };
        change(&ui, &shared, |editor| {
            editor.begin();
            editor.paint(low, high, if erase { 255 } else { 0 }, inside);
            false
        });
    });
    let (weak, shared) = (ui.as_weak(), Arc::clone(state));
    masks.on_done(move || {
        if let Some(ui) = weak.upgrade() {
            change(&ui, &shared, Editor::end);
        }
    });
    let (weak, shared) = (ui.as_weak(), Arc::clone(state));
    masks.on_clear(move || {
        if let Some(ui) = weak.upgrade() {
            change(&ui, &shared, |editor| {
                editor.begin();
                let (width, height) = (editor.gray.width as f32, editor.gray.height as f32);
                editor.paint((0.0, 0.0), (width, height), 255, |_, _| true);
                editor.end()
            });
        }
    });
    let (weak, shared) = (ui.as_weak(), Arc::clone(state));
    masks.on_invert(move || {
        if let Some(ui) = weak.upgrade() {
            change(&ui, &shared, |editor| {
                editor.begin();
                let pixels = editor.gray.pixels.iter_mut();
                pixels.for_each(|pixel| *pixel = 255 - *pixel);
                let whole = [0, 0, editor.gray.width, editor.gray.height];
                editor.part = Some(whole);
                editor.show(whole);
                editor.end()
            });
        }
    });
    for back in [true, false] {
        let (weak, shared) = (ui.as_weak(), Arc::clone(state));
        let travel = move || {
            if let Some(ui) = weak.upgrade() {
                travel(&ui, &shared, back);
            }
        };
        match back {
            true => masks.on_undo(travel),
            false => masks.on_redo(travel),
        }
    }
    let (weak, shared) = (ui.as_weak(), Arc::clone(state));
    masks.on_save(move || {
        if let Some(ui) = weak.upgrade() {
            close(&ui, &shared, true);
        }
    });
    let (weak, shared) = (ui.as_weak(), Arc::clone(state));
    masks.on_leave(move || {
        if let Some(ui) = weak.upgrade() {
            left(&ui, &shared, |_| {});
        }
    });
}
