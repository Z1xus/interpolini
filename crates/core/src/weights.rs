use crate::config::{Blending, Weighting};

fn normalize(mut weights: Vec<f64>) -> Vec<f64> {
    let total: f64 = weights.iter().sum();
    for weight in &mut weights {
        *weight /= total;
    }
    weights
}

fn spread(count: usize, start: f64, end: f64) -> impl Iterator<Item = f64> {
    let last = (count - 1).max(1) as f64;
    (0..count).map(move |x| x as f64 * (end - start) / last + start)
}

fn gaussian(count: usize, mean: f64, start: f64, end: f64) -> Vec<f64> {
    spread(count, start, end)
        .map(|x| (-(x - mean).powi(2) / 2.0).exp())
        .collect()
}

fn vegas(ratio: f64) -> Vec<f64> {
    let factor = ratio as usize;
    if factor % 2 == 1 {
        return vec![1.0; factor];
    }
    let mut weights = vec![2.0; factor + 1];
    weights[0] = 1.0;
    weights[factor] = 1.0;
    weights
}

pub fn plan(source_fps: f64, blending: &Blending) -> Vec<f64> {
    let ratio = source_fps / f64::from(blending.fps) * f64::from(blending.intensity);
    if ratio.round() <= 1.0 {
        return vec![1.0];
    }
    // the fused blend takes an odd window only
    let count = ratio.round() as usize | 1;
    let half = (count - 1) as f64 / 2.0;
    normalize(match blending.weighting {
        Weighting::Equal => vec![1.0; count],
        Weighting::Gaussian => gaussian(count, 2.0, 0.0, 2.0),
        Weighting::GaussianSym => gaussian(count, 0.0, -2.0, 2.0),
        Weighting::Pyramid => (0..count)
            .map(|x| half - (x as f64 - half).abs() + 1.0)
            .collect(),
        Weighting::Ascending => (1..=count).map(|x| x as f64).collect(),
        Weighting::Descending => (1..=count).rev().map(|x| x as f64).collect(),
        Weighting::Vegas => vegas(ratio),
        Weighting::Custom => spread(count, 0.0, blending.custom.len() as f64 - 0.1)
            .map(|x| f64::from(blending.custom[x as usize]))
            .collect(),
    })
}
