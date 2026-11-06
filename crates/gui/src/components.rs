use std::cell::RefCell;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::Path;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use interpolini_core::config;
use interpolini_core::work::Cancel;
use sha2::{Digest, Sha256};
use slint::{ComponentHandle, ModelRc, VecModel, Weak};

#[cfg(target_os = "linux")]
use crate::icon;
use crate::{App, Download, Preferences, Theme, ffmpeg, services, settings, themes};

const MODELS_URL: &str = "https://raw.githubusercontent.com/TNTwise/rife-ncnn-vulkan/13338e38debe2e400b3eeecf6792312d01a692f9/models/";
// the folder in rife/, and the sha256 of its flownet.bin and flownet.param
const MODELS: [(&str, &str, &str); 6] = [
    (
        "rife-v4.6",
        "f334ed2260149ce0188a6dcf049844e8b0cdd912e01cbcfb63553157d2508958",
        "724569596bcd1e7b9fa50455c604777ebed99746d2ef40aa86e31b5725f1053c",
    ),
    (
        "rife-v4.15-lite",
        "700f6d15fc365dfd99a8fb7b4f9986708d4e9fb89844f3ddc4b56e7f30d16cda",
        "18bc572aa0cd78ab7242e60840cdde0feae37dd319d2a7b108989d83cc6479f1",
    ),
    (
        "rife-v4.22-lite",
        "792da5d62937199886bf8bb781ab8e0eddf663854d57e4a43cd7b32e4e81b29b",
        "091119e19a5891a5477de8f90b3c9e979baece5ee57c27d4e4138a6cc2552cc3",
    ),
    (
        "rife-v4.25",
        "10de487a095e61cb2971c39e3b5e17005a70fba6201c77fb96e063f4423b583f",
        "6ba231fb00e4ae82b120f938d9b2df91db32fbf322bd110f29450efaf61848d6",
    ),
    (
        "rife-v4.25-lite",
        "350a15e464bea5ad378e06c0fb43996e90a0d35653d5a6ef6bc980d832538fb7",
        "e55b76f197b89637dc824ce89e64878df419214d3cf6a10aea0283c61b93a908",
    ),
    (
        "rife-v4.26",
        "94d58e30b75d7c7609cfa6f3bdad524deddd14f5f75e85c36d2f827ef5c64731",
        "79f16c28903f93f8308f0c4c947f8c7e0c17d99a57b85473e5f298dd578d137b",
    ),
];
// yes we download python wheels to get cpp libraries. say thanks to nvidia
type Wheel = (&'static str, &'static str, &'static [&'static str]);
#[cfg(target_os = "linux")]
const WHEELS: &[Wheel] = &[
    (
        "https://pypi.nvidia.com/tensorrt-cu12-libs/tensorrt_cu12_libs-10.9.0.34-py2.py3-none-manylinux_2_28_x86_64.whl",
        "4a82f0bda2874596f202f6edc8dae99b86a3c4ec2fa142a9c847c4d3a57864a0",
        &[
            "tensorrt_libs/libnvinfer.so.10",
            "tensorrt_libs/libnvinfer_builder_resource.so.10.9.0",
            "tensorrt_cu12_libs-10.9.0.34.dist-info/LICENSE.txt",
        ],
    ),
    (
        "https://pypi.nvidia.com/nvidia-cuda-runtime-cu12/nvidia_cuda_runtime_cu12-12.8.90-py3-none-manylinux2014_x86_64.manylinux_2_17_x86_64.whl",
        "adade8dcbd0edf427b7204d480d6066d33902cab2a4707dcfc48a2d0fd44ab90",
        &[
            "nvidia/cuda_runtime/lib/libcudart.so.12",
            "nvidia_cuda_runtime_cu12-12.8.90.dist-info/License.txt",
        ],
    ),
];
#[cfg(windows)]
const WHEELS: &[Wheel] = &[
    (
        "https://pypi.nvidia.com/tensorrt-cu12-libs/tensorrt_cu12_libs-10.9.0.34-py2.py3-none-win_amd64.whl",
        "e43d38cc380d615bf8afab637c265f01a9452dc23deb9b9dd47b7662edf24531",
        &[
            "tensorrt_libs/nvinfer_10.dll",
            "tensorrt_libs/nvinfer_builder_resource_10.dll",
            "tensorrt_cu12_libs-10.9.0.34.dist-info/LICENSE.txt",
        ],
    ),
    (
        "https://pypi.nvidia.com/nvidia-cuda-runtime-cu12/nvidia_cuda_runtime_cu12-12.8.90-py3-none-win_amd64.whl",
        "c0c6027f01505bfed6c3b21ec546f69c687689aad5f1a377554bc6ca4aa993a8",
        &[
            "nvidia/cuda_runtime/bin/cudart64_12.dll",
            "nvidia_cuda_runtime_cu12-12.8.90.dist-info/License.txt",
        ],
    ),
];
#[cfg(not(any(target_os = "linux", windows)))]
const WHEELS: &[Wheel] = &[];
pub type Outcome = Result<(), String>;
// how much of a download is done, from 0 to 1, and a line about it
pub type Report<'a> = &'a dyn Fn(f32, String);

pub fn text(error: impl ToString) -> String {
    error.to_string()
}

pub fn download(url: &str, sha: &str, to: &Path, report: Report, stop: &Cancel) -> Outcome {
    // two copies of the app can download the same file
    let part = to.with_extension(format!("{}.part", std::process::id()));
    let outcome =
        fetch(url, sha, &part, report, stop).and_then(|()| fs::rename(&part, to).map_err(text));
    if outcome.is_err() {
        let _ = fs::remove_file(&part);
    }
    outcome
}

fn fetch(url: &str, sha: &str, part: &Path, report: Report, stop: &Cancel) -> Outcome {
    // a stalled connection fails, so a closing app does not wait for it
    let config = ureq::Agent::config_builder()
        .timeout_resolve(Some(Duration::from_secs(15)))
        .timeout_connect(Some(Duration::from_secs(15)))
        .timeout_recv_response(Some(Duration::from_secs(30)))
        .timeout_per_read(Some(Duration::from_secs(30)))
        .build();
    let mut response = ureq::Agent::new_with_config(config)
        .get(url)
        .call()
        .map_err(text)?;
    let length = response.headers().get("content-length");
    let total = length
        .and_then(|length| length.to_str().ok()?.parse::<u64>().ok())
        .unwrap_or(0);
    let mut reader = response.body_mut().as_reader();
    let mut file = File::create(part).map_err(text)?;
    let mut hash = Sha256::new();
    let mut buffer = vec![0; 1 << 20];
    let started = Instant::now();
    let mut told = started;
    let mut done = 0;
    loop {
        let count = reader.read(&mut buffer).map_err(text)?;
        if count == 0 || stop.stopped() {
            break;
        }
        file.write_all(&buffer[..count]).map_err(text)?;
        hash.update(&buffer[..count]);
        done += count as u64;
        if told.elapsed() >= Duration::from_millis(100) {
            told = Instant::now();
            let speed = done as f64 / started.elapsed().as_secs_f64();
            let left = (total.saturating_sub(done) as f64 / speed.max(1.0)) as u64;
            let line = format!(
                "{} of {} MB  |  {:.1} MB/s  |  {}:{:02} left",
                done >> 20,
                total >> 20,
                speed / f64::from(1 << 20),
                left / 60,
                left % 60
            );
            report(done as f32 / total.max(1) as f32, line);
        }
    }
    let hash = hex(&hash.finalize());
    if stop.stopped() || hash != sha {
        return Err(match stop.stopped() {
            true => String::new(),
            false => "The download is damaged. Try again.".into(),
        });
    }
    Ok(())
}

// a download writes at least every 30 s, so an older part is left from a copy of the app that stopped
fn abandoned(folder: &Path) {
    for file in fs::read_dir(folder).into_iter().flatten().flatten() {
        let old = file
            .metadata()
            .and_then(|meta| meta.modified())
            .is_ok_and(|time| time.elapsed().unwrap_or_default() > Duration::from_secs(120));
        if old && file.path().extension().is_some_and(|kind| kind == "part") {
            let _ = fs::remove_file(file.path());
        }
    }
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn install_model(name: &str, report: Report, stop: &Cancel) -> Outcome {
    let (_, weights, graph) = MODELS.into_iter().find(|model| model.0 == name).ok_or("")?;
    let folder = config::app_dir().join("rife").join(name);
    fs::create_dir_all(&folder).map_err(text)?;
    // the file that marks the model as installed comes last
    for (file, sha) in [("flownet.bin", weights), (config::MODEL, graph)] {
        let url = format!("{MODELS_URL}{name}/{file}");
        download(&url, sha, &folder.join(file), report, stop)?;
    }
    Ok(())
}

fn unpack_tensorrt(folder: &Path, report: Report, stop: &Cancel) -> Outcome {
    for (url, sha, files) in WHEELS {
        let wheel = folder.join("wheel.zip");
        download(url, sha, &wheel, report, stop)?;
        report(1.0, "Unpacking…".into());
        let mut archive = zip::ZipArchive::new(File::open(&wheel).map_err(text)?).map_err(text)?;
        for file in *files {
            let mut entry = archive.by_name(file).map_err(text)?;
            let name = Path::new(file).file_name().unwrap_or_default();
            let mut target = File::create(folder.join(name)).map_err(text)?;
            std::io::copy(&mut entry, &mut target).map_err(text)?;
        }
        fs::remove_file(wheel).map_err(text)?;
    }
    Ok(())
}

fn install_tensorrt(report: Report, stop: &Cancel) -> Outcome {
    // the folder appears complete or not at all
    let folder = config::app_dir().join("tensorrt");
    let part = folder.with_extension("part");
    let _ = fs::remove_dir_all(&part);
    fs::create_dir_all(&part).map_err(text)?;
    let old = folder.with_extension("old");
    let _ = fs::remove_dir_all(&old);
    let done = unpack_tensorrt(&part, report, stop).and_then(|()| {
        // windows cannot remove a loaded library, so the old folder moves away first
        let moved = fs::rename(&folder, &old).is_ok();
        if let Err(error) = fs::rename(&part, &folder) {
            if moved {
                let _ = fs::rename(&old, &folder);
            }
            return Err(text(error));
        }
        let _ = fs::remove_dir_all(&old);
        Ok(())
    });
    if done.is_err() {
        let _ = fs::remove_dir_all(&part);
    }
    done?;
    // tensorrt is of no use without a model
    match config::models().is_empty() {
        true => install_model(MODELS[0].0, report, stop),
        false => Ok(()),
    }
}

fn remove(name: &str) -> Outcome {
    let app = config::app_dir();
    if name == "tensorrt" {
        return fs::remove_dir_all(app.join("tensorrt")).map_err(text);
    }
    let folder = app.join("rife").join(name);
    for file in fs::read_dir(&folder).map_err(text)?.flatten() {
        let path = file.path();
        let engine = path.extension().is_some_and(|kind| kind == "engine");
        if engine || file.file_name().to_string_lossy().starts_with("flownet.") {
            fs::remove_file(path).map_err(text)?;
        }
    }
    forget(name);
    let _ = fs::remove_dir(folder);
    Ok(())
}

// a model of the old tensorrt keeps its files until its ncnn files are in
pub fn unfinished(name: &str) -> bool {
    let folder = config::app_dir().join("rife").join(name);
    MODELS.iter().any(|model| model.0 == name)
        && folder.join("rife.onnx").exists()
        && !folder.join(config::MODEL).exists()
}

fn forget(name: &str) {
    let folder = config::app_dir().join("rife").join(name);
    for old in ["rife.onnx", "rife.engine", "model.7z"] {
        let _ = fs::remove_file(folder.join(old));
    }
}

fn clean() {
    let app = config::app_dir();
    for model in MODELS.iter().filter(|model| !unfinished(model.0)) {
        forget(model.0);
    }
    for model in MODELS {
        abandoned(&app.join("rife").join(model.0));
    }
    abandoned(&app.join("tensorrt"));
    for file in fs::read_dir(app.join("tensorrt"))
        .into_iter()
        .flatten()
        .flatten()
    {
        if file.file_name().to_string_lossy().contains("nvonnxparser") {
            let _ = fs::remove_file(file.path());
        }
    }
}

// runs on the install slot after each download, so a cancelled one goes on with the next
fn replace(ui: Weak<App>, stop: &Cancel) {
    let mut replaced = false;
    for model in MODELS.iter().filter(|model| unfinished(model.0)) {
        if stop.stopped() {
            break;
        }
        if install_model(model.0, &|_, _| {}, stop).is_ok() {
            forget(model.0);
            replaced = true;
        }
    }
    if replaced {
        let _ = ui.upgrade_in_event_loop(|ui| ui.invoke_models_changed());
    }
}

fn installed() -> bool {
    interpolini_core::plugin() && interpolini_core::libraries()
}

fn show(ui: &App) {
    let support = interpolini_core::support();
    ui.set_tensorrt_need(support.need().into());
    ui.set_tensorrt_fixable(support.fixable());
}

fn refresh(ui: &App, window: &Preferences) {
    let have = config::models();
    let models = MODELS.iter().map(|model| Download {
        name: model.0.into(),
        installed: have.iter().any(|name| name == model.0),
    });
    window.set_models(ModelRc::new(VecModel::from_iter(models)));
    window.set_tensorrt(installed());
    window.set_tensorrt_shipped(interpolini_core::plugin());
    let support = interpolini_core::support();
    let blocked = !support.fixable() && support != interpolini_core::Support::Ready;
    window.set_tensorrt_need(if blocked { support.need() } else { "" }.into());
    ffmpeg::show(window);
    show(ui);
    // the model list of the settings follows what is installed
    ui.invoke_reload();
}

pub fn finish(ui: &App, window: &Preferences, outcome: Outcome) {
    window.set_busy("".into());
    window.set_note(outcome.err().unwrap_or_default().into());
    refresh(ui, window);
}

pub fn wire(ui: &App) {
    ffmpeg::clean();
    clean();
    let weak = ui.as_weak();
    services::task("support", move |_| {
        interpolini_core::support();
        let _ = weak.upgrade_in_event_loop(|ui| show(&ui));
    });
    if MODELS.iter().any(|model| unfinished(model.0)) {
        let weak = ui.as_weak();
        // the downloads of the window share this slot, so two never write the same file
        services::with(|services| services.install.submit(move |stop| replace(weak, stop)));
    }
    // the window is made when it first shows: a window that waits hidden is in the task bar of the desktop
    let made: Rc<RefCell<Option<Preferences>>> = Rc::default();
    for section in [None, Some("Components")] {
        let (weak, made) = (ui.as_weak(), Rc::clone(&made));
        let open = move || {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let mut made = made.borrow_mut();
            let make = || {
                Preferences::new()
                    .ok()
                    .inspect(|window| attach(&ui, window))
            };
            let Some(window) = reuse(&mut made, make) else {
                return;
            };
            if let Some(section) = section {
                window.set_section(section.into());
            }
            window.set_prefs(ui.get_prefs());
            window.set_updates(ui.get_updates());
            window.set_presets(settings::presets());
            refresh(&ui, window);
            themes::paint(&window.global::<Theme>(), &ui.get_theme(), ui.get_light());
            present(window);
        };
        match section {
            Some(_) => ui.on_show_components(open),
            None => ui.on_show_preferences(open),
        }
    }
    let weak = ui.as_weak();
    ui.on_models_changed(move || {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        match made
            .borrow()
            .as_ref()
            .filter(|window| window.window().is_visible())
        {
            Some(window) => refresh(&ui, window),
            None => {
                show(&ui);
                ui.invoke_reload();
            }
        }
    });
}

// slint keeps the size limits of a hidden window, and wayland makes the window again
pub fn reuse<T: ComponentHandle>(
    made: &mut Option<T>,
    make: impl FnOnce() -> Option<T>,
) -> Option<&T> {
    if made
        .as_ref()
        .is_some_and(|window| !window.window().is_visible())
    {
        *made = None;
    }
    if made.is_none() {
        *made = make();
    }
    made.as_ref()
}

pub fn present(window: &(impl ComponentHandle + 'static)) {
    let _ = window.show();
    #[cfg(target_os = "linux")]
    {
        let shown = window.as_weak();
        slint::Timer::single_shot(Duration::from_millis(200), move || {
            shown
                .upgrade()
                .and_then(|window| icon::set(window.window()));
        });
    }
}

// with settings that are not saved the app only asks about them, and does not start again
pub fn restart(ui: &App) {
    if !ui.get_unsaved() {
        let _ = std::process::Command::new(config::exe()).spawn();
    }
    ui.invoke_quit();
}

// the things that the window can do
// the download that the window shows, a cancelled one still running must not change it
static JOB: AtomicU64 = AtomicU64::new(0);

fn attach(ui: &App, window: &Preferences) {
    let job = {
        let (weak, shown) = (ui.as_weak(), window.as_weak());
        move |name: String| {
            let (weak, shown) = (weak.clone(), shown.clone());
            let id = JOB.fetch_add(1, Ordering::Relaxed) + 1;
            services::with(|services| {
                services.install.submit(move |stop| {
                    let told = shown.clone();
                    let report = move |done: f32, progress: String| {
                        let _ = told.upgrade_in_event_loop(move |window| {
                            if JOB.load(Ordering::Relaxed) == id {
                                window.set_fraction(done);
                                window.set_progress(progress.into());
                            }
                        });
                    };
                    let outcome = match name.as_str() {
                        "tensorrt" => install_tensorrt(&report, stop),
                        model => install_model(model, &report, stop),
                    };
                    done(weak.clone(), shown, outcome, id);
                    replace(weak, stop);
                })
            });
        }
    };
    let shown = window.as_weak();
    window.on_install(move |name| {
        if let Some(window) = shown.upgrade() {
            window.set_busy(name.clone());
            window.set_progress("".into());
            window.set_note("".into());
        }
        job(name.into());
    });

    let (weak, shown) = (ui.as_weak(), window.as_weak());
    window.on_remove(move |name| {
        if let (Some(ui), Some(window)) = (weak.upgrade(), shown.upgrade()) {
            finish(&ui, &window, remove(&name));
        }
    });

    // a download that waits behind another one is dropped and never finishes
    let (weak, shown) = (ui.as_weak(), window.as_weak());
    window.on_cancel(move || {
        JOB.fetch_add(1, Ordering::Relaxed);
        services::with(|services| services.install.cancel());
        if let (Some(ui), Some(window)) = (weak.upgrade(), shown.upgrade()) {
            finish(&ui, &window, Ok(()));
        }
    });

    ffmpeg::wire(ui, window);
    settings::attach(ui, window);
}

fn done(ui: Weak<App>, window: Weak<Preferences>, outcome: Outcome, id: u64) {
    let _ = ui.upgrade_in_event_loop(move |ui| {
        if let Some(window) = window
            .upgrade()
            .filter(|_| JOB.load(Ordering::Relaxed) == id)
        {
            finish(&ui, &window, outcome);
        }
    });
}
