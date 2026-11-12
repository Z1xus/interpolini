use i_slint_backend_testing::{AccessibleRole, ElementHandle, ElementQuery};
use slint::ComponentHandle;

#[path = "../src/demo.rs"]
mod demo;

slint::include_modules!();

type Rect = [f32; 4];

fn rect(element: &ElementHandle) -> Rect {
    let (position, size) = (element.absolute_position(), element.size());
    [position.x, position.y, size.width, size.height]
}

fn section(app: &App, title: &str) -> ElementHandle {
    let title = title.to_owned();
    ElementQuery::from_root(app)
        .match_inherits("Section")
        .match_predicate(move |section| {
            let header = section
                .query_descendants()
                .match_predicate(|element| element.accessible_label().is_some())
                .find_first();
            header
                .and_then(|header| header.accessible_label())
                .as_deref()
                == Some(title.as_str())
        })
        .find_first()
        .expect("section")
}

fn row(section: &ElementHandle, label: &str) -> Rect {
    let label = label.to_owned();
    let name = section
        .query_descendants()
        .match_type_name("Label")
        .match_predicate(move |element| {
            element.accessible_label().as_deref() == Some(label.as_str())
        })
        .find_first()
        .expect("row");
    let [x, _, width, _] = rect(section);
    let [_, y, _, height] = rect(&name);
    [x, y, width, height]
}

fn number(value: f32) -> f32 {
    (value * 100.0).round() / 100.0
}

fn json(rects: &[(&str, Rect)], depth: usize) -> String {
    let tabs = "\t".repeat(depth);
    let rects: Vec<String> = rects
        .iter()
        .map(|(name, [x, y, w, h])| {
            let [x, y, w, h] = [x, y, w, h].map(|value| number(*value));
            format!("{tabs}\t\"{name}\": [{x}, {y}, {w}, {h}]")
        })
        .collect();
    format!("{{\n{}\n{tabs}}}", rects.join(",\n"))
}

fn state(app: &App, open: &str, change: impl Fn(&mut Settings)) {
    let mut settings = demo::defaults();
    change(&mut settings);
    app.set_settings(settings);
    demo::open(app, open);
    for _ in 0..2 {
        i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_secs(1));
    }
}

fn open_panel(app: &App, open: &str, title: &str, change: impl Fn(&mut Settings)) -> ElementHandle {
    state(app, open, change);
    section(app, title)
}

fn rows(section: &ElementHandle, labels: &[(&'static str, &str)]) -> Vec<(&'static str, Rect)> {
    let header = section
        .query_descendants()
        .match_predicate(|element| element.accessible_label().is_some())
        .find_first()
        .expect("header");
    let mut rects = vec![("section", rect(section)), ("header", rect(&header))];
    rects.extend(
        labels
            .iter()
            .map(|(key, label)| (*key, row(section, label))),
    );
    rects
}

fn entry(name: &str, rects: &[(&str, Rect)]) -> String {
    format!("\t\"{name}\": {}", json(rects, 1))
}

struct Scan(Vec<(String, String, Rect)>, Vec<(usize, Rect)>);

impl Scan {
    fn new(root: &impl ComponentHandle) -> Self {
        let mut items: Vec<(String, String, Rect)> = Vec::new();
        let mut clips: Vec<(usize, Rect)> = Vec::new();
        for element in ElementQuery::from_root(root).match_descendants().find_all() {
            let Some(label) = element.accessible_label() else {
                continue;
            };
            if let Some(index) = element
                .accessible_item_index()
                .filter(|_| element.accessible_role() == Some(AccessibleRole::ListItem))
            {
                clips.push((index, rect(&element)));
            }
            let item = (
                element.type_name().unwrap_or_default().to_string(),
                label.to_string(),
                rect(&element),
            );
            if !items.contains(&item) {
                items.push(item);
            }
        }
        clips.sort_by_key(|clip| clip.0);
        clips.dedup_by_key(|clip| clip.0);
        Self(items, clips)
    }

    fn get(&self, kind: &str, label: &str) -> Rect {
        let found = self.0.iter().find(|item| item.0 == kind && item.1 == label);
        found.unwrap_or_else(|| panic!("no {kind} {label}")).2
    }

    fn clips(&self) -> Vec<Rect> {
        self.1.iter().map(|clip| clip.1).collect()
    }
}

fn union(rects: &[Rect]) -> Rect {
    let left = rects.iter().map(|rect| rect[0]).fold(f32::MAX, f32::min);
    let top = rects.iter().map(|rect| rect[1]).fold(f32::MAX, f32::min);
    let right = rects
        .iter()
        .map(|rect| rect[0] + rect[2])
        .fold(0.0, f32::max);
    let bottom = rects
        .iter()
        .map(|rect| rect[1] + rect[3])
        .fold(0.0, f32::max);
    [left, top, right - left, bottom - top]
}

fn shift(rect: Rect, to: (f32, f32)) -> Rect {
    [rect[0] + to.0, rect[1] + to.1, rect[2], rect[3]]
}

fn grown(rect: Rect, by: f32) -> Rect {
    [
        rect[0] - by,
        rect[1] - by,
        rect[2] + 2.0 * by,
        rect[3] + 2.0 * by,
    ]
}

fn popup(rows: &[Rect], origin: (f32, f32)) -> Rect {
    let size = union(rows);
    [
        origin.0,
        origin.1,
        size[0] + size[2] + 4.0,
        size[1] + size[3] + 4.0,
    ]
}

fn above(timeline: Rect, x: f32, middle: f32, rows: &[Rect]) -> Rect {
    let height = union(rows)[1] + union(rows)[3] + 4.0;
    let anchor = middle.clamp(timeline[1] + 24.0, timeline[1] + timeline[3] - 8.0);
    popup(rows, (x, anchor - height))
}

fn names(app: &App, name: &str) -> Vec<(&'static str, Rect)> {
    let scan = Scan::new(app);
    let window = [0.0, 0.0, demo::WINDOW.0 as f32, demo::WINDOW.1 as f32];
    let stage = ElementHandle::find_by_element_id(app, "App::stage")
        .next()
        .expect("stage");
    let timeline = ElementHandle::find_by_element_id(app, "App::timeline")
        .next()
        .expect("timeline");
    let (stage, timeline) = (rect(&stage), rect(&timeline));
    let transport = ElementHandle::find_by_element_type_name(app, "Transport")
        .next()
        .map(|transport| rect(&transport))
        .expect("transport");
    let panel = ElementHandle::find_by_element_type_name(app, "Panel")
        .next()
        .map(|panel| rect(&panel))
        .expect("panel");
    let button = |label: &str| scan.get("IconButton", label);
    let action = |label: &str| scan.get("Action", label);
    let mut found = vec![
        ("stage", stage),
        ("timeline", timeline),
        ("transport", transport),
        ("editor", union(&[transport, timeline])),
        ("panel", panel),
        ("stage-transport", union(&[stage, transport])),
        ("tabs", scan.get("Tabs", "Preview view")),
        (
            "zoom",
            scan.0
                .iter()
                .find(|item| item.0 == "Action" && item.1.starts_with("Zoom"))
                .expect("zoom")
                .2,
        ),
        ("import", button("Import")),
        ("split", button("Split")),
        ("snapping", button("Snapping")),
        ("transform", button("Transform")),
        ("save-frame", button("Save frame")),
        ("resolution", scan.get("Dropdown", "Playback resolution")),
        ("config", scan.get("Dropdown", "Config")),
        (
            "ruler",
            [timeline[0] + 150.0, timeline[1], timeline[2] - 150.0, 24.0],
        ),
        ("joint", [timeline[0], timeline[1], 670.0, timeline[3]]),
        (
            "editor-joint",
            [
                transport[0],
                transport[1],
                670.0,
                transport[3] + timeline[3],
            ],
        ),
        ("panel-top", [panel[0], panel[1], panel[2], 190.0]),
    ];
    let joint = [timeline[0], timeline[1], 670.0, timeline[3]];
    let marks = [
        button("Set start"),
        button("Set end"),
        button("Split"),
        button("Restore full length"),
    ];
    found.push(("cut-tools", union(&marks)));
    found.push((
        "add-tracks",
        union(&[action("Add video track"), action("Add audio track")]),
    ));
    let scope = scan.get("Label", "Settings for");
    found.push(("scope", [panel[0], scope[1], panel[2], scope[3]]));
    let clips = scan.clips();
    match name {
        "gap" => {
            let (first, second) = (clips[0], clips[2]);
            let gap = [
                first[0] + first[2],
                first[1],
                second[0] - first[0] - first[2],
                clips[1][1] + clips[1][3] - first[1],
            ];
            found.push(("gap", gap));
        }
        "track-menu" => {
            let rows: Vec<Rect> = [
                "Add track",
                "Mute track",
                "Select clips on track",
                "Remove track",
            ]
            .iter()
            .map(|label| scan.get("MenuItem", label))
            .collect();
            let lane = scan.get("Rectangle", "Audio track A2");
            let menu = above(timeline, timeline[0] + 24.0, lane[1] + lane[3] / 2.0, &rows);
            found.push(("menu", menu));
            found.push(("menu-joint", union(&[menu, joint])));
            found.push(("mute", shift(rows[1], (menu[0], menu[1]))));
        }
        "speed" | "fades" => {
            let selected = if name == "speed" { clips[0] } else { clips[2] };
            let slider = |label: &str| scan.get("Slider", label);
            let segment = |label: &str| scan.get("Segment", label);
            let fades = [
                slider("Fade in"),
                segment("Fade in curve"),
                slider("Fade out"),
                segment("Fade out curve"),
            ];
            let mut rows = vec![
                slider("Speed"),
                scan.get("MenuItem", "Split"),
                scan.get("MenuItem", "Ripple delete"),
            ];
            rows.extend(fades);
            let x =
                (selected[0] + 12.0).clamp(timeline[0] + 158.0, timeline[0] + timeline[2] - 8.0);
            let menu = above(timeline, x, selected[1] + selected[3] / 2.0, &rows);
            let row = |rows: &[Rect]| {
                let rows = union(rows);
                [menu[0] + 4.0, menu[1] + rows[1], menu[2] - 8.0, rows[3]]
            };
            found.push(("menu", menu));
            found.push(("menu-joint", union(&[menu, joint])));
            found.push(("speed", row(&[slider("Speed")])));
            found.push(("fades", row(&fades)));
        }
        "file-menu" | "recent-menu" => {
            let origin = (4.0, 26.0);
            let rows: Vec<Rect> = [
                "New project",
                "Open project…",
                "Open recent",
                "Save",
                "Save as…",
                "Project settings…",
                "Import…",
                "Close all clips",
                "Locate missing files…",
                "Render",
                "Save frame",
                "Export each clip separately",
                "Settings…",
                "Quit",
            ]
            .iter()
            .map(|label| scan.get("MenuItem", label))
            .collect();
            let menu = popup(&rows, origin);
            found.push(("menu", menu));
            found.push((
                "file-open",
                shift(union(&[rows[0], rows[1], rows[2]]), origin),
            ));
            found.push(("file-save", shift(union(&[rows[3], rows[4]]), origin)));
            found.push(("file-project", shift(rows[5], origin)));
            if name == "recent-menu" {
                let entries: Vec<Rect> = ["montage", "clutch", "intro", "Clear list"]
                    .iter()
                    .map(|label| scan.get("MenuItem", label))
                    .collect();
                let list = shift(union(&entries), origin);
                found.push(("recent", list));
                found.push(("menu", union(&[menu, grown(list, 4.0)])));
            }
        }
        "mask-add-menu" | "mask-menu" => {
            let (origin, labels): ((f32, f32), &[&str]) = match name {
                "mask-add-menu" => (demo::ADD_MASK, &["Draw on the clip", "Import an image…"]),
                _ => (
                    demo::FIRST_MASK,
                    &["Edit", "Rename", "Duplicate", "Show in folder", "Delete"],
                ),
            };
            let rows: Vec<Rect> = labels
                .iter()
                .map(|label| scan.get("MenuItem", label))
                .collect();
            let menu = popup(&rows, origin);
            let gallery = ElementHandle::find_by_element_type_name(app, "MaskGrid")
                .next()
                .map(|gallery| rect(&gallery))
                .expect("gallery");
            found.push(("menu", menu));
            found.push(("gallery", gallery));
            found.push(("menu-gallery", union(&[menu, gallery])));
        }
        "mask-editor" => {
            let label = |label: &str| {
                let found = scan.get("Label", label);
                [panel[0], found[1], panel[2], found[3]]
            };
            let actions = union(&[action("Undo"), action("Invert")]);
            let buttons = union(&[scan.get("Button", "Cancel"), scan.get("Button", "Save")]);
            let note = scan.get("Text", "Paint the parts that keep the original frame.");
            let bottom = buttons[1] + buttons[3] + 10.0;
            let masking = [panel[0], note[1] - 6.0, panel[2], bottom - note[1] + 6.0];
            found.push(("masking", masking));
            found.push(("name", label("Name")));
            found.push(("tool", label("Tool")));
            found.push(("mode", label("Mode")));
            found.push(("size", label("Size")));
            found.push(("actions", [panel[0], actions[1], panel[2], actions[3]]));
            found.push(("soft", label("Soft edge")));
            found.push(("buttons", [panel[0], buttons[1], panel[2], buttons[3]]));
        }
        "separate-menu" => {
            let origin = (4.0, 26.0);
            let rows: Vec<Rect> = [
                "Project settings…",
                "Import…",
                "Close all clips",
                "Locate missing files…",
                "Render",
                "Save frame",
                "Export each clip separately",
                "Settings…",
                "Quit",
            ]
            .iter()
            .map(|label| scan.get("MenuItem", label))
            .collect();
            found.push(("menu", popup(&rows, origin)));
            found.push(("file-separate", shift(rows[6], origin)));
        }
        "rendering" => {
            let go = scan.get("Rectangle", "Stop");
            let foot = [
                panel[0],
                go[1] - 10.0,
                panel[2],
                panel[1] + panel[3] - go[1] + 10.0,
            ];
            found.push(("go", go));
            found.push(("foot", foot));
        }
        "share" => {
            let config = scan.get("Dropdown", "Config");
            // the row has 10px of padding and the popup overlaps it by 4px
            let origin = (panel[0] + 10.0, config[1] + config[3] + 6.0);
            let apply = scan.get("Button", "Apply");
            let popup = [
                origin.0,
                origin.1,
                panel[2] - 20.0,
                apply[1] + apply[3] + 12.0,
            ];
            found.push(("share", scan.get("Action", "Share config")));
            found.push(("popup", popup));
            found.push((
                "share-area",
                [
                    panel[0],
                    panel[1],
                    panel[2],
                    popup[1] + popup[3] + 10.0 - panel[1],
                ],
            ));
        }
        "recover" => {
            let question = scan
                .0
                .iter()
                .find(|item| item.0 == "Rectangle" && item.1.starts_with("Recover"))
                .expect("question");
            found.push(("modal", question.2));
        }
        "transform-move" | "transform-scale" | "transform-crop" | "transform-stretch" => {
            let item = app_clip(app);
            let picture = scan.get("Frame", "Transform box");
            let (full_width, full_height) = (item.zoom_x * picture[2], item.zoom_y * picture[3]);
            let left = picture[0] + (0.5 + item.x) * picture[2] - full_width / 2.0;
            let top = picture[1] + (0.5 + item.y) * picture[3] - full_height / 2.0;
            let frame = [
                left + item.crop_left * full_width,
                top + item.crop_top * full_height,
                full_width * (1.0 - item.crop_left - item.crop_right),
                full_height * (1.0 - item.crop_top - item.crop_bottom),
            ];
            found.push(("box", grown(frame, 3.0)));
            found.push(("corner", grown([frame[0], frame[1], 0.0, 0.0], 9.0)));
            found.push((
                "edge-left",
                grown([frame[0], frame[1] + frame[3] / 2.0, 0.0, 0.0], 12.0),
            ));
            found.push((
                "edge-right",
                grown(
                    [frame[0] + frame[2], frame[1] + frame[3] / 2.0, 0.0, 0.0],
                    12.0,
                ),
            ));
        }
        _ => {}
    }
    found.push(("window", window));
    found
}

// a copy hides the settings and the view tabs, so it has its own names
fn copied(app: &App) -> Vec<(&'static str, Rect)> {
    let scan = Scan::new(app);
    let find = |start: &str| {
        let found = scan.0.iter().find(|item| item.1.starts_with(start));
        found.unwrap_or_else(|| panic!("no {start}")).2
    };
    let mode = find("Mode");
    let kept = find("Not kept");
    let panel = ElementHandle::find_by_element_type_name(app, "Panel")
        .next()
        .map(|panel| rect(&panel))
        .expect("panel");
    vec![
        ("mode", mode),
        (
            "copy",
            [
                panel[0],
                panel[1],
                panel[2],
                kept[1] + kept[3] + 10.0 - panel[1],
            ],
        ),
        (
            "window",
            [0.0, 0.0, demo::WINDOW.0 as f32, demo::WINDOW.1 as f32],
        ),
    ]
}

fn app_clip(app: &App) -> Clip {
    use slint::Model;
    app.get_clips().row_data(0).expect("clip")
}

fn dialog_names(window: &Dialog, name: &str, origin: (f32, f32)) -> Vec<(&'static str, Rect)> {
    let scan = Scan::new(window);
    let at = |item: Rect| shift(item, origin);
    let size = [
        origin.0,
        origin.1,
        demo::DIALOG.0 as f32,
        demo::DIALOG.1 as f32,
    ];
    let mut found = vec![
        ("dialog", size),
        ("tab", at(scan.get("Rectangle", "Components"))),
    ];
    let row = |label: &str| {
        let label = scan.get("Label", label);
        at([0.0, label[1], demo::DIALOG.0 as f32, label[3]])
    };
    match name {
        "general-settings" => {
            found.push(("folder", row("Render folder")));
            found.push(("tab", at(scan.get("Rectangle", "General"))));
        }
        "projects-settings" => {
            found.push(("projects", row("Project files")));
            found.push(("autosave", row("Autosave")));
            found.push(("tab", at(scan.get("Rectangle", "Projects"))));
        }
        _ => {
            found.push(("ffmpeg", row("Source")));
            let rife = [row("rife-v4.6"), row("rife-v4.26")];
            found.push(("rife", union(&rife)));
            found.push(("tensorrt", row("TensorRT")));
        }
    }
    found
}

fn main() {
    i_slint_backend_testing::init_no_event_loop();
    let app = demo::app(&[0; 160 * 90 * 4 * 8], &[255; 120], &[0; 2560 * 720 * 4]).unwrap();
    let (width, height) = demo::WINDOW;
    app.window()
        .set_size(slint::LogicalSize::new(width as f32, height as f32));
    app.show().unwrap();
    let window = demo::dialog().unwrap();
    let (dialog_width, dialog_height) = demo::DIALOG;
    window.window().set_size(slint::LogicalSize::new(
        dialog_width as f32,
        dialog_height as f32,
    ));
    window.show().unwrap();
    let origin = (
        (width - dialog_width) as f32 / 2.0,
        (height - dialog_height) as f32 / 2.0,
    );

    let by_id = |id: &str| {
        ElementHandle::find_by_element_id(&app, id)
            .next()
            .expect(id)
    };
    let [x, y, w, h] = rect(&by_id("App::stage"));
    let strip = 30.0;
    let picture = ElementQuery::from_root(&app)
        .match_accessible_role(AccessibleRole::Image)
        .match_predicate(|image| image.accessible_label().as_deref() == Some("Preview, result"))
        .find_first()
        .expect("preview");
    let [_, _, pane_w, pane_h] = rect(&picture);
    let shown = (pane_w * 9.0 / 16.0).min(pane_h);
    let preview = [x, y + strip + (h - strip - shown) / 2.0, w, shown];
    let render = ElementQuery::from_root(&app)
        .match_accessible_role(AccessibleRole::Button)
        .match_predicate(|button| button.accessible_label().as_deref() == Some("Render"))
        .find_first()
        .expect("render");
    let transport = ElementHandle::find_by_element_type_name(&app, "Transport")
        .next()
        .expect("transport");
    let panel = ElementHandle::find_by_element_type_name(&app, "Panel")
        .next()
        .expect("panel");
    let curve = |app: &App| {
        ElementHandle::find_by_element_type_name(app, "Curve")
            .next()
            .map(|curve| rect(&curve))
            .expect("curve")
    };

    let mut parts = vec![
        format!("\t\"window\": [{width}, {height}]"),
        format!(
            "\t\"base\": {}",
            json(
                &[
                    ("preview", preview),
                    ("timeline", rect(&by_id("App::timeline"))),
                    ("transport", rect(&transport)),
                    ("panel", rect(&panel)),
                    ("render", rect(&render)),
                ],
                1
            )
        ),
    ];

    let none = |_: &mut Settings| {};
    let set_method =
        |name: &'static str| move |settings: &mut Settings| settings.mask_method = name.into();

    parts.push(entry(
        "dedup",
        &rows(
            &open_panel(&app, "dedup", "Dedup", none),
            &[("repeats", "Repeats replaced")],
        ),
    ));
    parts.push(entry(
        "pre",
        &rows(
            &open_panel(&app, "pre", "Pre-interpolation", |settings| {
                settings.pre = true
            }),
            &[
                ("fps", "Frame rate"),
                ("backend", "Backend"),
                ("model", "Model"),
            ],
        ),
    ));
    parts.push(entry(
        "interpolation",
        &rows(
            &open_panel(&app, "interpolation", "Interpolation", none),
            &[
                ("engine", "Engine"),
                ("fps", "Frame rate"),
                ("speed", "Speed"),
                ("tuning", "Tuning"),
                ("look", "Look"),
                ("half", "Half-size analysis"),
            ],
        ),
    ));
    parts.push(entry(
        "rife",
        &rows(
            &open_panel(&app, "interpolation", "Interpolation", |settings| {
                settings.engine = "rife".into()
            }),
            &[
                ("engine", "Engine"),
                ("fps", "Frame rate"),
                ("backend", "Backend"),
                ("model", "Model"),
            ],
        ),
    ));
    parts.push(entry(
        "peel",
        &rows(
            &open_panel(&app, "masking", "Masking", set_method("peel")),
            &[("method", "Method"), ("limit", "Limit")],
        ),
    ));
    parts.push(entry(
        "still",
        &rows(
            &open_panel(&app, "masking", "Masking", set_method("still")),
            &[
                ("method", "Method"),
                ("limit", "Limit"),
                ("edge", "Edge"),
                ("tolerance", "Tolerance"),
            ],
        ),
    ));
    let blending = open_panel(&app, "blending", "Blending", none);
    let mut rects = rows(
        &blending,
        &[
            ("fps", "Frame rate"),
            ("intensity", "Intensity"),
            ("gamma", "Gamma"),
            ("weighting", "Weighting"),
        ],
    );
    rects.insert(1, ("curve", curve(&app)));
    parts.push(entry("blending", &rects));
    parts.push(entry(
        "color",
        &rows(
            &open_panel(&app, "color", "Color", none),
            &[
                ("brightness", "Brightness"),
                ("contrast", "Contrast"),
                ("saturation", "Saturation"),
                ("hue", "Hue"),
                ("lut", "LUT"),
            ],
        ),
    ));
    parts.push(entry(
        "upscale",
        &rows(
            &open_panel(&app, "upscale", "Upscale", none),
            &[("resolution", "Resolution"), ("method", "Method")],
        ),
    ));
    parts.push(entry(
        "output",
        &rows(
            &open_panel(&app, "output", "Output", none),
            &[
                ("codec", "Codec"),
                ("encoder", "Encoder"),
                ("target", "Target"),
                ("quality", "Quality"),
                ("effort", "Effort"),
                ("audio", "Audio tracks"),
                ("options", "Options"),
                ("container", "Container"),
            ],
        ),
    ));
    parts.push(entry(
        "size",
        &rows(
            &open_panel(&app, "output", "Output", |settings| {
                settings.target = "size".into()
            }),
            &[("size", "Size")],
        ),
    ));

    let image = open_panel(&app, "masking", "Masking", |settings| {
        settings.mask_method = "image".into();
        settings.mask_image = "chat".into();
    });
    let scan = Scan::new(&app);
    let mut rects = rows(&image, &[("method", "Method"), ("soft", "Soft edge")]);
    let gallery = ElementHandle::find_by_element_type_name(&app, "MaskGrid")
        .next()
        .expect("gallery");
    rects.push(("gallery", rect(&gallery)));
    rects.push(("add", scan.get("Rectangle", "Add mask")));
    rects.push(("chat", scan.get("Rectangle", "chat")));
    parts.push(entry("image", &rects));

    state(&app, "interpolation", |_| {});
    let mut scenes = Vec::new();
    for name in SCENES {
        demo::scene(&app, name).unwrap();
        i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_secs(2));
        // the page reveals the end of the panel again while the layout grows
        if name.starts_with("mask-") {
            demo::reveal(&app);
            i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_secs(2));
        }
        let mut found = match name {
            "lossless" => copied(&app),
            _ => names(&app, name),
        };
        if DIALOGS.contains(&name) {
            demo::tab(&window, name);
            i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_secs(2));
            found.extend(dialog_names(&window, name, origin));
        }
        scenes.push(format!("\t\t\"{name}\": {}", json(&found, 2)));
    }
    parts.push(format!("\t\"scenes\": {{\n{}\n\t}}", scenes.join(",\n")));

    println!("{{\n{}\n}}", parts.join(",\n"));
}

const SCENES: [&str; 35] = [
    "plain",
    "tracks",
    "moving",
    "track-menu",
    "drop",
    "selected",
    "scope-clip",
    "picked",
    "snap",
    "gap",
    "speed",
    "fades",
    "view-original",
    "view-split",
    "view-side",
    "held",
    "zoomed",
    "transform-move",
    "transform-scale",
    "transform-crop",
    "transform-stretch",
    "file-menu",
    "recent-menu",
    "recover",
    "components",
    "components-install",
    "projects-settings",
    "general-settings",
    "separate-menu",
    "rendering",
    "lossless",
    "share",
    "mask-add-menu",
    "mask-menu",
    "mask-editor",
];
const DIALOGS: [&str; 4] = [
    "components",
    "components-install",
    "projects-settings",
    "general-settings",
];
