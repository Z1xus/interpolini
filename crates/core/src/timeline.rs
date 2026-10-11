use crate::config::Ease;

#[derive(Clone, Copy)]
pub struct Placed {
    pub at: f64,
    pub length: f64,
    pub track: usize,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fade {
    pub rise: f64,
    pub fall: f64,
    pub curves: (Ease, Ease),
}

impl Default for Fade {
    fn default() -> Self {
        Self {
            rise: 0.0,
            fall: 0.0,
            curves: (Ease::Linear, Ease::Linear),
        }
    }
}

impl Fade {
    pub fn none(&self) -> bool {
        self.rise == 0.0 && self.fall == 0.0
    }

    pub fn level(&self, inside: f64, length: f64) -> f64 {
        let part = |time: f64, whole: f64| (time / whole).clamp(0.0, 1.0);
        let up = match self.rise > 0.0 {
            true => self.curves.0.shape(part(inside, self.rise)),
            false => 1.0,
        };
        let down = match self.fall > 0.0 {
            true => 1.0 - self.curves.1.shape(1.0 - part(length - inside, self.fall)),
            false => 1.0,
        };
        up.min(down)
    }
}

// a stretch of the timeline that shows the same clips, the lowest one first
#[derive(Clone, Debug, PartialEq)]
pub struct Part {
    pub start: f64,
    pub end: f64,
    pub clips: Vec<usize>,
}

// a higher track is over a lower one, and on one track the later clip is over the earlier one
pub fn parts(clips: &[Placed]) -> Vec<Part> {
    let mut edges = vec![0.0];
    for clip in clips {
        edges.extend([clip.at, clip.at + clip.length]);
    }
    edges.sort_by(f64::total_cmp);
    let mut parts: Vec<Part> = Vec::new();
    for pair in edges.windows(2) {
        let (start, end) = (pair[0], pair[1]);
        // an edge can repeat, and a stretch shorter than a millisecond holds no frame
        if end - start < 0.001 {
            continue;
        }
        let middle = (start + end) / 2.0;
        let covers = |clip: &&Placed| clip.at <= middle && middle < clip.at + clip.length;
        let mut shown: Vec<usize> = (0..clips.len())
            .filter(|index| covers(&&clips[*index]))
            .collect();
        shown.sort_by(|a, b| {
            let (a, b) = (&clips[*a], &clips[*b]);
            a.track.cmp(&b.track).then(a.at.total_cmp(&b.at))
        });
        match parts.last_mut() {
            Some(last) if last.clips == shown => last.end = end,
            _ => parts.push(Part {
                start,
                end,
                clips: shown,
            }),
        }
    }
    parts
}
