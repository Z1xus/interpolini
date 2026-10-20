use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use interpolini_core::work;
use interpolini_core::{Mixer, RATE, Sound};

// about a quarter of a second of stereo samples waits for the sound card
const AHEAD: usize = RATE as usize / 2;
const SLICE: usize = 2048;

// the gain, as the bits of a float
static LEVEL: AtomicU32 = AtomicU32::new(0);

// half of the slider sounds half as loud, which is 10 decibels less
pub fn level(volume: f32) {
    let decibels = 10.0 * volume.log2();
    LEVEL.store(10f32.powf(decibels / 20.0).to_bits(), Ordering::Relaxed);
}

enum Command {
    Start(Vec<Sound>, f64),
    Stop,
}

#[derive(Default)]
struct Signal {
    command: Option<Command>,
    hungry: bool,
    closed: bool,
}

type Shared = Arc<(Mutex<Signal>, Condvar)>;
type Queue = Arc<Mutex<VecDeque<f32>>>;

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poison| poison.into_inner())
}

pub struct Audio {
    signal: Shared,
    thread: Option<JoinHandle<()>>,
}

impl Audio {
    pub fn new() -> Self {
        let signal = Shared::default();
        let shared = Arc::clone(&signal);
        Self {
            signal,
            thread: Some(work::spawn("audio", move || run(&shared))),
        }
    }

    pub fn start(&self, sounds: Vec<Sound>, seconds: f64) {
        self.send(Command::Start(sounds, seconds));
    }

    pub fn stop(&self) {
        self.send(Command::Stop);
    }

    fn send(&self, command: Command) {
        locked(&self.signal.0).command = Some(command);
        self.signal.1.notify_one();
    }
}

impl Drop for Audio {
    fn drop(&mut self) {
        locked(&self.signal.0).closed = true;
        self.signal.1.notify_one();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn open(queue: &Queue, signal: &Shared, broken: &Arc<AtomicBool>) -> Option<cpal::Stream> {
    let device = cpal::default_host().default_output_device()?;
    let config = cpal::StreamConfig {
        channels: 2,
        sample_rate: RATE,
        buffer_size: cpal::BufferSize::Default,
    };
    let (waiting, signal, broken) = (Arc::clone(queue), Arc::clone(signal), Arc::clone(broken));
    let mut level = f32::from_bits(LEVEL.load(Ordering::Relaxed));
    let play = move |output: &mut [f32], _: &cpal::OutputCallbackInfo| {
        // a busy queue gives silence, the callback must not wait
        let mut room = false;
        let mut waiting = waiting.try_lock().ok();
        let target = f32::from_bits(LEVEL.load(Ordering::Relaxed));
        for sample in output {
            // the level moves in small steps, a jump is heard as a click
            level += (target - level) * 0.002;
            let next = waiting.as_mut().and_then(|queue| queue.pop_front());
            *sample = next.unwrap_or(0.0) * level;
        }
        if let Some(queue) = &waiting {
            room = queue.len() + 2 * SLICE <= AHEAD;
        }
        if let Some(mut held) = room.then(|| signal.0.try_lock().ok()).flatten() {
            held.hungry = true;
            signal.1.notify_one();
        }
    };
    let failed = move |_| broken.store(true, Ordering::Relaxed);
    let stream = device
        .build_output_stream(config, play, failed, None)
        .ok()?;
    stream.play().ok()?;
    Some(stream)
}

fn run(signal: &Shared) {
    let queue = Queue::default();
    let broken = Arc::new(AtomicBool::new(false));
    let mut stream: Option<cpal::Stream> = None;
    let mut playing: Option<(Mixer, i64)> = None;
    loop {
        let command = {
            let mut held = locked(&signal.0);
            while held.command.is_none() && !held.closed && !(playing.is_some() && held.hungry) {
                held = signal.1.wait(held).unwrap_or_else(|p| p.into_inner());
            }
            if held.closed {
                return;
            }
            held.hungry = false;
            held.command.take()
        };
        match command {
            Some(Command::Stop) => {
                playing = None;
                locked(&queue).clear();
                if let Some(stream) = &stream {
                    let _ = stream.pause();
                }
            }
            Some(Command::Start(sounds, seconds)) => {
                let mixer = Mixer::new(&sounds, true);
                playing = None;
                locked(&queue).clear();
                if broken.swap(false, Ordering::Relaxed) {
                    stream = None;
                }
                if mixer.lanes() > 0 {
                    stream = stream.or_else(|| open(&queue, signal, &broken));
                    let started = stream.as_ref().is_some_and(|stream| stream.play().is_ok());
                    if started {
                        playing = Some((mixer, (seconds * f64::from(RATE)) as i64));
                        locked(&signal.0).hungry = true;
                    }
                }
            }
            None => {}
        }
        let Some((mixer, position)) = &mut playing else {
            continue;
        };
        while locked(&queue).len() + 2 * SLICE <= AHEAD {
            if mixer.lanes() == 0 {
                playing = None;
                break;
            }
            let Ok([left, right]) = mixer.take(0, *position, SLICE) else {
                playing = None;
                break;
            };
            *position += SLICE as i64;
            let both = left.iter().zip(&right).flat_map(|(l, r)| [*l, *r]);
            locked(&queue).extend(both);
            let held = locked(&signal.0);
            if held.command.is_some() || held.closed {
                break;
            }
        }
    }
}
