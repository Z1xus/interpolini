use std::io::{IsTerminal, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use interpolini_core::{Event, Job, Place, config, run, sequence, sounds};

const USAGE: &str = "usage: interpolini-cli [options] <clip>...

  -c, --config <name|file>  use interpolini.<name>.ini, or any ini file
  -s, --set <setting>       change one setting, like -s blending.fps=120
      --from <seconds>      start of the part to render
      --to <seconds>        end of the part to render
      --mute <track>        leave out an audio track, the first one is 1
  -1, --join                put all clips into one file, in their order
  -g, --global              ignore the configs in the folder of the clip
  -j, --json                print events as json lines

settings are the same as in interpolini.ini, written as section.key=value:
  interpolini-cli -s interpolation.fps=480 -s output.codec=hevc clip.mp4";

fn quote(text: &str) -> String {
    let mut quoted = String::from('"');
    for character in text.chars() {
        match character {
            '"' => quoted.push_str("\\\""),
            '\\' => quoted.push_str("\\\\"),
            control if control.is_control() => {
                quoted.push_str(&format!("\\u{:04x}", control as u32))
            }
            other => quoted.push(other),
        }
    }
    quoted + "\""
}

fn json(event: &Event, jobs: &[Job]) -> String {
    let clip = |index: usize| quote(&jobs[index].clip.to_string_lossy());
    match event {
        Event::Start { clip: index } => format!(r#"{{"event":"start","clip":{}}}"#, clip(*index)),
        Event::Info(text) => format!(r#"{{"event":"info","message":{}}}"#, quote(text)),
        Event::Warning(text) => format!(r#"{{"event":"warning","message":{}}}"#, quote(text)),
        Event::Progress { frame, frames, fps } => {
            format!(r#"{{"event":"progress","frame":{frame},"frames":{frames},"fps":{fps:.1}}}"#)
        }
        Event::Done {
            clip: index,
            output,
            seconds,
        } => format!(
            r#"{{"event":"done","clip":{},"output":{},"seconds":{seconds:.1}}}"#,
            clip(*index),
            quote(&output.to_string_lossy())
        ),
        Event::Failed { clip: index, error } => {
            format!(
                r#"{{"event":"failed","clip":{},"error":{}}}"#,
                clip(*index),
                quote(error)
            )
        }
    }
}

fn clock(seconds: f32) -> String {
    let seconds = seconds as u32;
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

fn print(event: &Event, jobs: &[Job], live: bool) {
    let name = |index: usize| {
        jobs[index]
            .clip
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
    };
    let clear = if live { "\r\x1b[2K" } else { "" };
    match event {
        Event::Start { clip } => {
            eprintln!("[*] {}  |  {} of {}", name(*clip), clip + 1, jobs.len())
        }
        Event::Info(text) => eprintln!("[*] {text}"),
        Event::Warning(text) => eprintln!("{clear}[!] {text}"),
        Event::Progress { frame, frames, fps } if live => {
            let left = clock((frames - frame) as f32 / fps.max(0.001));
            eprint!("{clear}[*] {frame}/{frames}  |  {fps:.0} fps  |  {left} left");
            let _ = std::io::stderr().flush();
        }
        Event::Progress { .. } => {}
        Event::Done {
            output, seconds, ..
        } => {
            let name = output.file_name().unwrap_or_default().to_string_lossy();
            eprintln!("{clear}[+] {name}  |  {}", clock(*seconds));
        }
        Event::Failed { clip, error } => eprintln!("{clear}[-] {}: {error}", name(*clip)),
    }
}

fn fail(message: impl std::fmt::Display) -> ExitCode {
    eprintln!("[-] {message}");
    ExitCode::from(2)
}

fn main() -> ExitCode {
    let (mut global, mut lines, mut named) = (false, false, String::from("default"));
    let mut join = false;
    let (mut settings, mut clips, mut muted) = (String::new(), Vec::new(), Vec::new());
    let (mut from, mut to) = (0.0, f64::INFINITY);
    let mut arguments = std::env::args().skip(1);
    while let Some(argument) = arguments.next() {
        let mut value = || arguments.next().unwrap_or_default();
        match argument.as_str() {
            "-g" | "--global" => global = true,
            "-j" | "--json" => lines = true,
            "-1" | "--join" => join = true,
            "-c" | "--config" => named = value(),
            "-s" | "--set" => {
                let value = value();
                let Some(((section, key), value)) = value
                    .split_once('=')
                    .and_then(|(name, value)| Some((name.split_once('.')?, value)))
                else {
                    return fail(format!("\"{value}\" is not section.key=value"));
                };
                settings.push_str(&format!("[{section}]\n{key}: {value}\n"));
            }
            "--from" | "--to" | "--mute" => {
                let Ok(number) = value().parse::<f64>() else {
                    return fail(format!("{argument} needs a number"));
                };
                match argument.as_str() {
                    "--from" => from = number,
                    "--to" => to = number,
                    _ => muted.push((number as usize).saturating_sub(1)),
                }
            }
            "-h" | "--help" => {
                println!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            "-V" | "--version" => {
                println!("interpolini {}", env!("CARGO_PKG_VERSION"));
                return ExitCode::SUCCESS;
            }
            _ => clips.push(PathBuf::from(argument)),
        }
    }
    let picked = clips.is_empty();
    if picked {
        let videos = ["mp4", "mkv", "mov", "webm", "avi", "m4v", "ts", "flv"];
        let dialog = rfd::FileDialog::new().add_filter("Video", &videos);
        clips = dialog.pick_files().unwrap_or_default();
    }
    if clips.is_empty() {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    }

    config::create();
    let file = PathBuf::from(&named);
    let mut jobs = Vec::new();
    for clip in clips {
        let mut loaded = match file.is_file() {
            true => config::read(config::Entry {
                name: named.clone(),
                path: file.clone(),
                local: false,
            }),
            false => config::load(&clip, global, &named),
        };
        if loaded.entry.is_none() && named != "default" {
            return fail(format!("there is no config \"{named}\""));
        }
        loaded.warnings.extend(loaded.config.apply(&settings));
        if !lines {
            let origin = loaded
                .entry
                .map_or("built-in".into(), |entry| entry.path.display().to_string());
            eprintln!("[*] {}  |  config {origin}", clip.display());
        }
        for warning in loaded.warnings {
            eprintln!("[!] {warning}");
        }
        jobs.push(Job {
            clip,
            config: loaded.config,
            cut: (from > 0.0 || to.is_finite()).then_some((from, to)),
            muted: muted.clone(),
            at: 0.0,
            track: 0,
            place: Place::default(),
        });
    }

    if let Err(error) = sequence(&mut jobs) {
        return fail(error);
    }
    let cancel = Arc::new(AtomicBool::new(false));
    let handler = Arc::clone(&cancel);
    // the first ctrl-c closes the file after the current frame, the second one quits
    let _ = ctrlc::set_handler(move || {
        if handler.swap(true, Ordering::Relaxed) {
            std::process::exit(130);
        }
    });

    let live = std::io::stderr().is_terminal();
    let failed = AtomicBool::new(false);
    let sink = |event: Event| {
        if matches!(event, Event::Failed { .. }) {
            failed.store(true, Ordering::Relaxed);
        }
        if lines {
            println!("{}", json(&event, &jobs));
        } else {
            print(&event, &jobs, live);
        }
    };
    let timeline = match join.then(|| sounds(&jobs)) {
        Some(Err(error)) => return fail(error),
        Some(Ok(sounds)) => Some(sounds),
        None => None,
    };
    run(&jobs, timeline.as_deref(), &sink, &cancel);
    // a double click opens a console that closes with the program
    if picked && std::io::stdin().is_terminal() {
        eprintln!("[*] press enter to close");
        let _ = std::io::stdin().read_line(&mut String::new());
    }
    ExitCode::from(u8::from(failed.load(Ordering::Relaxed)))
}
