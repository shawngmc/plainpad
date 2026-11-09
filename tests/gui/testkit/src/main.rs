//! Test helpers for Plainpad's GUI tests on a headless wlroots compositor (sway).
//!
//! `plainpad-testkit pointer`: a virtual pointer that stays connected (one-shot
//! tools disconnect between commands, which removes the seat's pointer and
//! loses button state). Reads commands from stdin, one per line:
//! `move X Y` (absolute, in output pixels), `down`, `up`.
//!
//! `plainpad-testkit hang-clipboard`: takes the clipboard with a text offer and
//! never sends the data, holding every request open, like a stuck app. Used to
//! check that pasting doesn't freeze the editor.

use std::io::BufRead;
use std::os::fd::OwnedFd;
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_pointer::ButtonState;
use wayland_client::protocol::{wl_registry, wl_seat::WlSeat};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};
use wayland_protocols_wlr::data_control::v1::client::{
    zwlr_data_control_device_v1::{self, ZwlrDataControlDeviceV1 as Device},
    zwlr_data_control_manager_v1::ZwlrDataControlManagerV1 as ControlManager,
    zwlr_data_control_offer_v1::ZwlrDataControlOfferV1 as Offer,
    zwlr_data_control_source_v1::{self, ZwlrDataControlSourceV1 as Source},
};
use wayland_protocols_wlr::virtual_pointer::v1::client::{
    zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1 as PointerManager,
    zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1 as VirtualPointer,
};

const BTN_LEFT: u32 = 0x110;

#[derive(Default)]
struct State {
    held: Vec<OwnedFd>,
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

macro_rules! ignore_events {
    ($($t:ty),*) => {$(
        impl Dispatch<$t, ()> for State {
            fn event(_: &mut Self, _: &$t, _: <$t as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
        }
    )*};
}
ignore_events!(WlSeat, PointerManager, VirtualPointer, ControlManager, Offer);

impl Dispatch<Device, ()> for State {
    fn event(
        _: &mut Self,
        _: &Device,
        _: zwlr_data_control_device_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
    wayland_client::event_created_child!(State, Device, [
        zwlr_data_control_device_v1::EVT_DATA_OFFER_OPCODE => (Offer, ()),
    ]);
}

impl Dispatch<Source, ()> for State {
    fn event(
        state: &mut Self,
        _: &Source,
        event: zwlr_data_control_source_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zwlr_data_control_source_v1::Event::Send { fd, .. } = event {
            eprintln!("hang-clipboard: holding a paste request open");
            state.held.push(fd);
        }
    }
}

fn main() {
    let mode = std::env::args().nth(1).unwrap_or_default();
    let conn = Connection::connect_to_env().expect("no Wayland display");
    let (globals, mut queue) = registry_queue_init::<State>(&conn).expect("registry");
    let qh = queue.handle();
    let seat: WlSeat = globals.bind(&qh, 1..=7, ()).expect("wl_seat");
    let mut state = State::default();
    match mode.as_str() {
        "pointer" => {
            let mgr: PointerManager = globals.bind(&qh, 1..=2, ()).expect("virtual pointer protocol");
            let ptr = mgr.create_virtual_pointer(Some(&seat), &qh, ());
            queue.roundtrip(&mut state).unwrap();
            // Large enough for any test output; positions are scaled into it.
            let (w, h) = (
                std::env::var("TESTKIT_WIDTH").ok().and_then(|v| v.parse().ok()).unwrap_or(1280u32),
                std::env::var("TESTKIT_HEIGHT").ok().and_then(|v| v.parse().ok()).unwrap_or(800u32),
            );
            let mut time = 0u32;
            for line in std::io::stdin().lock().lines() {
                let line = line.unwrap();
                let words: Vec<&str> = line.split_whitespace().collect();
                time += 10;
                match words.as_slice() {
                    ["move", x, y] => ptr.motion_absolute(time, x.parse().unwrap(), y.parse().unwrap(), w, h),
                    ["down"] => ptr.button(time, BTN_LEFT, ButtonState::Pressed),
                    ["up"] => ptr.button(time, BTN_LEFT, ButtonState::Released),
                    _ => continue,
                }
                ptr.frame();
                queue.roundtrip(&mut state).unwrap();
            }
        }
        "hang-clipboard" => {
            let mgr: ControlManager = globals.bind(&qh, 1..=2, ()).expect("data control protocol");
            let device = mgr.get_data_device(&seat, &qh, ());
            let source = mgr.create_data_source(&qh, ());
            source.offer("text/plain;charset=utf-8".into());
            device.set_selection(Some(&source));
            loop {
                queue.blocking_dispatch(&mut state).unwrap();
            }
        }
        _ => {
            eprintln!("usage: plainpad-testkit pointer|hang-clipboard");
            std::process::exit(2);
        }
    }
}
