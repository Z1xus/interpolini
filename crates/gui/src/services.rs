use std::path::PathBuf;
use std::process::{Child, Command};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use interpolini_core::budget::Class;
use interpolini_core::work::{self, Cancel, Pool, Slot};
use slint::Weak;

use crate::App;
use crate::preview::Worker;
use crate::sound::Audio;

// the fields stop in the order that they are here: a render and the preview first, the sound last
pub struct Services {
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
    children: Vec<(Child, Option<PathBuf>)>,
}

static SERVICES: Mutex<Option<Services>> = Mutex::new(None);
static KEYS: AtomicU64 = AtomicU64::new(0);

thread_local! {
    static REAP: slint::Timer = slint::Timer::default();
}

fn held() -> MutexGuard<'static, Option<Services>> {
    SERVICES.lock().unwrap_or_else(|poison| poison.into_inner())
}

pub fn start(ui: Weak<App>) {
    *held() = Some(Services {
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
        children: Vec::new(),
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

// the program is reaped when it ends, a timer looks for that while one runs
pub fn child(command: &mut Command, fallback: Option<PathBuf>) {
    let Ok(child) = command.spawn() else {
        fallback.into_iter().for_each(open);
        return;
    };
    with(|services| services.children.push((child, fallback)));
    REAP.with(|reap| {
        reap.start(slint::TimerMode::Repeated, Duration::from_secs(1), look);
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

fn look() {
    let mut again = Vec::new();
    let waiting = with(|services| {
        services.children.retain_mut(|(child, fallback)| {
            match child.try_wait() {
                Ok(None) => return true,
                Ok(Some(status)) if !status.success() => again.extend(fallback.take()),
                _ => {}
            }
            false
        });
        !services.children.is_empty()
    });
    again.iter().for_each(open);
    if waiting != Some(true) && again.is_empty() {
        REAP.with(slint::Timer::stop);
    }
}
