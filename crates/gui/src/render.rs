use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use interpolini_core::{Event, Fade, Job, Sound, budget, run};
use slint::{ComponentHandle, Model};

use crate::state::{Shared, State, lock, sounds};
use crate::view::{layout, megabytes, refresh, update};
use crate::{App, services};

// row is the clip that renders now, or none when all clips go into one file
fn show(ui: &App, event: Event, row: Option<usize>, lead: f32, current: usize, count: usize) {
    match event {
        Event::Start { clip } if row.is_some() => update(ui, clip, |row| row.state = "0%".into()),
        Event::Start { .. } => {}
        Event::Info(text) | Event::Warning(text) => {
            ui.set_log(format!("{}{text}\n", ui.get_log()).into());
            ui.set_status(text.into());
        }
        Event::Preparing(done) => {
            ui.set_status(format!("Preparing TensorRT for first use, {:.0}%", done * 100.0).into());
        }
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
                    None => ((lead + done * (end - lead) - clip.at) / clip.kept.max(0.001))
                        .clamp(0.0, 1.0),
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
        Event::Done {
            clip,
            output,
            seconds,
        } => {
            update(ui, clip, |row| {
                row.progress = 0.0;
                row.state = "Done".into();
            });
            let size = megabytes(std::fs::metadata(&output).map_or(0, |file| file.len()));
            let took = seconds as u32;
            ui.set_status(format!("{size} in {}:{:02}", took / 60, took % 60).into());
            ui.set_rendered(output.to_string_lossy().as_ref().into());
        }
        Event::Failed { clip, error } => {
            update(ui, clip, |row| {
                row.progress = 0.0;
                row.state = "Failed".into();
            });
            ui.set_failed(true);
            ui.set_log(format!("{}{error}\n", ui.get_log()).into());
            ui.set_status(error.into());
        }
    }
}

pub fn jobs(state: &State, separate: bool, copy: bool) -> (Vec<Job>, Vec<Sound>, Vec<usize>, f64) {
    let mut heard = sounds(state);
    // an image goes into the file of the timeline, and has no file of its own
    let mut rows = state.videos();
    rows.retain(|index| !separate || !state.entries[*index].still);
    let spans = state.spans();
    let job = |index: &usize| {
        let entry = &state.entries[*index];
        // the fades are for the timeline, a clip in a file of its own has none
        let span = &spans[*index];
        let (start, end) = (span.start, span.start + span.length);
        let faded = !separate && span.length != entry.kept();
        // a clip on its own takes the audio tracks of its file that are still on the timeline
        let kept = |stream: &usize| {
            heard
                .iter()
                .any(|sound| sound.clip == entry.clip && sound.stream == *stream)
        };
        Job {
            clip: entry.clip.clone(),
            config: entry.settled(),
            cut: match faded {
                true => Some((start, end.min(entry.seconds))),
                false => (entry.cut != (0.0, 1.0)).then_some((
                    f64::from(entry.cut.0) * entry.seconds,
                    f64::from(entry.cut.1) * entry.seconds,
                )),
            },
            muted: (0..entry.streams).filter(|stream| !kept(stream)).collect(),
            copy,
            at: if faded { span.at } else { entry.at },
            track: entry.track,
            place: entry.place,
            fade: if separate {
                Fade::default()
            } else {
                span.picture()
            },
            hold: if faded {
                (end - entry.seconds).max(0.0)
            } else {
                0.0
            },
        }
    };
    let mut jobs: Vec<Job> = rows.iter().map(job).collect();
    // the file starts with the first clip, not with the empty start of the timeline
    let places = jobs
        .iter()
        .map(|job| job.at)
        .chain(heard.iter().map(|sound| sound.at));
    let lead = places.min_by(f64::total_cmp).unwrap_or(0.0);
    jobs.iter_mut().for_each(|job| job.at -= lead);
    heard.iter_mut().for_each(|sound| sound.at -= lead);
    (jobs, heard, rows, lead)
}

fn render(ui: &App, shared: &Shared) {
    // a copy makes one file for each clip
    let copy = ui.get_copying();
    let separate = ui.get_separate() || copy;
    let (jobs, sounds, rows, lead) = {
        let mut state = lock(shared);
        layout(ui, &mut state);
        let (jobs, sounds, rows, lead) = jobs(&state, separate, copy);
        (jobs, sounds, rows, lead as f32)
    };
    if ui.get_videos() == 0 {
        ui.set_failed(true);
        ui.set_status("The timeline has no video".into());
        return;
    }
    ui.set_rendering(true);
    ui.set_log("".into());
    ui.set_failed(false);
    ui.set_rendered("".into());
    ui.set_progress(0.0);
    ui.set_playing(false);
    refresh(ui, shared);
    let (ui, state) = (ui.as_weak(), Arc::clone(shared));
    services::with(|services| {
        services.render.submit(move |cancel| {
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
                let _ =
                    ui.upgrade_in_event_loop(move |ui| show(&ui, event, row, lead, current, count));
            };
            budget::render(true);
            run(&jobs, (!separate).then_some(&sounds[..]), &sink, cancel);
            budget::render(false);
            let _ = ui.upgrade_in_event_loop(move |ui| {
                ui.set_rendering(false);
                refresh(&ui, &state);
            });
        })
    });
}

pub fn wire(ui: &App, state: &Shared) {
    let (weak, shared) = (ui.as_weak(), Arc::clone(state));
    ui.on_render(move || {
        if let Some(ui) = weak.upgrade() {
            render(&ui, &shared);
        }
    });

    ui.on_cancel(|| {
        services::with(|services| services.render.cancel());
    });
}
