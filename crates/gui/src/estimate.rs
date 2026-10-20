use std::cell::RefCell;
use std::sync::Arc;

use std::time::Duration;

use interpolini_core::config::Target;
use interpolini_core::work::Cancel;
use interpolini_core::{Job, Setup};
use slint::ComponentHandle;

use crate::render::jobs;
use crate::state::{Shared, lock};
use crate::view::megabytes;
use crate::{App, preview, services};

struct Asked {
    jobs: Vec<Job>,
    separate: bool,
    setup: Setup,
    stop: Cancel,
    done: bool,
}

thread_local! {
    static WAIT: slint::Timer = slint::Timer::default();
    static ASKED: RefCell<Option<Asked>> = const { RefCell::new(None) };
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
        let size =
            |entry: &crate::state::Entry| entry.bytes as f64 * f64::from(entry.cut.1 - entry.cut.0);
        let bytes: f64 = state.entries.iter().filter(video).map(size).sum();
        ASKED.set(None);
        return ui.set_estimate(match bytes > 0.0 {
            true => format!("about {}", megabytes(bytes as u64)).into(),
            false => "".into(),
        });
    }
    let separate = ui.get_separate();
    let (mut jobs, sounds, ..) = jobs(&state, separate, false, crate::render::folder(ui));
    if state.current(ui).config.output.target == Target::Size || ui.get_lossless() {
        jobs.clear();
    }
    let setup = state.project.setup();
    drop(state);
    let same =
        |asked: &Asked| asked.jobs == jobs && asked.separate == separate && asked.setup == setup;
    if ASKED.with_borrow(|asked| asked.as_ref().is_some_and(same)) {
        return;
    }
    let stop = Cancel::default();
    ASKED.set(Some(Asked {
        jobs: jobs.clone(),
        separate,
        setup,
        stop: stop.clone(),
        done: jobs.is_empty(),
    }));
    if jobs.is_empty() {
        return ui.set_estimate("".into());
    }
    ui.set_estimate("Measuring…".into());
    let (weak, own) = (ui.as_weak(), stop.clone());
    let done = move |bytes: Option<u64>| {
        let text = bytes.map_or(String::new(), |bytes| format!("about {}", megabytes(bytes)));
        let _ = weak.upgrade_in_event_loop(move |ui| {
            ASKED.with_borrow_mut(|asked| {
                let own = |asked: &&mut Asked| asked.stop.same(&own);
                if let Some(asked) = asked.as_mut().filter(own) {
                    asked.done = true;
                    ui.set_estimate(text.into());
                }
            });
        });
    };
    preview::measure(preview::Measure {
        jobs,
        sounds: (!separate).then_some(sounds),
        setup,
        done: Box::new(done),
    });
}

pub fn ask(ui: &App, shared: &Shared) {
    ASKED.with_borrow_mut(|asked| {
        if let Some(asked) = asked.take_if(|asked| !asked.done) {
            asked.stop.stop();
            services::with(|services| services.measure.cancel());
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
