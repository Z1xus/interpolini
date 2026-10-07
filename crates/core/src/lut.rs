use std::path::Path;

use crate::Result;

pub struct Lut {
    size: usize,
    data: Vec<[f32; 3]>,
}

fn mix(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    std::array::from_fn(|channel| a[channel] + (b[channel] - a[channel]) * t)
}

impl Lut {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
        let mut size = 0;
        let mut data = Vec::new();
        for line in text.lines().map(str::trim) {
            if let Some(value) = line.strip_prefix("LUT_3D_SIZE") {
                size = value.trim().parse().unwrap_or(0);
                data.reserve(size * size * size);
            } else if line
                .starts_with(|first: char| first.is_ascii_digit() || first == '-' || first == '.')
            {
                let mut values = line
                    .split_whitespace()
                    .map(|value| value.parse().unwrap_or(0.0));
                data.push(std::array::from_fn(|_| values.next().unwrap_or(0.0)));
            }
        }
        if size < 2 || data.len() != size * size * size {
            return Err(format!("{} is not a 3d cube lut", path.display()).into());
        }
        Ok(Self { size, data })
    }

    pub fn sample(&self, rgb: [f32; 3]) -> [f32; 3] {
        let size = self.size;
        let position = rgb.map(|value| value.clamp(0.0, 1.0) * (size - 1) as f32);
        let [r, g, b] = position.map(|value| (value as usize).min(size - 2));
        let [tr, tg, tb] = [
            position[0] - r as f32,
            position[1] - g as f32,
            position[2] - b as f32,
        ];
        let at = |r: usize, g: usize, b: usize| self.data[(b * size + g) * size + r];
        let row = |g: usize, b: usize| mix(at(r, g, b), at(r + 1, g, b), tr);
        let side = |b: usize| mix(row(g, b), row(g + 1, b), tg);
        mix(side(b), side(b + 1), tb)
    }
}
