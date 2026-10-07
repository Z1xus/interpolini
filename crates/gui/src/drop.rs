use std::io::Read;
use std::os::fd::AsFd;
use std::path::PathBuf;
use std::sync::Arc;

use slint::winit_030::WinitWindowAccessor;
use slint::winit_030::winit::raw_window_handle::{HasDisplayHandle, RawDisplayHandle};
use wayland_client::backend::Backend;
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_data_device::{self, WlDataDevice};
use wayland_client::protocol::wl_data_device_manager::{DndAction, WlDataDeviceManager};
use wayland_client::protocol::wl_data_offer::{self, WlDataOffer};
use wayland_client::protocol::{wl_registry, wl_seat};
use wayland_client::{
    Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum, delegate_noop, event_created_child,
};

const MIME: &str = "text/uri-list";

// the files, and the point of the window where they dropped
pub type Dropped = Arc<dyn Fn(Vec<PathBuf>, (f64, f64)) + Send + Sync>;

pub enum Hover {
    Files(Vec<PathBuf>),
    At(f64, f64),
    Gone,
}

pub type Hovered = Arc<dyn Fn(Hover) + Send + Sync>;

struct State {
    connection: Connection,
    dropped: Dropped,
    hovered: Hovered,
    offer: Option<WlDataOffer>,
    point: (f64, f64),
    files: bool,
    copies: bool,
}

// winit has no drag and drop on wayland :|
pub struct Drops {
    queue: EventQueue<State>,
    state: State,
    _device: WlDataDevice,
}

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

delegate_noop!(State: ignore wl_seat::WlSeat);
delegate_noop!(State: ignore WlDataDeviceManager);

impl Dispatch<WlDataOffer, ()> for State {
    fn event(
        state: &mut Self,
        _: &WlDataOffer,
        event: wl_data_offer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_data_offer::Event::Offer { mime_type } => state.files |= mime_type == MIME,
            wl_data_offer::Event::Action { dnd_action } => {
                state.copies = dnd_action == WEnum::Value(DndAction::Copy);
            }
            _ => {}
        }
    }
}

fn paths(list: &str) -> Vec<PathBuf> {
    let decode = |text: &str| {
        let mut bytes = Vec::new();
        let mut rest = text.as_bytes();
        while let [first, tail @ ..] = rest {
            let code = tail
                .get(..2)
                .and_then(|code| std::str::from_utf8(code).ok());
            match code
                .filter(|_| *first == b'%')
                .and_then(|code| u8::from_str_radix(code, 16).ok())
            {
                Some(byte) => {
                    bytes.push(byte);
                    rest = &tail[2..];
                }
                None => {
                    bytes.push(*first);
                    rest = tail;
                }
            }
        }
        PathBuf::from(String::from_utf8_lossy(&bytes).into_owned())
    };
    let lines = list
        .lines()
        .filter_map(|line| line.trim().strip_prefix("file://"));
    lines.map(decode).collect()
}

impl Dispatch<WlDataDevice, ()> for State {
    fn event(
        state: &mut Self,
        _: &WlDataDevice,
        event: wl_data_device::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_data_device::Event::DataOffer { .. } => {
                state.files = false;
                state.copies = false;
            }
            wl_data_device::Event::Motion { x, y, .. } => {
                state.point = (x, y);
                (state.hovered)(Hover::At(x, y));
            }
            wl_data_device::Event::Enter {
                serial,
                x,
                y,
                id: Some(offer),
                ..
            } => {
                state.point = (x, y);
                offer.accept(serial, state.files.then(|| MIME.to_owned()));
                if state.files && offer.version() >= 3 {
                    offer.set_actions(DndAction::Copy, DndAction::Copy);
                }
                // the names are read now, so the window can show where the files go
                if let Some((mut reader, writer)) = std::io::pipe().ok().filter(|_| state.files) {
                    offer.receive(MIME.to_owned(), writer.as_fd());
                    let _ = state.connection.flush();
                    let hovered = Arc::clone(&state.hovered);
                    std::thread::spawn(move || {
                        let mut list = String::new();
                        let _ = reader.read_to_string(&mut list);
                        hovered(Hover::Files(paths(&list)));
                        hovered(Hover::At(x, y));
                    });
                }
                state.offer = Some(offer);
            }
            wl_data_device::Event::Leave => {
                (state.hovered)(Hover::Gone);
                if let Some(offer) = state.offer.take() {
                    offer.destroy();
                }
            }
            wl_data_device::Event::Drop => {
                (state.hovered)(Hover::Gone);
                let Some(offer) = state.offer.take() else {
                    return;
                };
                let pipe = std::io::pipe().ok().filter(|_| state.files);
                let Some((mut reader, writer)) = pipe else {
                    offer.destroy();
                    return;
                };
                offer.receive(MIME.to_owned(), writer.as_fd());
                let _ = state.connection.flush();
                drop(writer);
                let (connection, dropped) = (state.connection.clone(), Arc::clone(&state.dropped));
                let finish = state.copies && offer.version() >= 3;
                let point = state.point;
                std::thread::spawn(move || {
                    let mut list = String::new();
                    let _ = reader.read_to_string(&mut list);
                    // finish is valid only after an accepted action
                    if finish {
                        offer.finish();
                    }
                    offer.destroy();
                    let _ = connection.flush();
                    dropped(paths(&list), point);
                });
            }
            // the clipboard comes through the same device
            wl_data_device::Event::Selection { id: Some(offer) } => offer.destroy(),
            _ => {}
        }
    }

    event_created_child!(State, WlDataDevice, [
        wl_data_device::EVT_DATA_OFFER_OPCODE => (WlDataOffer, ()),
    ]);
}

impl Drops {
    pub fn listen(window: &slint::Window, dropped: Dropped, hovered: Hovered) -> Option<Self> {
        let display =
            window.with_winit_window(|window| match window.display_handle().ok()?.as_raw() {
                RawDisplayHandle::Wayland(display) => Some(display.display),
                _ => None,
            })??;
        let backend = unsafe { Backend::from_foreign_display(display.as_ptr().cast()) };
        let connection = Connection::from_backend(backend);
        let (globals, mut queue) = registry_queue_init::<State>(&connection).ok()?;
        let handle = queue.handle();
        let manager: WlDataDeviceManager = globals.bind(&handle, 1..=3, ()).ok()?;
        let seat: wl_seat::WlSeat = globals.bind(&handle, 1..=1, ()).ok()?;
        let device = manager.get_data_device(&seat, &handle, ());
        let mut state = State {
            connection,
            dropped,
            hovered,
            offer: None,
            point: (0.0, 0.0),
            files: false,
            copies: false,
        };
        queue.roundtrip(&mut state).ok()?;
        Some(Self {
            queue,
            state,
            _device: device,
        })
    }

    pub fn pump(&mut self) {
        let _ = self.queue.dispatch_pending(&mut self.state);
        let _ = self.state.connection.flush();
    }
}
