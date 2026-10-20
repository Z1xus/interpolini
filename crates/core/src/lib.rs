macro_rules! api {
    ($($field:ident: $symbol:literal fn($($argument:ty),*) $(-> $output:ty)?;)+) => {
        struct Api {
            $($field: unsafe extern "C" fn($($argument),*) $(-> $output)?,)+
            _library: Library,
        }

        impl Api {
            fn load(library: Library) -> std::result::Result<Self, libloading::Error> {
                Ok(Self {
                    $($field: *unsafe { library.get($symbol) }?,)+
                    _library: library,
                })
            }
        }
    };
}

mod about;
mod audio;
mod blend;
pub mod budget;
mod color;
mod compose;
pub mod config;
mod copy;
mod decode;
mod dedup;
mod encode;
mod error;
mod estimate;
mod event;
mod graph;
mod lut;
mod queue;
mod rife;
mod segments;
mod svp;
mod tensorrt;
mod timeline;
mod upscale;
mod weights;
pub mod work;

pub use about::about;
pub use audio::{Mixer, RATE, Sound, peaks};
pub use compose::{Canvas, Picture, Place, Setup, Source, Sources};
pub use copy::keyframes;
pub use decode::{Probe, probe};
pub use dedup::{differences, repeats, threshold};
pub use error::{Error, Result};
pub use estimate::estimate;
pub use event::{Event, Sink};
pub use graph::{Graph, Image, Info, Plan, lossless};
pub use queue::{Job, run, sequence, sounds};
pub use tensorrt::{Support, libraries, plugin, support};
pub use timeline::{Fade, Part, Placed, parts};
