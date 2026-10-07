use std::fs::File;
use std::io::Write;
use std::os::fd::AsFd;

use slint::winit_030::WinitWindowAccessor;
use slint::winit_030::winit::platform::wayland::WindowExtWayland;
use slint::winit_030::winit::raw_window_handle::{HasDisplayHandle, RawDisplayHandle};
use wayland_client::backend::{Backend, ObjectId};
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::{wl_buffer, wl_registry, wl_shm, wl_shm_pool};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, delegate_noop};
use wayland_protocols::xdg::shell::client::xdg_toplevel::XdgToplevel;
use wayland_protocols::xdg::toplevel_icon::v1::client::xdg_toplevel_icon_manager_v1::XdgToplevelIconManagerV1;
use wayland_protocols::xdg::toplevel_icon::v1::client::xdg_toplevel_icon_v1::XdgToplevelIconV1;

const SVG: &[u8] = include_bytes!("../../../assets/interpolini.svg");
const SIZES: [usize; 4] = [256, 128, 64, 32];

struct State;

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

delegate_noop!(State: ignore wl_shm::WlShm);
delegate_noop!(State: ignore wl_shm_pool::WlShmPool);
delegate_noop!(State: ignore wl_buffer::WlBuffer);
delegate_noop!(State: ignore XdgToplevelIconManagerV1);
delegate_noop!(State: ignore XdgToplevelIconV1);

// the source is square and each size is a whole fraction of it
fn shrink(pixels: &[slint::Rgba8Pixel], from: usize, to: usize) -> Vec<u8> {
    let step = from / to;
    let mut output = Vec::with_capacity(to * to * 4);
    for y in 0..to {
        for x in 0..to {
            let mut sums = [0u32; 4];
            for row in 0..step {
                for pixel in &pixels[(y * step + row) * from + x * step..][..step] {
                    let alpha = u32::from(pixel.a);
                    sums[0] += u32::from(pixel.b) * alpha / 255;
                    sums[1] += u32::from(pixel.g) * alpha / 255;
                    sums[2] += u32::from(pixel.r) * alpha / 255;
                    sums[3] += alpha;
                }
            }
            output.extend(sums.map(|sum| (sum / (step * step) as u32) as u8));
        }
    }
    output
}

// wayland has no window icons, plasma takes one through xdg-toplevel-icon
pub fn set(window: &slint::Window) -> Option<()> {
    let (display, toplevel) = window.with_winit_window(|window| {
        let RawDisplayHandle::Wayland(display) = window.display_handle().ok()?.as_raw() else {
            return None;
        };
        Some((display.display, window.xdg_toplevel()?))
    })??;
    let image = slint::Image::load_from_svg_data(SVG).ok()?.to_rgba8()?;
    let from = image.width() as usize;

    let connection =
        Connection::from_backend(unsafe { Backend::from_foreign_display(display.as_ptr().cast()) });
    let (globals, mut queue) = registry_queue_init::<State>(&connection).ok()?;
    let handle = queue.handle();
    let manager: XdgToplevelIconManagerV1 = globals.bind(&handle, 1..=1, ()).ok()?;
    let shm: wl_shm::WlShm = globals.bind(&handle, 1..=1, ()).ok()?;

    let directory = std::env::var_os("XDG_RUNTIME_DIR")?;
    let path =
        std::path::Path::new(&directory).join(format!("interpolini-icon-{}", std::process::id()));
    let mut file = File::options()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&path)
        .ok()?;
    let _ = std::fs::remove_file(&path);
    let icon = manager.create_icon(&handle, ());
    let total: usize = SIZES.iter().map(|size| size * size * 4).sum();
    for size in SIZES {
        file.write_all(&shrink(image.as_slice(), from, size)).ok()?;
    }
    let pool = shm.create_pool(file.as_fd(), total as i32, &handle, ());
    let mut offset = 0;
    for size in SIZES.map(|size| size as i32) {
        let format = wl_shm::Format::Argb8888;
        icon.add_buffer(
            &pool.create_buffer(offset, size, size, size * 4, format, &handle, ()),
            1,
        );
        offset += size * size * 4;
    }
    let id =
        unsafe { ObjectId::from_ptr(XdgToplevel::interface(), toplevel.as_ptr().cast()) }.ok()?;
    manager.set_icon(&XdgToplevel::from_id(&connection, id).ok()?, Some(&icon));
    queue.roundtrip(&mut State).ok()?;
    Some(())
}
