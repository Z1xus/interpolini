use std::path::Path;

use crate::Result;

pub struct Lut {
    size: usize,
    data: Vec<[f32; 3]>,
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
                if size > 256 {
                    return Err(format!("{} is too big for a lut", path.display()).into());
                }
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

    pub fn size(&self) -> usize {
        self.size
    }

    pub fn sample(&self, rgb: [f32; 3]) -> [f32; 3] {
        let size = self.size;
        let scale = (size - 1) as f32;
        let position = rgb.map(|value| value.clamp(0.0, 1.0) * scale);
        let [r, g, b] = position.map(|value| (value as usize).min(size - 2));
        let [tr, tg, tb] = [
            position[0] - r as f32,
            position[1] - g as f32,
            position[2] - b as f32,
        ];
        let base = (b * size + g) * size + r;
        let sg = size;
        let sb = size * size;
        let (w0, wa, wb, w1, ia, ib) = if tr >= tg {
            if tg >= tb {
                (1.0 - tr, tr - tg, tg - tb, tb, 1, sg + 1)
            } else if tr >= tb {
                (1.0 - tr, tr - tb, tb - tg, tg, 1, sb + 1)
            } else {
                (1.0 - tb, tb - tr, tr - tg, tg, sb, sb + 1)
            }
        } else if tb >= tg {
            (1.0 - tb, tb - tg, tg - tr, tr, sb, sb + sg)
        } else if tb >= tr {
            (1.0 - tg, tg - tb, tb - tr, tr, sg, sb + sg)
        } else {
            (1.0 - tg, tg - tr, tr - tb, tb, sg, sg + 1)
        };
        let c0 = self.data[base];
        let ca = self.data[base + ia];
        let cb = self.data[base + ib];
        let c1 = self.data[base + sb + sg + 1];
        std::array::from_fn(|ch| w0 * c0[ch] + wa * ca[ch] + wb * cb[ch] + w1 * c1[ch])
    }
}
