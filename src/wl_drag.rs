//! Dragging tabs between windows on Wayland.
//!
//! Wayland never tells a client where its windows are on screen, so the
//! position-based approach used elsewhere can't find the window a tab was
//! dropped on. Instead this does what browsers do: start a real Wayland
//! drag-and-drop with a private MIME type. The compositor then tells us which
//! of our surfaces the pointer is over and where the drop happened.
//!
//! winit doesn't expose drag-and-drop, so this binds its own seat, pointer
//! (to learn the button-press serial a drag must start from) and data device
//! on winit's existing libwayland connection, using a separate event queue
//! that is drained from the event loop's `about_to_wait`.

use std::ffi::c_void;
use wayland_backend::client::{Backend, ObjectId};
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::wl_data_device::{self, WlDataDevice};
use wayland_client::protocol::wl_data_device_manager::{DndAction, WlDataDeviceManager};
use wayland_client::protocol::wl_data_offer::{self, WlDataOffer};
use wayland_client::protocol::wl_data_source::{self, WlDataSource};
use wayland_client::protocol::wl_pointer::{self, WlPointer};
use wayland_client::protocol::wl_registry::WlRegistry;
use wayland_client::protocol::wl_seat::{self, WlSeat};
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{event_created_child, Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum};

const MIME: &str = "application/x-plainpad-tab";

/// How a drag ended.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Outcome {
    /// Dropped on one of our windows, identified by its `wl_surface` pointer, at
    /// this position in surface-local logical pixels.
    Dropped { surface: usize, x: f64, y: f64 },
    /// Dropped somewhere that isn't one of our windows, or cancelled.
    Elsewhere,
}

#[derive(Default)]
struct State {
    pointer: Option<WlPointer>,
    /// Serial of the last button press; a drag must start from it.
    press_serial: Option<u32>,
    source: Option<WlDataSource>,
    offer: Option<WlDataOffer>,
    offer_action: bool,
    /// Our surface the drag is currently over, and the pointer position on it.
    over: Option<usize>,
    pos: (f64, f64),
    outcome: Option<Outcome>,
}

pub struct WlDrag {
    conn: Connection,
    queue: EventQueue<State>,
    state: State,
    manager: WlDataDeviceManager,
    device: WlDataDevice,
}

impl WlDrag {
    /// Attaches to winit's Wayland connection. `display` is the `wl_display`
    /// pointer from the window's display handle.
    pub fn new(display: *mut c_void) -> Option<Self> {
        // SAFETY: the display comes from winit and outlives the event loop, and we
        // only use our own event queue on it.
        let conn = Connection::from_backend(unsafe { Backend::from_foreign_display(display.cast()) });
        let (globals, mut queue) = registry_queue_init::<State>(&conn).ok()?;
        let qh = queue.handle();
        let seat: WlSeat = globals.bind(&qh, 1..=7, ()).ok()?;
        let manager: WlDataDeviceManager = globals.bind(&qh, 3..=3, ()).ok()?;
        let device = manager.get_data_device(&seat, &qh, ());
        let mut state = State::default();
        queue.roundtrip(&mut state).ok()?;
        Some(Self { conn, queue, state, manager, device })
    }

    /// Starts dragging from `surface` (a `wl_surface` pointer from a window
    /// handle). Must be called while the mouse button is still held down.
    pub fn start(&mut self, surface: *mut c_void) -> bool {
        self.poll_events();
        let Some(serial) = self.state.press_serial else { return false };
        // SAFETY: the pointer is a live wl_surface owned by one of our windows.
        let Ok(id) = (unsafe { ObjectId::from_ptr(WlSurface::interface(), surface.cast()) }) else { return false };
        let Ok(origin) = WlSurface::from_id(&self.conn, id) else { return false };
        let qh = self.queue.handle();
        let source = self.manager.create_data_source(&qh, ());
        source.offer(MIME.into());
        source.set_actions(DndAction::Move);
        self.device.start_drag(Some(&source), &origin, None, serial);
        self.state.source = Some(source);
        self.state.outcome = None;
        self.state.over = None;
        let _ = self.conn.flush();
        true
    }

    fn poll_events(&mut self) {
        let _ = self.queue.dispatch_pending(&mut self.state);
        let _ = self.conn.flush();
    }

    /// Processes pending events; returns how the drag ended, once it has.
    pub fn poll(&mut self) -> Option<Outcome> {
        self.poll_events();
        self.state.outcome.take()
    }
}

fn surface_key(s: &WlSurface) -> usize {
    s.id().as_ptr() as usize
}

impl State {
    fn end(&mut self, outcome: Outcome) {
        if let Some(src) = self.source.take() {
            src.destroy();
            self.outcome.get_or_insert(outcome);
        }
        if let Some(offer) = self.offer.take() {
            offer.destroy();
        }
        self.over = None;
    }
}

impl Dispatch<WlRegistry, GlobalListContents> for State {
    fn event(_: &mut Self, _: &WlRegistry, _: <WlRegistry as Proxy>::Event, _: &GlobalListContents, _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<WlDataDeviceManager, ()> for State {
    fn event(_: &mut Self, _: &WlDataDeviceManager, _: <WlDataDeviceManager as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<WlSeat, ()> for State {
    fn event(state: &mut Self, seat: &WlSeat, event: wl_seat::Event, _: &(), _: &Connection, qh: &QueueHandle<Self>) {
        if let wl_seat::Event::Capabilities { capabilities: WEnum::Value(caps) } = event {
            let has = caps.contains(wl_seat::Capability::Pointer);
            match (has, &state.pointer) {
                (true, None) => state.pointer = Some(seat.get_pointer(qh, ())),
                (false, Some(_)) => {
                    if let Some(p) = state.pointer.take()
                        && p.version() >= 3
                    {
                        p.release();
                    }
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<WlPointer, ()> for State {
    fn event(state: &mut Self, _: &WlPointer, event: wl_pointer::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let wl_pointer::Event::Button { serial, state: WEnum::Value(wl_pointer::ButtonState::Pressed), .. } = event {
            state.press_serial = Some(serial);
        }
    }
}

impl Dispatch<WlDataDevice, ()> for State {
    fn event(state: &mut Self, _: &WlDataDevice, event: wl_data_device::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match event {
            wl_data_device::Event::Motion { x, y, .. } => state.pos = (x, y),
            wl_data_device::Event::Enter { serial, surface, id, x, y } => {
                state.pos = (x, y);
                if let Some(old) = state.offer.take() {
                    old.destroy();
                }
                // Only accept our own drags; other apps' drags pass through untouched.
                if let (Some(offer), true) = (id, state.source.is_some()) {
                    offer.accept(serial, Some(MIME.into()));
                    offer.set_actions(DndAction::Move, DndAction::Move);
                    state.offer = Some(offer);
                    state.offer_action = false;
                    state.over = Some(surface_key(&surface));
                } else {
                    state.over = None;
                }
            }
            wl_data_device::Event::Leave => {
                if let Some(o) = state.offer.take() {
                    o.destroy();
                }
                state.over = None;
            }
            wl_data_device::Event::Drop => {
                if let Some(target) = state.over {
                    if let Some(o) = &state.offer
                        && state.offer_action
                    {
                        o.finish();
                    }
                    let (x, y) = state.pos;
                    state.end(Outcome::Dropped { surface: target, x, y });
                }
            }
            _ => {}
        }
    }

    event_created_child!(State, WlDataDevice, [
        wl_data_device::EVT_DATA_OFFER_OPCODE => (WlDataOffer, ()),
    ]);
}

impl Dispatch<WlDataOffer, ()> for State {
    fn event(state: &mut Self, _: &WlDataOffer, event: wl_data_offer::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let wl_data_offer::Event::Action { dnd_action: WEnum::Value(a) } = event {
            state.offer_action = a.contains(DndAction::Move);
        }
    }
}

impl Dispatch<WlDataSource, ()> for State {
    fn event(state: &mut Self, _: &WlDataSource, event: wl_data_source::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match event {
            // Nothing outside this process accepts our MIME type, so no data is ever transferred.
            wl_data_source::Event::Send { fd, .. } => drop(fd),
            wl_data_source::Event::Cancelled => state.end(Outcome::Elsewhere),
            wl_data_source::Event::DndFinished => {
                let (target, (x, y)) = (state.over, state.pos);
                state.end(target.map_or(Outcome::Elsewhere, |surface| Outcome::Dropped { surface, x, y }));
            }
            _ => {}
        }
    }
}
