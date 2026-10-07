use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use interpolini_core::{Event, Job, run};
use slint::{ComponentHandle, Model};

use crate::App;
use crate::state::{Shared, lock, sounds};
use crate::view::{layout, refresh, update};

// row is the clip that renders now, or none when all clips go into one file
fn show(ui: &App, event: Event, row: Option<usize>, current: usize, count: usize) {
    match event {
        Event::Start { clip } if row.is_some() => update(ui, clip, |row| row.state = "0%".into()),
        Event::Start { .. } => {}
        Event::Info(text) | Event::Warning(text) => ui.set_status(text.into()),
        Event::Progress { frame, frames, fps } => {
            let done = frame as f32 / frames as f32;
            let rows = ui.get_clips();
            let pictures = || rows.iter().filter(|row| !row.audio);
            // in one file for all clips, each clip fills while the render is in its time
            let end = pictures().map(|row| row.at + row.kept).fold(0.0, f32::max);
            for index in 0..rows.row_count() {
                let Some(clip) = rows.row_data(index).filter(|clip| !clip.audio) else {
                    continue;
                };
                let part = match row {
                    Some(row) if row == index => done,
                    Some(_) => continue,
                    None => ((done * end - clip.at) / clip.kept.max(0.001)).clamp(0.0, 1.0),
                };
                update(ui, index, |row| {
                    row.progress = part;
                    row.state = match part {
                        part if part <= 0.0 => "".into(),
                        part if part >= 1.0 => "Done".into(),
                        part => format!("{:.0}%", part * 100.0).into(),
                    };
                });
            }
            ui.set_progress(match row {
                Some(_) => (current as f32 + done) / count as f32,
                None => done,
            });
            let left = ((frames - frame) as f32 / fps.max(0.001)) as u32;
            ui.set_status(format!("{fps:.0} fps  |  {}:{:02} left", left / 60, left % 60).into());
        }
        Event::Done { clip, output, .. } => {
            update(ui, clip, |row| {
                row.progress = 0.0;
                row.state = "Done".into();
            });
            let name = output.file_name().unwrap_or_default().to_string_lossy();
            ui.set_status(name.as_ref().into());
        }
        Event::Failed { clip, error } => {
            update(ui, clip, |row| {
                row.progress = 0.0;
                row.state = "Failed".into();
            });
            ui.set_failed(true);
            ui.set_status(error.into());
        }
    }
}

fn render(ui: &App, shared: &Shared) {
    let separate = ui.get_separate();
    let (jobs, sounds, rows, cancel) = {
        let mut state = lock(shared);
        layout(ui, &mut state);
        state.cancel = Arc::new(AtomicBool::new(false));
        let heard = sounds(&state);
        // an image goes into the file of the timeline, and has no file of its own
        let mut rows = state.videos();
        rows.retain(|index| !separate || !state.entries[*index].still);
        let job = |index: &usize| {
            let entry = &state.entries[*index];
            let (start, end) = entry.cut;
            // a clip on its own takes the audio tracks of its file that are still on the timeline
            let kept = |stream: &usize| {
                heard
                    .iter()
                    .any(|sound| sound.clip == entry.clip && sound.stream == *stream)
            };
            Job {
                clip: entry.clip.clone(),
                config: entry.config.clone(),
                cut: (entry.cut != (0.0, 1.0)).then_some((
                    f64::from(start) * entry.seconds,
                    f64::from(end) * entry.seconds,
                )),
                muted: (0..entry.streams).filter(|stream| !kept(stream)).collect(),
                at: entry.at,
                track: entry.track,
                place: entry.place,
            }
        };
        let jobs: Vec<Job> = rows.iter().map(job).collect();
        let sounds = heard.clone();
        (jobs, sounds, rows, Arc::clone(&state.cancel))
    };
    if ui.get_videos() == 0 {
        ui.set_failed(true);
        ui.set_status("The timeline has no video".into());
        return;
    }
    ui.set_rendering(true);
    ui.set_failed(false);
    ui.set_progress(0.0);
    ui.set_playing(false);
    refresh(ui, shared);
    let (ui, state) = (ui.as_weak(), Arc::clone(shared));
    lock(shared).render = Some(std::thread::spawn(move || {
        let current = AtomicUsize::new(0);
        let count = jobs.len();
        let sink = |event: Event| {
            // the events count the video clips, and the rows count all clips
            let event = match event {
                Event::Start { clip } => {
                    current.store(clip, Ordering::Relaxed);
                    Event::Start { clip: rows[clip] }
                }
                Event::Done {
                    clip,
                    output,
                    seconds,
                } => Event::Done {
                    clip: rows[clip],
                    output,
                    seconds,
                },
                Event::Failed { clip, error } => Event::Failed {
                    clip: rows[clip],
                    error,
                },
                other => other,
            };
            let current = current.load(Ordering::Relaxed);
            let row = separate.then_some(rows[current]);
            let _ = ui.upgrade_in_event_loop(move |ui| show(&ui, event, row, current, count));
        };
        run(&jobs, (!separate).then_some(&sounds[..]), &sink, &cancel);
        let _ = ui.upgrade_in_event_loop(move |ui| {
            ui.set_rendering(false);
            refresh(&ui, &state);
        });
    }));
}

pub fn wire(ui: &App, state: &Shared) {
    let (weak, shared) = (ui.as_weak(), Arc::clone(state));
    ui.on_render(move || {
        if let Some(ui) = weak.upgrade() {
            render(&ui, &shared);
        }
    });

    let shared = Arc::clone(state);
    ui.on_cancel(move || lock(&shared).cancel.store(true, Ordering::Relaxed));
}
