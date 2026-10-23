use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use interpolini_core::budget::Class;
use interpolini_core::work::{self, Cancel, Pool, Slot};
use slint::Weak;

use crate::App;
use crate::preview::Worker;
use crate::sound::Audio;

// the fields stop in the order that they are here: a render and the preview first, the sound last
pub struct Services {
    #[cfg(target_os = "linux")]
    pub drops: Option<crate::drop::Drops>,
    pub render: Slot,
    pub preview: Slot,
    pub measure: Slot,
    pub install: Slot,
    pub update: Slot,
    pub background: Pool,
    pub tasks: Pool,
    pub worker: Arc<Mutex<Worker>>,
    pub closer: Pool,
    pub audio: Audio,
}

static SERVICES: Mutex<Option<Services>> = Mutex::new(None);
static KEYS: AtomicU64 = AtomicU64::new(0);

fn held() -> MutexGuard<'static, Option<Services>> {
    SERVICES.lock().unwrap_or_else(|poison| poison.into_inner())
}

pub fn start(ui: Weak<App>) {
    *held() = Some(Services {
        #[cfg(target_os = "linux")]
        drops: None,
        render: Slot::new(Class::Render),
        preview: Slot::new(Class::Interactive),
        measure: Slot::new(Class::Interactive),
        install: Slot::new(Class::Background),
        update: Slot::new(Class::Background),
        background: Pool::new(Class::Background),
        tasks: Pool::new(Class::Background),
        worker: Arc::new(Mutex::new(Worker::new(ui))),
        closer: Pool::new(Class::Interactive),
        audio: Audio::new(),
    });
}

pub fn stop() -> usize {
    let services = held().take();
    drop(services);
    work::live()
}

pub fn with<R>(run: impl FnOnce(&mut Services) -> R) -> Option<R> {
    held().as_mut().map(run)
}

pub fn key(name: &str) -> String {
    format!("{name} {}", KEYS.fetch_add(1, Ordering::Relaxed))
}

pub fn background(key: &str, job: impl FnOnce(&Cancel) + Send + 'static) {
    with(|services| services.background.submit(key, job));
}

pub fn task(name: &str, job: impl FnOnce(&Cancel) + Send + 'static) {
    with(|services| services.tasks.submit(&key(name), job));
}

pub fn close<T: Send + 'static>(value: T) {
    with(|services| services.closer.submit(&key("close"), move |_| drop(value)));
}

// a thread waits for the program, a failed one falls back
pub fn child(command: &mut Command, fallback: Option<PathBuf>) {
    let Ok(mut child) = command.spawn() else {
        fallback.into_iter().for_each(open);
        return;
    };
    std::thread::spawn(move || {
        let failed = child.wait().is_ok_and(|status| !status.success());
        if let Some(fallback) = fallback.filter(|_| failed) {
            let _ = slint::invoke_from_event_loop(move || open(fallback));
        }
    });
}

pub fn open(target: impl AsRef<std::ffi::OsStr>) {
    let program = match std::env::consts::OS {
        "windows" => "explorer",
        "macos" => "open",
        _ => "xdg-open",
    };
    child(Command::new(program).arg(target), None);
}
