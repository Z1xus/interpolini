use std::cell::RefCell;
use std::env::consts::{DLL_PREFIX, DLL_SUFFIX};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use interpolini_core::config::{self, Backend};
use sevenz_rust2::{ArchiveReader, Password};
use sha2::{Digest, Sha256};
use slint::{ComponentHandle, ModelRc, VecModel, Weak};

#[cfg(target_os = "linux")]
use crate::icon;
use crate::{App, Components, Download, Theme, ffmpeg, themes};

const MODELS_URL: &str =
    "https://github.com/AmusementClub/vs-mlrt/releases/download/external-models/";
// the folder in rife/, the archive, the file in it, and the sha256 of the archive
const MODELS: [(&str, &str, &str, &str); 5] = [
    (
        "rife-v4.6",
        "rife_v2_v4.7z",
        "rife_v2/rife_v4.6.onnx",
        "c9999d6af6800365f4f3d716dbaa51f5954e05f38424f83b705b055d754c71cc",
    ),
    (
        "rife-v4.15-lite",
        "rife_v4.15_lite.7z",
        "rife_v2/rife_v4.15_lite.onnx",
        "9c894bccb7dbd5bd0ff9d99616360285628230de81374181b97a69124f73d1a9",
    ),
    (
        "rife-v4.22-lite",
        "rife_v4.22_lite.7z",
        "rife_v2/rife_v4.22_lite.onnx",
        "e7c3872fd217663911f0b89a5940bdc314f5a061fd5fa8639fd1674d31d2f1cf",
    ),
    (
        "rife-v4.25",
        "rife_v4.25.7z",
        "rife_v2/rife_v4.25.onnx",
        "172fe975c1775134bb87108e4ec6d1a89e861cc5d3be2ac23bf08afe5ed626b8",
    ),
    (
        "rife-v4.26",
        "rife_v4.26.7z",
        "rife_v2/rife_v4.26.onnx",
        "dfdabd84a2a3db773f87604b8cc255e94a6a72f13550d910ccd3b4ee2606cd4f",
    ),
];
const VULKAN_URL: &str = "https://raw.githubusercontent.com/TNTwise/rife-ncnn-vulkan/13338e38debe2e400b3eeecf6792312d01a692f9/models/";
// the folder in rife/, and the sha256 of its flownet.bin and flownet.param
const VULKAN: [(&str, &str, &str); 6] = [
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
            "tensorrt_libs/libnvonnxparser.so.10",
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
            "tensorrt_libs/nvonnxparser_10.dll",
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
type Report<'a> = &'a dyn Fn(f32, String);

pub fn text(error: impl ToString) -> String {
    error.to_string()
}

fn download(url: &str, sha: &str, to: &Path, report: Report, stop: &AtomicBool) -> Outcome {
    let part = to.with_extension("part");
    let mut response = ureq::get(url).call().map_err(text)?;
    let length = response.headers().get("content-length");
    let total = length
        .and_then(|length| length.to_str().ok()?.parse::<u64>().ok())
        .unwrap_or(0);
    let mut reader = response.body_mut().as_reader();
    let mut file = File::create(&part).map_err(text)?;
    let mut hash = Sha256::new();
    let mut buffer = vec![0; 1 << 20];
    let started = Instant::now();
    let mut told = started;
    let mut done = 0;
    loop {
        let count = reader.read(&mut buffer).map_err(text)?;
        if count == 0 || stop.load(Ordering::Relaxed) {
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
    let hash: String = hash
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    if stop.load(Ordering::Relaxed) || hash != sha {
        let _ = fs::remove_file(&part);
        return Err(match stop.load(Ordering::Relaxed) {
            true => String::new(),
            false => "The download is damaged. Try again.".into(),
        });
    }
    fs::rename(part, to).map_err(text)
}

fn install_model(name: &str, report: Report, stop: &AtomicBool) -> Outcome {
    let (_, archive, file, sha) = MODELS.into_iter().find(|model| model.0 == name).ok_or("")?;
    let folder = config::app_dir().join("rife").join(name);
    fs::create_dir_all(&folder).map_err(text)?;
    let packed = folder.join("model.7z");
    download(
        &format!("{MODELS_URL}{archive}"),
        sha,
        &packed,
        report,
        stop,
    )?;
    report(1.0, "Unpacking…".into());
    let mut reader = ArchiveReader::open(&packed, Password::empty()).map_err(text)?;
    let model = reader.read_file(file).map_err(text)?;
    fs::write(folder.join(Backend::Tensorrt.model()), model).map_err(text)?;
    fs::remove_file(packed).map_err(text)
}

fn install_vulkan(name: &str, report: Report, stop: &AtomicBool) -> Outcome {
    let (_, weights, graph) = VULKAN.into_iter().find(|model| model.0 == name).ok_or("")?;
    let folder = config::app_dir().join("rife").join(name);
    fs::create_dir_all(&folder).map_err(text)?;
    // the file that marks the model as installed comes last
    for (file, sha) in [("flownet.bin", weights), (Backend::Vulkan.model(), graph)] {
        let url = format!("{VULKAN_URL}{name}/{file}");
        download(&url, sha, &folder.join(file), report, stop)?;
    }
    Ok(())
}

fn install_tensorrt(report: Report, stop: &AtomicBool) -> Outcome {
    let folder = config::app_dir().join("tensorrt");
    fs::create_dir_all(&folder).map_err(text)?;
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
    // tensorrt is of no use without a model
    match config::models(Backend::Tensorrt).is_empty() {
        true => install_model(MODELS[0].0, report, stop),
        false => Ok(()),
    }
}

// a name is tensorrt, or a backend and a model, like vulkan/rife-v4.6
fn remove(name: &str) -> Outcome {
    let app = config::app_dir();
    let Some((backend, model)) = name.split_once('/') else {
        return fs::remove_dir_all(app.join("tensorrt")).map_err(text);
    };
    let folder = app.join("rife").join(model);
    for file in fs::read_dir(&folder).map_err(text)?.flatten() {
        let path = file.path();
        let gone = match backend {
            "vulkan" => path.file_stem().is_some_and(|name| name == "flownet"),
            // the engines that tensorrt made for the model go with it
            _ => {
                let engine = path.extension().is_some_and(|kind| kind == "engine");
                engine || path.ends_with(Backend::Tensorrt.model())
            }
        };
        if gone {
            fs::remove_file(path).map_err(text)?;
        }
    }
    // the folder stays when it also holds the model for the other backend
    let _ = fs::remove_dir(folder);
    Ok(())
}

// the plugin for tensorrt comes with the app on the systems that have tensorrt
fn plugin() -> bool {
    let name = format!("{DLL_PREFIX}interpolini_rife_trt{DLL_SUFFIX}");
    config::app_dir().join(name).exists()
}

fn installed() -> bool {
    let library = WHEELS.first().and_then(|wheel| wheel.2.first());
    let library = library.and_then(|file| Path::new(file).file_name());
    plugin() && library.is_some_and(|name| config::app_dir().join("tensorrt").join(name).exists())
}

fn refresh(ui: &App, window: &Components) {
    let list = |backend: Backend, names: Vec<&str>| {
        let have = config::models(backend);
        let models = names.into_iter().map(|name| Download {
            name: name.into(),
            installed: have.iter().any(|model| model == name),
        });
        ModelRc::new(VecModel::from_iter(models))
    };
    let vulkan = VULKAN.iter().map(|model| model.0).collect();
    window.set_vulkan(list(Backend::Vulkan, vulkan));
    let tensorrt = MODELS.iter().map(|model| model.0).collect();
    window.set_models(list(Backend::Tensorrt, tensorrt));
    window.set_tensorrt(installed());
    window.set_tensorrt_shipped(plugin());
    ffmpeg::show(window);
    ui.set_tensorrt(installed());
    // the model list of the settings follows what is installed
    ui.invoke_select(ui.get_selected());
}

pub fn finish(ui: &App, window: &Components, outcome: Outcome) {
    window.set_busy("".into());
    window.set_note(outcome.err().unwrap_or_default().into());
    refresh(ui, window);
}

pub fn wire(ui: &App) {
    ffmpeg::clean();
    ui.set_components(true);
    ui.set_tensorrt(installed());
    // the window is made when it first shows: a window that waits hidden is in the task bar of the desktop
    let made: Rc<RefCell<Option<Components>>> = Rc::default();
    let weak = ui.as_weak();
    ui.on_show_components(move || {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        let mut made = made.borrow_mut();
        if made.is_none() {
            *made = Components::new().ok().inspect(|window| attach(&ui, window));
        }
        let Some(window) = made.as_ref() else {
            return;
        };
        refresh(&ui, window);
        themes::paint(&window.global::<Theme>(), &ui.get_theme(), ui.get_light());
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
    });
}

// the things that the window can do
fn attach(ui: &App, window: &Components) {
    let stop = Arc::new(AtomicBool::new(false));
    let job = {
        let (weak, shown, stop) = (ui.as_weak(), window.as_weak(), Arc::clone(&stop));
        move |name: String| {
            let (weak, shown, stop) = (weak.clone(), shown.clone(), Arc::clone(&stop));
            stop.store(false, Ordering::Relaxed);
            std::thread::spawn(move || {
                let told = shown.clone();
                let report = move |done: f32, progress: String| {
                    let _ = told.upgrade_in_event_loop(move |window| {
                        window.set_fraction(done);
                        window.set_progress(progress.into());
                    });
                };
                let outcome = match name.split_once('/') {
                    Some(("vulkan", model)) => install_vulkan(model, &report, &stop),
                    Some((_, model)) => install_model(model, &report, &stop),
                    None => install_tensorrt(&report, &stop),
                };
                done(weak, shown, outcome);
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

    window.on_cancel(move || stop.store(true, Ordering::Relaxed));

    ffmpeg::wire(ui, window);
}

fn done(ui: Weak<App>, window: Weak<Components>, outcome: Outcome) {
    let _ = ui.upgrade_in_event_loop(move |ui| {
        if let Some(window) = window.upgrade() {
            finish(&ui, &window, outcome);
        }
    });
}
