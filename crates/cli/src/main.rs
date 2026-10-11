use std::io::{IsTerminal, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use interpolini_core::{Event, Fade, Job, Place, config, run, sequence, sounds};

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

fn paint(text: &str, code: &str) -> String {
    match std::io::stderr().is_terminal() && std::env::var_os("NO_COLOR").is_none() {
        true => format!("\x1b[{code}m{text}\x1b[0m"),
        false => text.to_owned(),
    }
}

fn line(symbol: char, text: &str) -> String {
    let code = match symbol {
        '!' => "1;93",
        '+' => "1;92",
        '-' => "1;91",
        _ => "1;96",
    };
    let text = text.replace("  |  ", &paint("  |  ", "2"));
    format!("{} {text}", paint(&format!("[{symbol}]"), code))
}

fn print(event: &Event, jobs: &[Job], live: bool) {
    static TURN: AtomicUsize = AtomicUsize::new(0);
    let name = |index: usize| {
        let name = jobs[index].clip.file_name().unwrap_or_default();
        paint(&name.to_string_lossy(), "1")
    };
    let clear = if live { "\r\x1b[2K" } else { "" };
    let text = match event {
        Event::Start { clip } => format!("{}  |  {} of {}", name(*clip), clip + 1, jobs.len()),
        Event::Info(text) => text.clone(),
        Event::Warning(text) => paint(text, "93"),
        Event::Progress { frame, frames, fps } if live => {
            let left = clock((frames - frame) as f32 / fps.max(0.001));
            // the old windows console has no braille
            let old = cfg!(windows) && std::env::var_os("WT_SESSION").is_none();
            let spins: Vec<char> = if old {
                "|/-\\"
            } else {
                "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏"
            }
            .chars()
            .collect();
            let spin = spins[TURN.fetch_add(1, Ordering::Relaxed) % spins.len()];
            // the pasta goes soft from the left as the frames get done
            let cooked = (frame * 24 / frames.max(&1)).min(24) as usize;
            let pasta = paint(&"~".repeat(cooked), "93") + &paint(&"─".repeat(24 - cooked), "2");
            let text = format!("{pasta}  {frame}/{frames}  |  {fps:.0} fps  |  {left} left");
            eprint!("{clear}{}", line(spin, &text));
            let _ = std::io::stderr().flush();
            return;
        }
        Event::Progress { .. } => return,
        Event::Done {
            output, seconds, ..
        } => {
            let name = output.file_name().unwrap_or_default().to_string_lossy();
            format!("{}  |  {}", paint(&name, "1;92"), clock(*seconds))
        }
        Event::Failed { clip, error } => format!("{}: {}", name(*clip), paint(error, "91")),
    };
    let symbol = match event {
        Event::Warning(_) => '!',
        Event::Done { .. } => '+',
        Event::Failed { .. } => '-',
        _ => '*',
    };
    eprintln!("{clear}{}", line(symbol, &text));
}

fn fail(message: impl std::fmt::Display) -> ExitCode {
    eprintln!("{}", line('-', &paint(&message.to_string(), "91")));
    ExitCode::from(2)
}

// the old windows console takes colors only after it is asked to
#[cfg(windows)]
fn colors() {
    use std::ffi::c_void;
    unsafe extern "system" {
        fn GetStdHandle(which: u32) -> *mut c_void;
        fn GetConsoleMode(console: *mut c_void, mode: *mut u32) -> i32;
        fn SetConsoleMode(console: *mut c_void, mode: u32) -> i32;
    }
    let mut mode = 0;
    // -12 is the error stream, and 4 is the mode for escape codes
    unsafe {
        let console = GetStdHandle(-12i32 as u32);
        if GetConsoleMode(console, &mut mode) != 0 {
            SetConsoleMode(console, mode | 4);
        }
    }
}

// a double click on linux gives no terminal, so the program starts again in one
#[cfg(target_os = "linux")]
fn terminal() -> bool {
    let Ok(app) = std::env::current_exe() else {
        return false;
    };
    let named = std::env::var("TERMINAL").map(|terminal| format!("{terminal} -e"));
    let home = std::env::var("HOME").unwrap_or_default();
    let kde = std::fs::read_to_string(format!("{home}/.config/kdeglobals")).unwrap_or_default();
    let kde = kde
        .lines()
        .find_map(|line| line.strip_prefix("TerminalApplication="))
        .map(|terminal| format!("{terminal} -e"));
    // its 2026 and linux still cant tell you the default terminal, so enjoy this fucking list like every other app
    let terminals = [
        "xdg-terminal-exec",
        named.as_deref().unwrap_or_default(),
        kde.as_deref().unwrap_or_default(),
        "konsole -e",
        "gnome-terminal --",
        "ptyxis --",
        "kgx -e",
        "xfce4-terminal -x",
        "ghostty -e",
        "alacritty -e",
        "kitty",
        "wezterm start --",
        "foot",
        "xterm -e",
    ];
    terminals.iter().any(|terminal| {
        let mut words = terminal.split_whitespace();
        let Some(program) = words.next() else {
            return false;
        };
        let mut command = std::process::Command::new(program);
        command.args(words).arg(&app);
        command.args(std::env::args_os().skip(1)).spawn().is_ok()
    })
}

fn main() -> ExitCode {
    #[cfg(windows)]
    colors();
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
    #[cfg(target_os = "linux")]
    if clips.is_empty() && !std::io::stderr().is_terminal() && terminal() {
        return ExitCode::SUCCESS;
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
            let clip = paint(&clip.display().to_string(), "1");
            eprintln!("{}", line('*', &format!("{clip}  |  config {origin}")));
        }
        for warning in loaded.warnings {
            eprintln!("{}", line('!', &paint(&warning, "93")));
        }
        jobs.push(Job {
            clip,
            config: loaded.config,
            cut: (from > 0.0 || to.is_finite()).then_some((from, to)),
            muted: muted.clone(),
            at: 0.0,
            track: 0,
            place: Place::default(),
            fade: Fade::default(),
            hold: 0.0,
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
        eprintln!("{}", line('*', "press enter to close"));
        let _ = std::io::stdin().read_line(&mut String::new());
    }
    ExitCode::from(u8::from(failed.load(Ordering::Relaxed)))
}
