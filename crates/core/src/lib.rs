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
mod color;
mod compose;
pub mod config;
mod copy;
mod decode;
mod dedup;
mod encode;
mod error;
mod event;
mod graph;
mod lut;
mod queue;
mod rife;
mod svp;
mod timeline;
mod weights;

pub use about::about;
pub use audio::{Mixer, RATE, Sound, peaks};
pub use compose::{Canvas, Picture, Place, Source};
pub use decode::{Probe, probe};
pub use dedup::repeats;
pub use error::{Error, Result};
pub use event::{Event, Sink};
pub use graph::{Graph, Image, Info, Plan, lossless};
pub use queue::{Job, run, sequence, sounds};
pub use timeline::{Part, Placed, parts};
