use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
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

pub struct Player {
    _stream: cpal::Stream,
    stop: Arc<AtomicBool>,
}

impl Drop for Player {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

impl Player {
    pub fn start(sounds: &[Sound], seconds: f64) -> Option<Self> {
        let device = cpal::default_host().default_output_device()?;
        let config = cpal::StreamConfig {
            channels: 2,
            sample_rate: RATE,
            buffer_size: cpal::BufferSize::Default,
        };
        let queue = Arc::new(Mutex::new(VecDeque::<f32>::new()));
        let waiting = Arc::clone(&queue);
        let mut level = f32::from_bits(LEVEL.load(Ordering::Relaxed));
        let play = move |output: &mut [f32], _: &cpal::OutputCallbackInfo| {
            let mut waiting = waiting.lock().unwrap_or_else(|poison| poison.into_inner());
            let target = f32::from_bits(LEVEL.load(Ordering::Relaxed));
            for sample in output {
                // the level moves in small steps, a jump is heard as a click
                level += (target - level) * 0.002;
                *sample = waiting.pop_front().unwrap_or(0.0) * level;
            }
        };
        let stream = device
            .build_output_stream(config, play, |_| {}, None)
            .ok()?;
        stream.play().ok()?;

        let stop = Arc::new(AtomicBool::new(false));
        let (stopped, mut mixer) = (Arc::clone(&stop), Mixer::new(sounds, true));
        let mut position = (seconds * f64::from(RATE)) as i64;
        std::thread::spawn(move || {
            while !stopped.load(Ordering::Relaxed) && mixer.lanes() > 0 {
                let full = queue
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner())
                    .len()
                    >= AHEAD;
                if full {
                    std::thread::sleep(Duration::from_millis(5));
                    continue;
                }
                let Ok([left, right]) = mixer.take(0, position, SLICE) else {
                    return;
                };
                position += SLICE as i64;
                let mut queue = queue.lock().unwrap_or_else(|poison| poison.into_inner());
                queue.extend(
                    left.iter()
                        .zip(&right)
                        .flat_map(|(left, right)| [*left, *right]),
                );
            }
        });
        Some(Self {
            _stream: stream,
            stop,
        })
    }
}
