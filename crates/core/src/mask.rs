use std::path::{Path, PathBuf};
use std::sync::Mutex;

use ffmpeg_next::format::Pixel;
use ffmpeg_next::{ffi, frame};

use crate::Result;
use crate::compose::Scaler;
use crate::config;
use crate::decode::Decoder;

#[derive(Clone)]
pub struct Gray {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

pub fn path(name: &str) -> PathBuf {
    let folder = config::app_dir().join("masks");
    let plain = folder.join(format!("{name}.png"));
    let other = || {
        let found = std::fs::read_dir(&folder).ok()?.flatten();
        found.map(|entry| entry.path()).find(|path| {
            let png = path
                .extension()
                .is_some_and(|kind| kind.eq_ignore_ascii_case("png"));
            let stem = path.file_stem().and_then(|stem| stem.to_str());
            png && stem.map(str::trim) == Some(name)
        })
    };
    match plain.exists() {
        true => plain,
        false => other().unwrap_or(plain),
    }
}

fn full(picture: &mut frame::Video) {
    unsafe { (*picture.as_mut_ptr()).color_range = ffi::AVColorRange::AVCOL_RANGE_JPEG };
}

static HELD: Mutex<Option<(String, Gray)>> = Mutex::new(None);

pub fn hold(draft: Option<(String, Gray)>) {
    *HELD.lock().unwrap_or_else(|poison| poison.into_inner()) = draft;
}

pub fn load(name: &str) -> Result<Gray> {
    if let Some((held, gray)) = &*HELD.lock().unwrap_or_else(|poison| poison.into_inner())
        && held == name
    {
        return Ok(gray.clone());
    }
    read(&path(name))
}

pub fn read(file: &Path) -> Result<Gray> {
    let mut source = Decoder::open(file)?.first()?;
    full(&mut source);
    let (width, height) = (source.width(), source.height());
    let mut rgba = frame::Video::new(Pixel::RGBA, width, height);
    full(&mut rgba);
    Scaler::new().run(&mut rgba, &source)?;
    let stride = rgba.stride(0);
    let rows = rgba.data(0).chunks(stride).take(height as usize);
    let pixels = rows
        .flat_map(|row| row[..width as usize * 4].chunks(4))
        .map(|pixel| {
            let [r, g, b, a] = [0, 1, 2, 3].map(|at| u32::from(pixel[at]));
            let luma = (77 * r + 150 * g + 29 * b + 128) >> 8;
            ((luma * a + 255 * (255 - a) + 127) / 255) as u8
        })
        .collect();
    Ok(Gray {
        width,
        height,
        pixels,
    })
}

impl Gray {
    pub fn fit(&self, width: u32, height: u32, soft: f32) -> Result<Vec<u8>> {
        let mut from = frame::Video::new(Pixel::GRAY8, self.width, self.height);
        let mut to = frame::Video::new(Pixel::GRAY8, width, height);
        full(&mut from);
        full(&mut to);
        let stride = from.stride(0);
        for (row, line) in self.pixels.chunks(self.width as usize).enumerate() {
            from.data_mut(0)[row * stride..][..line.len()].copy_from_slice(line);
        }
        Scaler::new().run(&mut to, &from)?;
        let (stride, width, height) = (to.stride(0), width as usize, height as usize);
        let rows = to.data(0).chunks(stride).take(height);
        let sharp: Vec<u8> = rows.flat_map(|row| &row[..width]).copied().collect();
        // soft is in pixels at 1080p
        let radius = (soft * height as f32 / 1080.0).round() as usize;
        if radius == 0 {
            return Ok(sharp);
        }
        let mut soft = sharp.clone();
        for _ in 0..2 {
            blur(&mut soft, 1, width, height, width, radius);
            blur(&mut soft, width, 1, width, height, radius);
        }
        Ok(soft.iter().zip(&sharp).map(|(a, b)| *a.min(b)).collect())
    }
}

fn blur(pixels: &mut [u8], step: usize, next: usize, lines: usize, length: usize, radius: usize) {
    let mut line = vec![0u32; length];
    for start in (0..lines).map(|index| index * next) {
        for (at, value) in line.iter_mut().enumerate() {
            *value = u32::from(pixels[start + at * step]);
        }
        let take = |at: isize| line[at.clamp(0, length as isize - 1) as usize];
        let (radius, size) = (radius as isize, 2 * radius as u32 + 1);
        let mut sum: u32 = (-radius..=radius).map(take).sum();
        for at in 0..length as isize {
            pixels[start + at as usize * step] = ((sum + size / 2) / size) as u8;
            sum += take(at + radius + 1);
            sum -= take(at - radius);
        }
    }
}
