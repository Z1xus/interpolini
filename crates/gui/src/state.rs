use std::collections::HashMap;
use std::ops::{Deref, DerefMut};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

use interpolini_core::config::{self, Config, Ease};
use interpolini_core::{Fade, Part, Place, Placed, Sound, parts};
use serde::{Deserialize, Serialize};

use crate::App;
use crate::project::Project;
use crate::view::selected;

// an image is secretly a 1 hour video :)
pub const STILL: f64 = 3600.0;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Saved {
    pub id: u64,
    // clips from one file share a link, and move and cut as one
    pub link: u64,
    // the audio track of the file that this clip plays, a video clip has none
    pub audio: Option<usize>,
    pub streams: usize,
    // the file keeps each config once
    #[serde(skip)]
    pub config: Config,
    pub origin: Option<config::Entry>,
    pub cut: (f32, f32),
    pub seconds: f64,
    pub speed: f32,
    pub fps: f64,
    pub at: f64,
    pub track: usize,
    pub still: bool,
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

impl Default for Saved {
    fn default() -> Self {
        Self {
            id: 0,
            link: 0,
            audio: None,
            streams: 0,
            config: Config::default(),
            origin: None,
            cut: (0.0, 1.0),
            seconds: 0.0,
            speed: 1.0,
            fps: 0.0,
            at: 0.0,
            track: 0,
            still: false,
            size: (0, 0),
            place: Place::default(),
            gain: 0.0,
            fade: (0.0, 0.0),
            cross: 0.0,
            eases: [Ease::Linear; 3],
        }
    }
}

#[derive(Clone, Default)]
pub struct Entry {
    pub saved: Saved,
    pub clip: PathBuf,
    // the times of the keyframes of a video, empty until they are read
    pub keys: Arc<[f64]>,
    // how much its sampled frames differ from the frames before them, empty until it is measured
    pub differences: Arc<[Vec<f32>]>,
    // the cut that the differences were measured for
    pub measured: Option<(f32, f32)>,
    pub missing: bool,
    pub bytes: u64,
}

impl Deref for Entry {
    type Target = Saved;

    fn deref(&self) -> &Saved {
        &self.saved
    }
}

impl DerefMut for Entry {
    fn deref_mut(&mut self) -> &mut Saved {
        &mut self.saved
    }
}

impl Entry {
    pub fn settled(&self) -> Config {
        let mut config = self.config.clone();
        let share = config.dedup.repeats / 100.0;
        config.dedup.threshold = interpolini_core::threshold(&self.differences, share);
        config
    }
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
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Tracks {
    pub video: Vec<f32>,
    pub audio: Vec<(f32, bool)>,
}

impl Default for Tracks {
    fn default() -> Self {
        Self {
            video: vec![64.0],
            audio: vec![(36.0, false)],
        }
    }
}

pub struct State {
    pub entries: Vec<Entry>,
    // the settings that show without a clip, new clips start with them
    pub draft: Entry,
    pub tracks: Tracks,
    pub project: Project,
    pub ids: u64,
    // the clip that the pointer holds to move it, where on the clip it holds, and where the clip was
    pub held: Option<(u64, f64, f64)>,
    // the link of the clips that the pointer moves or trims
    pub touched: Option<u64>,
    pub cutting: bool,
    pub resume: bool,
    pub lasso: Vec<u64>,
    // a drag past the last track makes a new track, of video or of audio, which goes away again without a clip
    pub spare: Option<bool>,
    // the next picture also goes to a file
    pub snap: bool,
    pub span: f64,
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

    fn shape(&self, index: usize, (before, after): (Option<usize>, Option<usize>)) -> Span {
        let entry = &self.entries[index];
        let (before, after) = (
            before.map(|at| &self.entries[at]),
            after.map(|at| &self.entries[at]),
        );
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
            at: entry.at - lead,
            start: f64::from(entry.cut.0) * entry.seconds - lead * f64::from(entry.speed),
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

    pub fn spans(&self) -> Vec<Span> {
        type Times = Vec<(f64, usize)>;
        let mut tracks: HashMap<(bool, usize), (Times, Times)> = HashMap::new();
        for (index, entry) in self.entries.iter().enumerate() {
            let track = tracks
                .entry((entry.audio.is_some(), entry.track))
                .or_default();
            track.0.push((entry.at, index));
            track.1.push((entry.at + entry.kept(), index));
        }
        for (starts, ends) in tracks.values_mut() {
            starts.sort_by(|a, b| a.0.total_cmp(&b.0));
            ends.sort_by(|a, b| a.0.total_cmp(&b.0));
        }
        let near = |list: &[(f64, usize)], time: f64, index: usize| {
            let from = list.partition_point(|other| other.0 <= time - 0.001);
            let inside = list[from..]
                .iter()
                .take_while(|other| other.0 < time + 0.001);
            inside.map(|other| other.1).find(|other| *other != index)
        };
        let neighbours = |index: usize| {
            let entry = &self.entries[index];
            let (starts, ends) = &tracks[&(entry.audio.is_some(), entry.track)];
            let (start, end) = (entry.at, entry.at + entry.kept());
            (near(ends, start, index), near(starts, end, index))
        };
        (0..self.entries.len())
            .map(|index| self.shape(index, neighbours(index)))
            .collect()
    }

    pub fn videos(&self) -> Vec<usize> {
        let video = |index: &usize| self.entries[*index].audio.is_none();
        (0..self.entries.len()).filter(video).collect()
    }

    // the stretches of the timeline and the clips that each one shows, the lowest one first
    pub fn parts(&self) -> Vec<Part> {
        self.parts_in(&self.spans())
    }

    pub fn parts_in(&self, spans: &[Span]) -> Vec<Part> {
        let videos = self.videos();
        let placed = |index: &usize| Placed {
            at: spans[*index].at,
            length: spans[*index].length,
            track: self.entries[*index].track,
        };
        let parts = parts(&videos.iter().map(placed).collect::<Vec<_>>());
        let shown = |part: Part| Part {
            clips: part.clips.iter().map(|clip| videos[*clip]).collect(),
            ..part
        };
        parts.into_iter().map(shown).collect()
    }

    pub fn time(&self, position: f32) -> f64 {
        f64::from(position) * self.span
    }

    pub fn frame(&self, position: f32) -> f64 {
        let (time, seconds) = (self.time(position), self.seconds());
        match (time - seconds).abs() < 0.001 {
            true => seconds - 0.001,
            false => time,
        }
    }

    pub fn part(&self, position: f32) -> Option<(f64, Part)> {
        let time = self.frame(position);
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
    pub fn length(&self) -> f64 {
        self.seconds / f64::from(self.speed)
    }

    pub fn kept(&self) -> f64 {
        self.length() * f64::from(self.cut.1 - self.cut.0)
    }

    // the keyframe that is nearest to a time in the source, between two times
    pub fn key(&self, time: f64, from: f64, to: f64) -> Option<f64> {
        let inside = self.keys.iter().filter(|key| from <= **key && **key <= to);
        inside
            .min_by(|a, b| (*a - time).abs().total_cmp(&(*b - time).abs()))
            .copied()
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
    let audio =
        |(_, entry): &(usize, &Entry)| entry.audio.is_some() && !entry.missing && heard(entry);
    let spans = state.spans();
    let sound = |(index, entry): (usize, &Entry)| {
        let span = &spans[index];
        Sound {
            clip: entry.clip.clone(),
            stream: entry.audio.unwrap_or(0),
            start: span.start,
            length: span.length,
            speed: f64::from(entry.speed),
            at: span.at,
            track: entry.track,
            gain: 10f32.powf(entry.gain / 20.0),
            fade: span.fade,
        }
    };
    let entries = state.entries.iter().enumerate();
    entries.filter(audio).map(sound).collect()
}
