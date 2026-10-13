use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use interpolini_core::config::Target;
use interpolini_core::{Job, Sound, estimate};
use slint::{ComponentHandle, Weak};

use crate::App;
use crate::render::jobs;
use crate::state::{Shared, lock};
use crate::view::megabytes;

struct Request {
    jobs: Vec<Job>,
    sounds: Option<Vec<Sound>>,
    stop: Arc<AtomicBool>,
}

struct Asked {
    jobs: Vec<Job>,
    separate: bool,
    stop: Arc<AtomicBool>,
    done: bool,
}

thread_local! {
    static WAIT: slint::Timer = slint::Timer::default();
    static ASKED: RefCell<Option<Asked>> = const { RefCell::new(None) };
}

fn send(ui: Weak<App>, request: Request) {
    static REQUESTS: OnceLock<Sender<Request>> = OnceLock::new();
    let requests = REQUESTS.get_or_init(|| {
        let (sender, receiver) = channel::<Request>();
        std::thread::spawn(move || {
            for Request { jobs, sounds, stop } in receiver {
                let text = match estimate(&jobs, sounds.as_deref(), &stop) {
                    Ok(bytes) => format!("about {}", megabytes(bytes)),
                    Err(_) => String::new(),
                };
                let _ = ui.upgrade_in_event_loop(move |ui| {
                    ASKED.with_borrow_mut(|asked| {
                        let own = |asked: &&mut Asked| Arc::ptr_eq(&asked.stop, &stop);
                        if let Some(asked) = asked.as_mut().filter(own) {
                            asked.done = true;
                            ui.set_estimate(text.into());
                        }
                    });
                });
            }
        });
        sender
    });
    let _ = requests.send(request);
}

fn measure(ui: &App, shared: &Shared) {
    // the gpu is for the playback and for the render
    if ui.get_playing() {
        return ask(ui, shared);
    }
    if ui.get_rendering() {
        return;
    }
    let mut state = lock(shared);
    // a copy is as large as the part of each file that it keeps
    if ui.get_copying() {
        let video = |entry: &&crate::state::Entry| entry.audio.is_none() && !entry.still;
        let size = |entry: &crate::state::Entry| {
            let file = std::fs::metadata(&entry.clip).map_or(0, |file| file.len());
            file as f64 * f64::from(entry.cut.1 - entry.cut.0)
        };
        let bytes: f64 = state.entries.iter().filter(video).map(size).sum();
        ASKED.set(None);
        return ui.set_estimate(match bytes > 0.0 {
            true => format!("about {}", megabytes(bytes as u64)).into(),
            false => "".into(),
        });
    }
    let separate = ui.get_separate();
    let (mut jobs, sounds, _) = jobs(&state, separate, false);
    if state.current(ui).config.output.target == Target::Size || ui.get_lossless() {
        jobs.clear();
    }
    drop(state);
    let same = |asked: &Asked| asked.jobs == jobs && asked.separate == separate;
    if ASKED.with_borrow(|asked| asked.as_ref().is_some_and(same)) {
        return;
    }
    let stop = Arc::new(AtomicBool::new(false));
    ASKED.set(Some(Asked {
        jobs: jobs.clone(),
        separate,
        stop: Arc::clone(&stop),
        done: jobs.is_empty(),
    }));
    if jobs.is_empty() {
        return ui.set_estimate("".into());
    }
    ui.set_estimate("Measuring…".into());
    let sounds = (!separate).then_some(sounds);
    send(ui.as_weak(), Request { jobs, sounds, stop });
}

pub fn ask(ui: &App, shared: &Shared) {
    ASKED.with_borrow_mut(|asked| {
        if let Some(asked) = asked.take_if(|asked| !asked.done) {
            asked.stop.store(true, Ordering::Relaxed);
        }
    });
    let (weak, shared) = (ui.as_weak(), Arc::clone(shared));
    WAIT.with(|wait| {
        wait.start(
            slint::TimerMode::SingleShot,
            Duration::from_secs(1),
            move || {
                if let Some(ui) = weak.upgrade() {
                    measure(&ui, &shared);
                }
            },
        );
    });
}
