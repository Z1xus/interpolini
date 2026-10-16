use std::path::PathBuf;

#[derive(Clone, Debug)]
pub enum Event {
    Start {
        clip: usize,
    },
    Info(String),
    Warning(String),
    // tensorrt engine build, 0 to 1
    Preparing(f32),
    Progress {
        frame: u32,
        frames: u32,
        fps: f32,
    },
    Done {
        clip: usize,
        output: PathBuf,
        seconds: f32,
    },
    Failed {
        clip: usize,
        error: String,
    },
}

pub type Sink<'a> = &'a (dyn Fn(Event) + Sync);
