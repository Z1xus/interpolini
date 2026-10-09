use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

const STEM: &str = "interpolini";

trait Value {
    fn read(&mut self, text: &str) -> bool;
    fn write(&self) -> String;
}

macro_rules! named {
    ($name:ident { $($variant:ident = $text:literal),+ }) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum $name {
            $($variant),+
        }

        impl $name {
            pub const ALL: &[Self] = &[$(Self::$variant),+];

            pub fn name(self) -> &'static str {
                match self {
                    $(Self::$variant => $text),+
                }
            }

            pub fn parse(text: &str) -> Option<Self> {
                Self::ALL.iter().copied().find(|value| value.name() == text)
            }
        }

        impl Value for &mut $name {
            fn read(&mut self, text: &str) -> bool {
                $name::parse(text).map(|value| **self = value).is_some()
            }

            fn write(&self) -> String {
                self.name().to_owned()
            }
        }
    };
}

named!(Engine { Svp = "svp", Rife = "rife" });
named!(Backend { Vulkan = "vulkan", Tensorrt = "tensorrt" });
named!(Speed { Medium = "medium", Fast = "fast", Faster = "faster", Fastest = "fastest" });
named!(Tuning { Weak = "weak", Smooth = "smooth", Film = "film", Animation = "animation" });
named!(Algorithm { Sharp = "2", Standard = "13", Smooth = "23" });
named!(Weighting {
    Equal = "equal",
    Gaussian = "gaussian",
    GaussianSym = "gaussian_sym",
    Pyramid = "pyramid",
    Ascending = "ascending",
    Descending = "descending",
    Vegas = "vegas",
    Custom = "custom"
});
named!(Resolution { P1080 = "1080p", P1440 = "1440p", P2160 = "2160p" });
named!(Method { Nearest = "nearest", Bilinear = "bilinear", Bicubic = "bicubic", Lanczos = "lanczos" });
named!(Codec { H264 = "h264", Hevc = "hevc", Av1 = "av1" });
named!(Encoder { Auto = "auto", Hardware = "hardware", Software = "software" });
named!(Container { Mp4 = "mp4", Mkv = "mkv", Mov = "mov" });
named!(Audio { Separate = "separate", Mix = "mix" });

impl Value for &mut bool {
    fn read(&mut self, text: &str) -> bool {
        let value = match text.to_lowercase().as_str() {
            "yes" | "true" | "on" | "1" => true,
            "no" | "false" | "off" | "0" => false,
            _ => return false,
        };
        **self = value;
        true
    }

    fn write(&self) -> String {
        if **self { "yes" } else { "no" }.to_owned()
    }
}

impl Value for &mut String {
    fn read(&mut self, text: &str) -> bool {
        **self = text.to_owned();
        true
    }

    fn write(&self) -> String {
        (**self).clone()
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Rate {
    Fps(u32),
    Times(f32),
}

impl Rate {
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim().to_lowercase();
        match text.strip_prefix('x').or_else(|| text.strip_suffix('x')) {
            Some(times) => {
                let times = times
                    .parse()
                    .ok()
                    .filter(|times| (1.0..=1000.0).contains(times));
                times.map(Self::Times)
            }
            None => text
                .parse()
                .ok()
                .filter(|fps| (2..=100_000).contains(fps))
                .map(Self::Fps),
        }
    }

    pub fn at(self, fps: f64) -> u32 {
        match self {
            Self::Fps(fps) => fps,
            Self::Times(times) => (fps * f64::from(times)).round() as u32,
        }
    }
}

impl std::fmt::Display for Rate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Fps(fps) => write!(f, "{fps}"),
            Self::Times(times) => write!(f, "{times}x"),
        }
    }
}

impl Value for &mut Rate {
    fn read(&mut self, text: &str) -> bool {
        Rate::parse(text).map(|rate| **self = rate).is_some()
    }

    fn write(&self) -> String {
        self.to_string()
    }
}

struct Within<'a, T>(&'a mut T, T, T);

impl<T: std::str::FromStr + PartialOrd + ToString> Value for Within<'_, T> {
    fn read(&mut self, text: &str) -> bool {
        let value = text
            .parse()
            .ok()
            .filter(|value| (&self.1..=&self.2).contains(&value));
        value.map(|value| *self.0 = value).is_some()
    }

    fn write(&self) -> String {
        self.0.to_string()
    }
}

struct Curve<'a>(&'a mut Weighting, &'a mut Vec<f32>);

pub fn curve(text: &str) -> Option<Vec<f32>> {
    let values: Option<Vec<f32>> = text
        .strip_prefix('[')?
        .strip_suffix(']')?
        .split(',')
        .map(|value| value.trim().parse().ok().filter(|value| *value > 0.0))
        .collect();
    values.filter(|values| !values.is_empty())
}

pub fn list(values: &[f32]) -> String {
    let values: Vec<String> = values.iter().map(f32::to_string).collect();
    format!("[{}]", values.join(", "))
}

impl Value for Curve<'_> {
    fn read(&mut self, text: &str) -> bool {
        if let Some(values) = curve(text) {
            *self.1 = values;
            *self.0 = Weighting::Custom;
            return true;
        }
        self.0.read(text)
    }

    fn write(&self) -> String {
        match *self.0 {
            Weighting::Custom => list(self.1),
            weighting => weighting.name().to_owned(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Dedup {
    pub enabled: bool,
    pub threshold: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Pre {
    pub enabled: bool,
    pub fps: Rate,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Interpolation {
    pub enabled: bool,
    pub engine: Engine,
    pub fps: Rate,
    pub speed: Speed,
    pub tuning: Tuning,
    pub algorithm: Algorithm,
    pub half: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Rife {
    pub backend: Backend,
    pub model: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Mask {
    pub enabled: bool,
    pub limit: f32,
    pub edge: f32,
    pub tolerance: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Blending {
    pub enabled: bool,
    pub fps: u32,
    pub intensity: f32,
    pub weighting: Weighting,
    pub custom: Vec<f32>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Color {
    pub enabled: bool,
    pub brightness: f32,
    pub contrast: f32,
    pub saturation: f32,
    pub hue: f32,
    pub lut: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Upscale {
    pub enabled: bool,
    pub resolution: Resolution,
    pub method: Method,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Output {
    pub codec: Codec,
    pub encoder: Encoder,
    pub quality: u32,
    pub container: Container,
    pub audio: Audio,
    pub options: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    pub dedup: Dedup,
    pub pre: Pre,
    pub interpolation: Interpolation,
    pub rife: Rife,
    pub mask: Mask,
    pub blending: Blending,
    pub color: Color,
    pub upscale: Upscale,
    pub output: Output,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            dedup: Dedup {
                enabled: false,
                threshold: 0.5,
            },
            pre: Pre {
                enabled: false,
                fps: Rate::Times(2.0),
            },
            interpolation: Interpolation {
                enabled: true,
                engine: Engine::Svp,
                fps: Rate::Fps(960),
                speed: Speed::Faster,
                tuning: Tuning::Weak,
                algorithm: Algorithm::Standard,
                half: true,
            },
            rife: Rife {
                backend: Backend::Vulkan,
                model: "rife-v4.6".into(),
            },
            // the defaults of open-svpflow
            mask: Mask {
                enabled: true,
                limit: 4.5,
                edge: 5.0,
                tolerance: 30.0,
            },
            blending: Blending {
                enabled: true,
                fps: 60,
                intensity: 1.0,
                weighting: Weighting::Equal,
                custom: vec![1.0, 2.0, 5.0],
            },
            color: Color {
                enabled: false,
                brightness: 1.0,
                contrast: 1.0,
                saturation: 1.0,
                hue: 0.0,
                lut: String::new(),
            },
            upscale: Upscale {
                enabled: false,
                resolution: Resolution::P2160,
                method: Method::Nearest,
            },
            output: Output {
                codec: Codec::H264,
                encoder: Encoder::Auto,
                quality: 20,
                container: Container::Mp4,
                audio: Audio::Mix,
                options: String::new(),
            },
        }
    }
}

type Field<'a> = (&'static str, &'static str, Box<dyn Value + 'a>);

impl Config {
    fn fields(&mut self) -> Vec<Field<'_>> {
        fn field<'a>(
            section: &'static str,
            key: &'static str,
            value: impl Value + 'a,
        ) -> Field<'a> {
            (section, key, Box::new(value))
        }
        let Self {
            dedup,
            pre,
            interpolation,
            rife,
            mask,
            blending,
            color,
            upscale,
            output,
        } = self;
        vec![
            field("dedup", "enabled", &mut dedup.enabled),
            field(
                "dedup",
                "threshold",
                Within(&mut dedup.threshold, 0.0, 255.0),
            ),
            field("pre-interpolation", "enabled", &mut pre.enabled),
            field("pre-interpolation", "fps", &mut pre.fps),
            field("interpolation", "enabled", &mut interpolation.enabled),
            field("interpolation", "engine", &mut interpolation.engine),
            field("interpolation", "fps", &mut interpolation.fps),
            field("interpolation", "speed", &mut interpolation.speed),
            field("interpolation", "tuning", &mut interpolation.tuning),
            field("interpolation", "algorithm", &mut interpolation.algorithm),
            field("interpolation", "half size", &mut interpolation.half),
            field("rife", "backend", &mut rife.backend),
            field("rife", "model", &mut rife.model),
            field("mask", "enabled", &mut mask.enabled),
            field("mask", "limit", Within(&mut mask.limit, 0.0, 255.0)),
            field("mask", "edge", Within(&mut mask.edge, 0.0, 255.0)),
            field("mask", "tolerance", Within(&mut mask.tolerance, 0.0, 100.0)),
            field("blending", "enabled", &mut blending.enabled),
            field("blending", "fps", Within(&mut blending.fps, 1, 1000)),
            field(
                "blending",
                "intensity",
                Within(&mut blending.intensity, 0.0, 10.0),
            ),
            field(
                "blending",
                "weighting",
                Curve(&mut blending.weighting, &mut blending.custom),
            ),
            field("color", "enabled", &mut color.enabled),
            field(
                "color",
                "brightness",
                Within(&mut color.brightness, 0.0, 4.0),
            ),
            field("color", "contrast", Within(&mut color.contrast, 0.0, 4.0)),
            field(
                "color",
                "saturation",
                Within(&mut color.saturation, 0.0, 4.0),
            ),
            field("color", "hue", Within(&mut color.hue, -360.0, 360.0)),
            field("color", "lut", &mut color.lut),
            field("upscale", "enabled", &mut upscale.enabled),
            field("upscale", "resolution", &mut upscale.resolution),
            field("upscale", "method", &mut upscale.method),
            field("output", "codec", &mut output.codec),
            field("output", "encoder", &mut output.encoder),
            field("output", "quality", Within(&mut output.quality, 0, 51)),
            field("output", "container", &mut output.container),
            field("output", "audio", &mut output.audio),
            field("output", "options", &mut output.options),
        ]
    }

    pub fn apply(&mut self, text: &str) -> Vec<String> {
        let mut values = HashMap::new();
        let mut section = String::new();
        for line in text.lines().map(str::trim) {
            if line.is_empty() || line.starts_with([';', '#']) {
                continue;
            }
            if let Some(name) = line
                .strip_prefix('[')
                .and_then(|line| line.strip_suffix(']'))
            {
                section = name.trim().to_lowercase();
            } else if let Some((key, value)) = line.split_once(':') {
                values.insert(
                    (section.clone(), key.trim().to_lowercase()),
                    value.trim().to_owned(),
                );
            }
        }
        let mut warnings = Vec::new();
        for (section, key, mut value) in self.fields() {
            if let Some(text) = values.remove(&(section.to_owned(), key.to_owned()))
                && !value.read(&text)
            {
                warnings.push(format!("[{section}] {key}: \"{text}\" is not valid"));
            }
        }
        let mut unknown: Vec<String> = values
            .keys()
            .map(|(section, key)| format!("[{section}] {key}: unknown setting"))
            .collect();
        unknown.sort();
        warnings.extend(unknown);
        warnings
    }

    pub fn to_ini(&self) -> String {
        let mut text = String::new();
        let mut last = "";
        for (section, key, value) in self.clone().fields() {
            if section != last {
                let gap = if last.is_empty() { "" } else { "\n" };
                text.push_str(&format!("{gap}[{section}]\n"));
                last = section;
            }
            text.push_str(&format!("{key}: {}\n", value.write()));
        }
        text
    }
}

// the path is read once, an update moves the files of the running app
pub fn exe() -> &'static Path {
    static EXE: OnceLock<PathBuf> = OnceLock::new();
    EXE.get_or_init(|| std::env::current_exe().unwrap_or_default())
}

pub fn app_dir() -> PathBuf {
    exe().parent().map(Path::to_owned).unwrap_or_default()
}

#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub name: String,
    pub path: PathBuf,
    pub local: bool,
}

pub fn path(directory: &Path, name: &str) -> PathBuf {
    match name {
        "default" => directory.join(format!("{STEM}.ini")),
        name => directory.join(format!("{STEM}.{name}.ini")),
    }
}

fn scan(directory: &Path, local: bool) -> Vec<Entry> {
    let files = std::fs::read_dir(directory).into_iter().flatten().flatten();
    let mut entries: Vec<Entry> = files
        .filter_map(|file| {
            let name = file.file_name().into_string().ok()?;
            let name = name.strip_prefix(STEM)?.strip_suffix(".ini")?;
            let name = match name {
                "" => "default",
                name => name.strip_prefix('.')?,
            };
            Some(Entry {
                name: name.to_owned(),
                path: file.path(),
                local,
            })
        })
        .collect();
    entries.sort_by_key(|entry| (entry.name != "default", entry.name.clone()));
    entries
}

pub fn entries(clip: &Path) -> Vec<Entry> {
    // a clip that is named without a folder is in the current folder
    let folder = clip
        .parent()
        .filter(|folder| !folder.as_os_str().is_empty());
    let mut entries = scan(folder.unwrap_or(Path::new(".")), true);
    let shared = scan(&app_dir(), false);
    let known = |entry: &Entry| entries.iter().any(|local| local.path == entry.path);
    let shared: Vec<Entry> = shared.into_iter().filter(|entry| !known(entry)).collect();
    entries.extend(shared);
    entries
}

pub struct Loaded {
    pub config: Config,
    pub entry: Option<Entry>,
    pub warnings: Vec<String>,
}

pub fn read(entry: Entry) -> Loaded {
    let mut config = Config::default();
    let text = std::fs::read_to_string(&entry.path).unwrap_or_default();
    let warnings = config.apply(&text);
    Loaded {
        config,
        entry: Some(entry),
        warnings,
    }
}

pub fn load(clip: &Path, global: bool, name: &str) -> Loaded {
    let found = entries(clip)
        .into_iter()
        .find(|entry| entry.name == name && !(global && entry.local));
    match found {
        Some(entry) => read(entry),
        None => Loaded {
            config: Config::default(),
            entry: None,
            warnings: Vec::new(),
        },
    }
}

impl Resolution {
    pub fn side(self) -> u32 {
        match self {
            Self::P1080 => 1080,
            Self::P1440 => 1440,
            Self::P2160 => 2160,
        }
    }
}

impl Backend {
    // the file that makes a folder in rife/ a model for this backend
    pub fn model(self) -> &'static str {
        match self {
            Self::Vulkan => "flownet.param",
            Self::Tensorrt => "rife.onnx",
        }
    }
}

pub fn models(backend: Backend) -> Vec<String> {
    let folders = std::fs::read_dir(app_dir().join("rife"))
        .into_iter()
        .flatten()
        .flatten();
    let mut models: Vec<String> = folders
        .filter(|folder| folder.path().join(backend.model()).exists())
        .filter_map(|folder| folder.file_name().into_string().ok())
        .collect();
    models.sort();
    models
}

pub fn create() {
    let path = path(&app_dir(), "default");
    if !path.exists() {
        let _ = std::fs::write(path, Config::default().to_ini());
    }
}
