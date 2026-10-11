use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;

use interpolini_core::config::{self, Config, Ease};
use interpolini_core::{Fade, Part, Place, Placed, Sound, parts};

use crate::view::selected;
use crate::{App, preview};

// an image is secretly a 1 hour video :)
pub const STILL: f64 = 3600.0;

#[derive(Clone)]
pub struct Entry {
    pub id: u64,
    // clips from one file share a link, and move and cut as one
    pub link: u64,
    pub clip: PathBuf,
    // the audio track of the file that this clip plays, a video clip has none
    pub audio: Option<usize>,
    pub streams: usize,
    pub config: Config,
    pub origin: Option<config::Entry>,
    pub cut: (f32, f32),
    pub seconds: f64,
    pub fps: f64,
    pub at: f64,
    pub track: usize,
    pub still: bool,
    // the part of its frames that repeat the frame before them, 0 until it is measured
    pub repeats: f32,
    // the cut that the repeats were counted for
    pub measured: Option<(f32, f32)>,
    pub size: (u32, u32),
    pub place: Place,
    // decibels
    pub gain: f32,
    // seconds, in and out
    pub fade: (f32, f32),
    // the seconds of the crossfade with the clip before this one
    pub cross: f32,
    // the curves of the fade in, of the fade out and of the crossfade
    pub eases: [Ease; 3],
}

// where a clip plays, a crossfade makes it start early and end late
pub struct Span {
    pub at: f64,
    pub start: f64,
    pub length: f64,
    pub fade: Fade,
    pub cross: (f64, f64),
}

impl Span {
    // a picture does not fade out under the clip that fades in over it
    pub fn picture(&self) -> Fade {
        Fade {
            fall: if self.cross.1 > 0.0 {
                0.0
            } else {
                self.fade.fall
            },
            ..self.fade
        }
    }
}

// the heights of the tracks, and for an audio track if it is muted
#[derive(Clone)]
pub struct Tracks {
    pub video: Vec<f32>,
    pub audio: Vec<(f32, bool)>,
}

pub struct State {
    pub entries: Vec<Entry>,
    // the settings that show without a clip, new clips start with them
    pub draft: Entry,
    pub tracks: Tracks,
    pub preview: Option<Sender<preview::Request>>,
    pub ids: u64,
    // the clip that the pointer holds to move it, where on the clip it holds, and where the clip was
    pub held: Option<(u64, f64, f64)>,
    // the link of the clips that the pointer moves or trims
    pub touched: Option<u64>,
    // a drag past the last track makes a new track, of video or of audio, which goes away again without a clip
    pub spare: Option<bool>,
    pub cancel: Arc<AtomicBool>,
    pub render: Option<JoinHandle<()>>,
}

impl State {
    // the video clip that the settings are for: the selected one, or the one of a selected sound
    pub fn video(&self, ui: &App) -> Option<usize> {
        let entry = self.entries.get(selected(ui)?)?;
        let own = |other: &&Entry| other.link == entry.link && other.audio.is_none();
        self.entries
            .iter()
            .position(|other| own(&other) && !other.still)
    }

    pub fn current(&mut self, ui: &App) -> &mut Entry {
        match self.video(ui) {
            Some(index) => &mut self.entries[index],
            None => &mut self.draft,
        }
    }

    pub fn seconds(&self) -> f64 {
        let ends = self.entries.iter().map(|entry| entry.at + entry.kept());
        ends.fold(0.0, f64::max)
    }

    pub fn span(&self, index: usize) -> Span {
        let entry = &self.entries[index];
        let (start, end) = (entry.at, entry.at + entry.kept());
        let same = |other: &&Entry| {
            let kind = other.audio.is_some() == entry.audio.is_some();
            other.id != entry.id && kind && other.track == entry.track
        };
        let mut others = self.entries.iter().filter(same);
        let before = others
            .clone()
            .find(|other| (other.at + other.kept() - start).abs() < 0.001);
        let after = others.find(|other| (other.at - end).abs() < 0.001);
        // a crossfade has the cut in its middle, so each of the two clips plays half of it more
        let half = |first: &Entry, second: &Entry| {
            (f64::from(second.cross) / 2.0)
                .min(first.kept())
                .min(second.kept())
        };
        let lead = before.map_or(0.0, |other| half(other, entry));
        let tail = after.map_or(0.0, |other| half(entry, other));
        let next = after.filter(|_| tail > 0.0);
        Span {
            at: start - lead,
            start: f64::from(entry.cut.0) * entry.seconds - lead,
            length: entry.kept() + lead + tail,
            // a crossfade takes the place of the fade at its end of the clip
            fade: Fade {
                rise: if lead > 0.0 {
                    lead * 2.0
                } else {
                    f64::from(entry.fade.0)
                },
                fall: if tail > 0.0 {
                    tail * 2.0
                } else {
                    f64::from(entry.fade.1)
                },
                curves: (
                    entry.eases[if lead > 0.0 { 2 } else { 0 }],
                    next.map_or(entry.eases[1], |next| next.eases[2]),
                ),
            },
            cross: (lead * 2.0, tail * 2.0),
        }
    }

    pub fn videos(&self) -> Vec<usize> {
        let video = |index: &usize| self.entries[*index].audio.is_none();
        (0..self.entries.len()).filter(video).collect()
    }

    // the stretches of the timeline and the clips that each one shows, the lowest one first
    pub fn parts(&self) -> Vec<Part> {
        let videos = self.videos();
        let placed = |index: &usize| Placed {
            at: self.span(*index).at,
            length: self.span(*index).length,
            track: self.entries[*index].track,
        };
        let parts = parts(&videos.iter().map(placed).collect::<Vec<_>>());
        let shown = |part: Part| Part {
            clips: part.clips.iter().map(|clip| videos[*clip]).collect(),
            ..part
        };
        parts.into_iter().map(shown).collect()
    }

    pub fn part(&self, position: f32) -> Option<(f64, Part)> {
        let time = (f64::from(position) * self.seconds()).min(self.seconds() - 0.001);
        let inside = |part: &Part| part.start <= time && time < part.end;
        Some((time, self.parts().into_iter().find(inside)?))
    }

    pub fn locate(&self, position: f32) -> Option<usize> {
        self.part(position)?.1.clips.last().copied()
    }

    // the first video clip gives the picture size, the lowest one when two start together
    pub fn base(&self) -> Option<usize> {
        let videos = self.videos().into_iter();
        let order = |index: &usize| (self.entries[*index].at, self.entries[*index].track);
        videos
            .filter(|index| !self.entries[*index].still)
            .min_by(|a, b| {
                let (a, b) = (order(a), order(b));
                a.0.total_cmp(&b.0).then(a.1.cmp(&b.1))
            })
    }

    // where each track starts, from the top: the highest video track is first
    pub fn places(&self) -> (Vec<f32>, Vec<f32>, f32) {
        let mut top = 0.0;
        let mut video = vec![0.0; self.tracks.video.len()];
        for index in (0..video.len()).rev() {
            video[index] = top;
            top += self.tracks.video[index];
        }
        let mut audio = Vec::new();
        for (height, _) in &self.tracks.audio {
            audio.push(top);
            top += height;
        }
        (video, audio, top)
    }

    pub fn aim(&mut self, place: Option<(f64, f32)>, pictured: bool) -> (usize, Option<usize>) {
        let (_, tops, _) = self.places();
        let below = |depth: f32| tops.first().is_some_and(|top| depth >= *top);
        match place {
            Some((_, depth)) if pictured && !below(depth) => (self.reach(false, depth), None),
            Some((_, depth)) if !pictured && below(depth) => (0, Some(self.reach(true, depth))),
            _ => (0, None),
        }
    }

    // the track of a kind at a depth: over the highest video track or under the lowest audio track
    // it is a new track, which goes away again when nothing lands on it
    pub fn reach(&mut self, sound: bool, depth: f32) -> usize {
        if self.spare != Some(sound) {
            self.retreat();
        }
        let past = match sound {
            true => depth >= self.places().2,
            false => depth < 0.0,
        };
        if past && self.spare.is_none() {
            match sound {
                true => self.tracks.audio.push((36.0, false)),
                false => self.tracks.video.push(64.0),
            }
            self.spare = Some(sound);
        }
        let last = match sound {
            true => self.tracks.audio.len() - 1,
            false => self.tracks.video.len() - 1,
        };
        let track = match past {
            true => last,
            false => self.track(sound, depth),
        };
        if track != last {
            self.retreat();
        }
        track
    }

    pub fn retreat(&mut self) {
        match self.spare.take() {
            Some(true) => drop(self.tracks.audio.pop()),
            Some(false) => drop(self.tracks.video.pop()),
            None => {}
        }
    }

    pub fn track(&self, audio: bool, depth: f32) -> usize {
        let (video, sound, _) = self.places();
        let (tops, count) = if audio {
            (sound, self.tracks.audio.len())
        } else {
            (video, self.tracks.video.len())
        };
        let near = |a: &usize, b: &usize| {
            (tops[*a] - depth)
                .abs()
                .total_cmp(&(tops[*b] - depth).abs())
        };
        let under = (0..count).filter(|index| tops[*index] <= depth);
        under
            .min_by(near)
            .or_else(|| (0..count).min_by(near))
            .unwrap_or(0)
    }
}

impl Entry {
    pub fn kept(&self) -> f64 {
        self.seconds * f64::from(self.cut.1 - self.cut.0)
    }
}

pub type Shared = Arc<Mutex<State>>;

pub fn lock(state: &Shared) -> MutexGuard<'_, State> {
    state.lock().unwrap_or_else(|poison| poison.into_inner())
}

pub fn sounds(state: &State) -> Vec<Sound> {
    let heard = |entry: &Entry| {
        !state
            .tracks
            .audio
            .get(entry.track)
            .is_some_and(|track| track.1)
    };
    let audio = |(_, entry): &(usize, &Entry)| entry.audio.is_some() && heard(entry);
    let sound = |(index, entry): (usize, &Entry)| {
        let span = state.span(index);
        Sound {
            clip: entry.clip.clone(),
            stream: entry.audio.unwrap_or(0),
            start: span.start,
            length: span.length,
            at: span.at,
            track: entry.track,
            gain: 10f32.powf(entry.gain / 20.0),
            fade: span.fade,
        }
    };
    let entries = state.entries.iter().enumerate();
    entries.filter(audio).map(sound).collect()
}
