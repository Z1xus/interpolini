#[derive(Clone, Copy)]
pub struct Placed {
    pub at: f64,
    pub length: f64,
    pub track: usize,
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
        shown.sort_by_key(|index| clips[*index].track);
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
