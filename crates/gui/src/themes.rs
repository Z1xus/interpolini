use slint::{Color, ComponentHandle, ModelRc, VecModel};

use crate::{App, Piece, Theme};

// void, rack, raised, seam, ink, muted, faint, accent, ok
type Palette = [u32; 9];

struct Art {
    theme: &'static str,
    area: &'static str,
    mask: &'static str,
    picture: &'static str,
    // 0 is left or top, 1 is right or bottom
    x: f32,
    y: f32,
    // as parts of the width of the area and of full opacity
    width: f32,
    opacity: f32,
}

const ART: [Art; 2] = [
    Art {
        theme: "ctt",
        area: "timeline",
        mask: "ctt.mask.png",
        picture: "ctt.png",
        x: 0.5,
        y: 1.0,
        width: 1.0,
        opacity: 0.4,
    },
    Art {
        theme: "ctt",
        area: "start",
        mask: "ctt.mask.png",
        picture: "ctt.png",
        x: 1.0,
        y: 1.0,
        width: 0.8,
        opacity: 1.0,
    },
];

pub const THEMES: [(&str, Palette, Option<Palette>); 12] = [
    (
        "interpolini",
        [
            0x0a0a0a, 0x131313, 0x1c1c1c, 0x2a2a2a, 0xececec, 0x9c9c9c, 0x8a8a8a, 0xe8483a,
            0x6fbf8b,
        ],
        Some([
            0xf4f4f4, 0xffffff, 0xf0f0f0, 0xd6d6d6, 0x161616, 0x5c5c5c, 0x6e6e6e, 0xc93426,
            0x1a7f37,
        ]),
    ),
    (
        "smoothie to go",
        [
            0x030303, 0x0c0c0c, 0x161616, 0x242424, 0xe8e8e8, 0x959595, 0x7b7b7b, 0x2cc4e4,
            0x4fcb7a,
        ],
        None,
    ),
    (
        "vtrl",
        [
            0x09090d, 0x121118, 0x1b1924, 0x26242e, 0xcecece, 0x8d8a96, 0x827f8c, 0xa246d9,
            0x7ef2a7,
        ],
        None,
    ),
    (
        "z1xus",
        [
            0x061923, 0x0c2431, 0x1a2e41, 0x2c4257, 0xe7ddd0, 0xb6b2ad, 0x8a95a2, 0xe1a268,
            0x9fc9a2,
        ],
        None,
    ),
    (
        "aech",
        [
            0x0f0f0f, 0x161616, 0x1f1f1f, 0x3a3a3a, 0xa6ff8f, 0x6fbf5f, 0x5f9c53, 0xb84df5,
            0x39ff14,
        ],
        None,
    ),
    (
        "aetopia",
        [
            0x050505, 0x0d0d0d, 0x171717, 0x2a2a2a, 0xededed, 0xa3a3a3, 0x858585, 0xff0000,
            0x7fc98f,
        ],
        None,
    ),
    (
        "ctt",
        [
            0x171920, 0x1e2029, 0x2e2c3e, 0x3d3b52, 0xe2e4e9, 0xa3a6b4, 0x8a8da0, 0x7c4dff,
            0x7ef2a7,
        ],
        Some([
            0xf5f5f7, 0xffffff, 0xf0f0f5, 0xd8d8e0, 0x1e2029, 0x5a5d6b, 0x6b6e7c, 0x7c4dff,
            0x1a7f37,
        ]),
    ),
    (
        "frost",
        [
            0x000000, 0x05080d, 0x0b111a, 0x15202e, 0xdbe9f7, 0x8fa3b8, 0x6f8399, 0x2a8bea,
            0x6fd3c1,
        ],
        None,
    ),
    (
        "tekno",
        [
            0x000000, 0x0a0a0a, 0x141414, 0x333333, 0xffffff, 0xa3a3a3, 0x808080, 0x4ade80,
            0x7ef2a7,
        ],
        Some([
            0xf8f0eb, 0xfdf8f5, 0xefe6e0, 0xc6c0bc, 0x000000, 0x525252, 0x737373, 0x16a34a,
            0x1a7f37,
        ]),
    ),
    (
        "github",
        [
            0x010409, 0x0d1117, 0x161b22, 0x30363d, 0xe6edf3, 0x8d96a0, 0x7d8590, 0x2f81f7,
            0x3fb950,
        ],
        Some([
            0xf6f8fa, 0xffffff, 0xf6f8fa, 0xd0d7de, 0x1f2328, 0x656d76, 0x6e7781, 0x0969da,
            0x1a7f37,
        ]),
    ),
    (
        "github dimmed",
        [
            0x1c2128, 0x22272e, 0x2d333b, 0x444c56, 0xadbac7, 0x909dab, 0x8b96a3, 0x539bf5,
            0x57ab5a,
        ],
        None,
    ),
    (
        "github colorblind",
        [
            0x010409, 0x0d1117, 0x161b22, 0x30363d, 0xe6edf3, 0x8d96a0, 0x7d8590, 0xd47616,
            0x58a6ff,
        ],
        Some([
            0xf6f8fa, 0xffffff, 0xf6f8fa, 0xd0d7de, 0x1f2328, 0x656d76, 0x6e7781, 0xb35900,
            0x0969da,
        ]),
    ),
];

pub fn has_light(name: &str) -> bool {
    THEMES
        .iter()
        .any(|theme| theme.0 == name && theme.2.is_some())
}

pub fn paint(theme: &Theme<'_>, name: &str, light: bool) {
    let Some((_, dark, bright)) = THEMES.iter().find(|theme| theme.0 == name) else {
        return;
    };
    let palette = bright.filter(|_| light).unwrap_or(*dark);
    let [void, rack, raised, seam, ink, muted, faint, accent, ok] = palette
        .map(|color| Color::from_rgb_u8((color >> 16) as u8, (color >> 8) as u8, color as u8));
    theme.set_void(void);
    theme.set_rack(rack);
    theme.set_raised(raised);
    theme.set_seam(seam);
    theme.set_ink(ink);
    theme.set_muted(muted);
    theme.set_faint(faint);
    theme.set_accent(accent);
    theme.set_ok(ok);
}

pub fn apply(ui: &App, name: &str, light: bool) {
    paint(&ui.global::<Theme>(), name, light);

    let folder = interpolini_core::config::app_dir().join("themes");
    let image = |file: &str| slint::Image::load_from_path(&folder.join(file)).unwrap_or_default();
    let pieces = ART.iter().filter(|art| art.theme == name);
    let pieces = pieces.map(|art| Piece {
        area: art.area.into(),
        mask: image(art.mask),
        picture: image(art.picture),
        x: art.x,
        y: art.y,
        size: art.width,
        opacity: art.opacity,
    });
    ui.set_art(ModelRc::new(VecModel::from(pieces.collect::<Vec<_>>())));
}
