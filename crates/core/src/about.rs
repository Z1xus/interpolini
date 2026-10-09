use std::process::Command;

use ffmpeg_next::encoder;

use crate::config::{self, Backend};

const ENCODERS: [&str; 14] = [
    "h264_nvenc",
    "hevc_nvenc",
    "av1_nvenc",
    "h264_amf",
    "hevc_amf",
    "av1_amf",
    "h264_qsv",
    "hevc_qsv",
    "av1_qsv",
    "h264_videotoolbox",
    "hevc_videotoolbox",
    "libx264",
    "libx265",
    "libsvtav1",
];

fn run(program: &str, arguments: &[&str]) -> Option<String> {
    let output = Command::new(program).args(arguments).output().ok()?;
    let text = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    (output.status.success() && !text.is_empty()).then_some(text)
}

#[cfg(target_os = "linux")]
fn system() -> Vec<(&'static str, String)> {
    let read = |path: &str| std::fs::read_to_string(path).unwrap_or_default();
    let field = |text: &str, key: &str| {
        let line = text.lines().find_map(|line| line.strip_prefix(key));
        line.map(|value| value.trim_matches([' ', '\t', ':', '=', '"']).to_owned())
    };
    let variable = |name: &str| std::env::var(name).unwrap_or_default();
    // each gpu is a card in drm, with the driver that runs it and its pci id
    let cards = std::fs::read_dir("/sys/class/drm").into_iter().flatten();
    let gpus = cards.flatten().filter_map(|card| {
        let device = read(&card.path().join("device/uevent").to_string_lossy());
        let numbered = card
            .file_name()
            .to_string_lossy()
            .trim_start_matches("card")
            .parse::<u32>();
        let (driver, id) = (field(&device, "DRIVER")?, field(&device, "PCI_ID")?);
        numbered.is_ok().then(|| format!("{driver} {id}"))
    });
    vec![
        (
            "system",
            field(&read("/etc/os-release"), "PRETTY_NAME").unwrap_or_default(),
        ),
        (
            "kernel",
            read("/proc/sys/kernel/osrelease").trim().to_owned(),
        ),
        (
            "desktop",
            format!(
                "{} {}",
                variable("XDG_CURRENT_DESKTOP"),
                variable("XDG_SESSION_TYPE")
            ),
        ),
        (
            "cpu",
            field(&read("/proc/cpuinfo"), "model name").unwrap_or_default(),
        ),
        (
            "memory",
            field(&read("/proc/meminfo"), "MemTotal").unwrap_or_default(),
        ),
        ("gpu", gpus.collect::<Vec<_>>().join(", ")),
        (
            "nvidia driver",
            read("/sys/module/nvidia/version").trim().to_owned(),
        ),
    ]
}

#[cfg(windows)]
fn system() -> Vec<(&'static str, String)> {
    let shell = |script: &str| run("powershell", &["-NoProfile", "-Command", script]);
    let list = |class: &str, fields: &str| {
        let script = format!(
            "Get-CimInstance {class} | ForEach-Object {{ {fields} }} | Out-String -Width 400"
        );
        shell(&script)
            .unwrap_or_default()
            .lines()
            .collect::<Vec<_>>()
            .join(", ")
    };
    vec![
        ("system", run("cmd", &["/c", "ver"]).unwrap_or_default()),
        (
            "cpu",
            std::env::var("PROCESSOR_IDENTIFIER").unwrap_or_default(),
        ),
        (
            "memory",
            list(
                "Win32_ComputerSystem",
                "[string][math]::Round($_.TotalPhysicalMemory / 1GB) + ' GB'",
            ),
        ),
        (
            "gpu",
            list("Win32_VideoController", "$_.Name + ' ' + $_.DriverVersion"),
        ),
    ]
}

#[cfg(target_os = "macos")]
fn system() -> Vec<(&'static str, String)> {
    let displays = run("system_profiler", &["SPDisplaysDataType"]).unwrap_or_default();
    let chips = displays
        .lines()
        .filter_map(|line| line.trim().strip_prefix("Chipset Model: "));
    vec![
        (
            "system",
            run("sw_vers", &["-productVersion"]).unwrap_or_default(),
        ),
        (
            "cpu",
            run("sysctl", &["-n", "machdep.cpu.brand_string"]).unwrap_or_default(),
        ),
        ("gpu", chips.collect::<Vec<_>>().join(", ")),
    ]
}

// what a person needs to see to help with a problem: the build, the system and what is installed
pub fn about() -> String {
    let directory = config::app_dir();
    let present = |name: &str| directory.join(libloading::library_filename(name)).exists();
    let installed = |present: bool| if present { "yes" } else { "no" };
    let version = ffmpeg_next::format::version();
    let encoders = ENCODERS
        .iter()
        .filter(|name| encoder::find_by_name(name).is_some());
    let nvidia = run(
        "nvidia-smi",
        &[
            "--query-gpu=name,driver_version,memory.total",
            "--format=csv,noheader",
        ],
    );
    let mut lines = vec![(
        "os",
        format!("{} {}", std::env::consts::OS, std::env::consts::ARCH),
    )];
    lines.extend(system());
    lines.extend([
        ("nvidia", nvidia.unwrap_or_default().replace('\n', ", ")),
        (
            "libavformat",
            format!(
                "{}.{}.{}",
                version >> 16,
                (version >> 8) & 0xff,
                version & 0xff
            ),
        ),
        (
            "encoders in ffmpeg",
            encoders.copied().collect::<Vec<_>>().join(" "),
        ),
        (
            "open-svpflow library",
            installed(present("open_svpflow")).into(),
        ),
        (
            "rife on vulkan",
            installed(present("interpolini_rife")).into(),
        ),
        ("vulkan models", config::models(Backend::Vulkan).join(" ")),
        (
            "rife on tensorrt",
            installed(present("interpolini_rife_trt")).into(),
        ),
        (
            "tensorrt libraries",
            installed(crate::tensorrt::libraries()).into(),
        ),
        (
            "tensorrt models",
            config::models(Backend::Tensorrt).join(" "),
        ),
    ]);
    let lines = lines.into_iter().filter(|line| !line.1.trim().is_empty());
    let mut text: Vec<String> = lines
        .map(|(name, value)| format!("{name}: {}", value.trim()))
        .collect();
    // the package has a file that says what it was built from
    let build = std::fs::read_to_string(directory.join("BUILD.txt")).unwrap_or_default();
    // after the commits and the versions come the details of the compiler
    text.extend(build.lines().take(5).map(str::to_lowercase));
    text.join("\n")
}
