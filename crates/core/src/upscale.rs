use ffmpeg_next::format::Pixel;
use ffmpeg_next::{ffi, frame};

use crate::Result;
use crate::compose::Scaler;
use crate::config::{Method, Upscale};
use crate::decode::Meta;

pub struct Upscaler {
    scaler: Scaler,
    pub meta: Meta,
}

impl Upscaler {
    pub fn new(meta: &Meta, config: &Upscale) -> Option<Self> {
        let side = config.resolution.side();
        let short = meta.width.min(meta.height);
        if !config.enabled || short >= side {
            return None;
        }
        let scale = |length: u32| (u64::from(length) * u64::from(side) / u64::from(short)) as u32;
        let flags = match config.method {
            Method::Nearest => ffi::SwsFlags::SWS_POINT,
            Method::Bilinear => ffi::SwsFlags::SWS_BILINEAR,
            Method::Bicubic => ffi::SwsFlags::SWS_BICUBIC,
            Method::Lanczos => ffi::SwsFlags::SWS_LANCZOS,
        };
        Some(Self {
            scaler: Scaler::with(flags),
            meta: Meta {
                width: scale(meta.width) & !1,
                height: scale(meta.height) & !1,
                ..*meta
            },
        })
    }

    pub fn run(&self, picture: &frame::Video) -> Result<frame::Video> {
        let mut scaled = frame::Video::new(Pixel::YUV420P, self.meta.width, self.meta.height);
        self.meta.tag(&mut scaled);
        self.scaler.run(&mut scaled, picture)?;
        Ok(scaled)
    }
}
