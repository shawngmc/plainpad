//! Native Wayland support that winit doesn't provide: dragging tabs between
//! windows, and the clipboard.
//!
//! Both use the core `wl_data_device` protocol, which every compositor
//! implements (unlike `wlr-data-control`, which GNOME lacks). This binds its
//! own seat, pointer, keyboard and data device on winit's existing libwayland
//! connection, with its own event queue dispatched on a dedicated thread. That
//! thread answers clipboard requests from other apps immediately, whatever the
//! main thread is doing, and wakes the event loop when a drag ends.
//!
//! - Tab drags: Wayland never tells a client where its windows are on screen,
//!   so this starts a real drag-and-drop with a private MIME type, as browsers
//!   do. The compositor reports which of our surfaces it was dropped on.
//! - Clipboard: copying offers the text and claims the selection using the
//!   serial of the key press or click that triggered it (the compositor
//!   requires one). Pasting reads the current selection offer through a pipe.

mod present;
pub use present::Framebuffer;

use std::ffi::c_void;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use wayland_backend::client::{Backend, ObjectId};
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_callback::WlCallback;
use wayland_client::protocol::wl_data_device::{self, WlDataDevice};
use wayland_client::protocol::wl_data_device_manager::{DndAction, WlDataDeviceManager};
use wayland_client::protocol::wl_data_offer::{self, WlDataOffer};
use wayland_client::protocol::wl_data_source::{self, WlDataSource};
use wayland_client::protocol::wl_keyboard::{self, WlKeyboard};
use wayland_client::protocol::wl_pointer::{self, WlPointer};
use wayland_client::protocol::wl_registry::WlRegistry;
use wayland_client::protocol::wl_seat::{self, WlSeat};
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, WEnum, event_created_child};

const TAB_MIME: &str = "application/x-plainpad-tab";
/// Text formats offered when copying, best first. The X11-style names are what
/// apps running under Xwayland ask for.
const TEXT_MIMES: [&str; 5] = ["text/plain;charset=utf-8", "UTF8_STRING", "text/plain", "TEXT", "STRING"];
/// The most we'll read from another app's clipboard. Without a cap, an app
/// that never stops writing would fill memory.
const PASTE_MAX: u64 = 256 * 1024 * 1024;
/// At most this many threads writing our clipboard text to other apps at once.
/// An app that keeps requesting and never reads would otherwise pile up threads.
const MAX_WRITERS: usize = 8;
static WRITERS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
/// Ignore offers that advertise absurd numbers of MIME types.
const MAX_MIMES: usize = 64;

/// How a paste request started.
pub enum PasteStart {
    /// Our own copied text, available immediately.
    Ready(String),
    /// Being read from another app; the result will be delivered later.
    Pending,
    /// Nothing pasteable on the clipboard.
    Empty,
}

/// How a drag ended.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Outcome {
    /// Dropped on one of our windows, identified by its `wl_surface` pointer, at
    /// this position in surface-local logical pixels.
    Dropped { surface: usize, x: f64, y: f64 },
    /// Dropped somewhere that isn't one of our windows, or cancelled.
    Elsewhere,
}

/// What a data source we created is for.
enum SourceKind {
    Drag,
    /// The text we put on the clipboard.
    Clipboard(Arc<String>),
}

/// MIME types an offer advertises, collected from its `offer` events.
#[derive(Default)]
struct OfferMimes(Mutex<Vec<String>>);

/// Everything both threads look at, behind one mutex.
#[derive(Default)]
struct Shared {
    pointer: Option<WlPointer>,
    keyboard: Option<WlKeyboard>,
    /// Serial of the last button press; a drag must start from it.
    press_serial: Option<u32>,
    /// Serial of the latest key press or click; claiming the clipboard needs one.
    input_serial: Option<u32>,
    // Drag state.
    drag_source: Option<WlDataSource>,
    drag_offer: Option<WlDataOffer>,
    offer_action: bool,
    /// Our surface the drag is currently over, and the pointer position on it.
    over: Option<usize>,
    pos: (f64, f64),
    outcome: Option<Outcome>,
    // Clipboard state.
    /// Our clipboard source while we own the clipboard.
    clip_source: Option<WlDataSource>,
    /// The current clipboard contents offered by whoever owns it (maybe us).
    selection: Option<WlDataOffer>,
}

/// The dispatch thread's state.
struct State {
    shared: Arc<Mutex<Shared>>,
    /// Wakes the main event loop (a drag ended, or a frame buffer was released).
    wake: Box<dyn Fn() + Send>,
}

impl State {
    fn lock(&self) -> MutexGuard<'_, Shared> {
        self.shared.lock().unwrap_or_else(|e| e.into_inner())
    }
}

pub struct Wayland {
    conn: Connection,
    qh: QueueHandle<State>,
    shared: Arc<Mutex<Shared>>,
    manager: WlDataDeviceManager,
    device: WlDataDevice,
    shm: Option<wayland_client::protocol::wl_shm::WlShm>,
    trimmer: Arc<present::Trimmer>,
    trim_thread: Option<std::thread::JoinHandle<()>>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Wayland {
    /// Attaches to winit's Wayland connection and starts the dispatch thread.
    /// `display` is the `wl_display` pointer from a window's display handle;
    /// `wake` is called (from that thread) when the main loop should call [`Wayland::poll`].
    pub fn new(display: *mut c_void, wake: impl Fn() + Send + 'static) -> Option<Self> {
        // SAFETY: the display comes from winit and stays open until the event loop
        // ends. App drops this in `exiting`, before that, and Drop stops the thread.
        let conn = Connection::from_backend(unsafe { Backend::from_foreign_display(display.cast()) });
        let (globals, mut queue) = registry_queue_init::<State>(&conn).ok()?;
        let qh = queue.handle();
        let seat: WlSeat = globals.bind(&qh, 1..=7, ()).ok()?;
        let manager: WlDataDeviceManager = globals.bind(&qh, 3..=3, ()).ok()?;
        let device = manager.get_data_device(&seat, &qh, ());
        let shm = globals.bind(&qh, 1..=1, ()).ok();
        let shared = Arc::new(Mutex::new(Shared::default()));
        let mut state = State { shared: Arc::clone(&shared), wake: Box::new(wake) };
        queue.roundtrip(&mut state).ok()?;
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let thread = std::thread::Builder::new()
            .name("wayland".into())
            .spawn(move || {
                while !thread_stop.load(Ordering::Acquire) {
                    if queue.blocking_dispatch(&mut state).is_err() {
                        break;
                    }
                }
            })
            .ok()?;
        let (trimmer, trim_thread) = present::Trimmer::start();
        Some(Self { conn, qh, shared, manager, device, shm, trimmer, trim_thread, stop, thread: Some(thread) })
    }

    /// A low-memory frame presenter for a window's `wl_surface` (a pointer from
    /// its window handle).
    pub fn framebuffer(&self, surface: *mut c_void) -> Option<Framebuffer> {
        let shm = self.shm.clone()?;
        // SAFETY: the pointer is a live wl_surface owned by one of our windows.
        let id = unsafe { ObjectId::from_ptr(WlSurface::interface(), surface.cast()) }.ok()?;
        let surface = WlSurface::from_id(&self.conn, id).ok()?;
        Some(Framebuffer::new(self.conn.clone(), self.qh.clone(), shm, surface, Arc::clone(&self.trimmer)))
    }

    fn lock(&self) -> MutexGuard<'_, Shared> {
        self.shared.lock().unwrap_or_else(|e| e.into_inner())
    }

    // ------------------------------------------------------------ tab drag

    /// Starts dragging from `surface` (a `wl_surface` pointer from a window
    /// handle). Must be called while the mouse button is still held down.
    pub fn start(&mut self, surface: *mut c_void) -> bool {
        // Hold the lock while creating the source, so the dispatch thread can't see
        // its events before it's recorded.
        let mut sh = self.lock();
        let Some(serial) = sh.press_serial else { return false };
        // SAFETY: the pointer is a live wl_surface owned by one of our windows.
        let Ok(id) = (unsafe { ObjectId::from_ptr(WlSurface::interface(), surface.cast()) }) else { return false };
        let Ok(origin) = WlSurface::from_id(&self.conn, id) else { return false };
        let source = self.manager.create_data_source(&self.qh, SourceKind::Drag);
        source.offer(TAB_MIME.into());
        source.set_actions(DndAction::Move);
        self.device.start_drag(Some(&source), &origin, None, serial);
        sh.drag_source = Some(source);
        sh.outcome = None;
        sh.over = None;
        drop(sh);
        let _ = self.conn.flush();
        true
    }

    /// Returns how a drag ended, once it has.
    pub fn poll(&mut self) -> Option<Outcome> {
        self.lock().outcome.take()
    }

    // ------------------------------------------------------------ clipboard

    /// Puts `text` on the clipboard. Returns false if there is no recent input
    /// event to authorise it (the compositor would ignore the request).
    pub fn set_clipboard(&mut self, text: String) -> bool {
        let mut sh = self.lock();
        let Some(serial) = sh.input_serial else { return false };
        let source = self.manager.create_data_source(&self.qh, SourceKind::Clipboard(Arc::new(text)));
        for m in TEXT_MIMES {
            source.offer(m.into());
        }
        self.device.set_selection(Some(&source), serial);
        if let Some(old) = sh.clip_source.replace(source) {
            old.destroy();
        }
        drop(sh);
        let _ = self.conn.flush();
        true
    }

    /// Starts reading the clipboard as text. Text we copied ourselves comes back
    /// at once; otherwise another app sends it through a pipe, which is read on a
    /// background thread that passes the result to `deliver`. Nothing here waits
    /// on the other app, so a slow or stuck one can't freeze the editor.
    pub fn request_paste(&mut self, deliver: impl FnOnce(Result<String, String>) + Send + 'static) -> PasteStart {
        let sh = self.lock();
        if let Some(src) = &sh.clip_source
            && let Some(SourceKind::Clipboard(text)) = src.data::<SourceKind>()
        {
            return PasteStart::Ready(text.to_string());
        }
        // Keep holding the lock while using the offer: the dispatch thread destroys
        // replaced offers under this lock, and libwayland doesn't allow destroying
        // an object on one thread while another sends a request on it.
        let Some(offer) = sh.selection.as_ref() else { return PasteStart::Empty };
        let mimes = offer.data::<OfferMimes>().and_then(|m| m.0.lock().ok().map(|v| v.clone())).unwrap_or_default();
        let Some(mime) = TEXT_MIMES.iter().find(|m| mimes.iter().any(|x| x == *m)) else { return PasteStart::Empty };
        let Ok((reader, writer)) = std::io::pipe() else { return PasteStart::Empty };
        offer.receive(mime.to_string(), std::os::fd::AsFd::as_fd(&writer));
        let _ = self.conn.flush();
        drop(sh);
        drop(writer); // our copy of the write end; the other app holds its own
        // Stop at PASTE_MAX so an app that never stops sending can't exhaust memory.
        let spawned = std::thread::Builder::new().name("clipboard-read".into()).spawn(move || {
            let mut buf = Vec::new();
            let result = match reader.take(PASTE_MAX + 1).read_to_end(&mut buf) {
                Ok(_) if buf.len() as u64 > PASTE_MAX => {
                    Err("The clipboard contents are too large to paste (over 256 MB)".to_string())
                }
                Ok(_) => Ok(String::from_utf8_lossy(&buf).into_owned()),
                Err(e) => Err(format!("Couldn't read the clipboard: {e}")),
            };
            deliver(result);
        });
        if spawned.is_ok() { PasteStart::Pending } else { PasteStart::Empty }
    }
}

impl Drop for Wayland {
    /// Stops the dispatch thread before the display can go away: set the flag,
    /// then make a request whose reply wakes the thread's blocking wait.
    fn drop(&mut self) {
        self.trimmer.stop();
        if let Some(t) = self.trim_thread.take() {
            let _ = t.join();
        }
        self.stop.store(true, Ordering::Release);
        self.conn.display().sync(&self.qh, ());
        let _ = self.conn.flush();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn surface_key(s: &WlSurface) -> usize {
    s.id().as_ptr() as usize
}

impl Shared {
    fn end_drag(&mut self, outcome: Outcome) {
        if let Some(src) = self.drag_source.take() {
            src.destroy();
            self.outcome.get_or_insert(outcome);
        }
        if let Some(offer) = self.drag_offer.take() {
            offer.destroy();
        }
        self.over = None;
    }
}

impl Dispatch<WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut Self,
        _: &WlRegistry,
        _: <WlRegistry as Proxy>::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlCallback, ()> for State {
    fn event(
        _: &mut Self,
        _: &WlCallback,
        _: <WlCallback as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlDataDeviceManager, ()> for State {
    fn event(
        _: &mut Self,
        _: &WlDataDeviceManager,
        _: <WlDataDeviceManager as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlSeat, ()> for State {
    fn event(state: &mut Self, seat: &WlSeat, event: wl_seat::Event, _: &(), _: &Connection, qh: &QueueHandle<Self>) {
        let mut sh = state.lock();
        if let wl_seat::Event::Capabilities { capabilities: WEnum::Value(caps) } = event {
            let pointer = caps.contains(wl_seat::Capability::Pointer);
            match (pointer, &sh.pointer) {
                (true, None) => sh.pointer = Some(seat.get_pointer(qh, ())),
                (false, Some(_)) => {
                    if let Some(p) = sh.pointer.take()
                        && p.version() >= 3
                    {
                        p.release();
                    }
                }
                _ => {}
            }
            let keyboard = caps.contains(wl_seat::Capability::Keyboard);
            match (keyboard, &sh.keyboard) {
                (true, None) => sh.keyboard = Some(seat.get_keyboard(qh, ())),
                (false, Some(_)) => {
                    if let Some(k) = sh.keyboard.take()
                        && k.version() >= 3
                    {
                        k.release();
                    }
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<WlPointer, ()> for State {
    fn event(state: &mut Self, _: &WlPointer, event: wl_pointer::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        let mut sh = state.lock();
        if let wl_pointer::Event::Button { serial, state: WEnum::Value(wl_pointer::ButtonState::Pressed), .. } = event {
            sh.press_serial = Some(serial);
            sh.input_serial = Some(serial);
        }
    }
}

impl Dispatch<WlKeyboard, ()> for State {
    fn event(
        state: &mut Self,
        _: &WlKeyboard,
        event: wl_keyboard::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let mut sh = state.lock();
        // Only serials are needed; winit handles the actual key input. The keymap
        // event's file descriptor is closed when it's dropped here.
        match event {
            wl_keyboard::Event::Key { serial, state: WEnum::Value(wl_keyboard::KeyState::Pressed), .. }
            | wl_keyboard::Event::Enter { serial, .. } => sh.input_serial = Some(serial),
            _ => {}
        }
    }
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
        let mut sh = state.lock();
        match event {
            wl_data_device::Event::Selection { id } => {
                if let Some(old) = std::mem::replace(&mut sh.selection, id) {
                    old.destroy();
                }
            }
            wl_data_device::Event::Motion { x, y, .. } => sh.pos = (x, y),
            wl_data_device::Event::Enter { serial, surface, id, x, y } => {
                sh.pos = (x, y);
                if let Some(old) = sh.drag_offer.take() {
                    old.destroy();
                }
                // Only accept our own tab drags; other apps' drags pass through untouched.
                match (id, sh.drag_source.is_some()) {
                    (Some(offer), true) => {
                        offer.accept(serial, Some(TAB_MIME.into()));
                        offer.set_actions(DndAction::Move, DndAction::Move);
                        sh.drag_offer = Some(offer);
                        sh.offer_action = false;
                        sh.over = Some(surface_key(&surface));
                    }
                    (offer, _) => {
                        if let Some(o) = offer {
                            o.destroy();
                        }
                        sh.over = None;
                    }
                }
            }
            wl_data_device::Event::Leave => {
                if let Some(o) = sh.drag_offer.take() {
                    o.destroy();
                }
                sh.over = None;
            }
            wl_data_device::Event::Drop => {
                if let Some(target) = sh.over {
                    if let Some(o) = &sh.drag_offer
                        && sh.offer_action
                    {
                        o.finish();
                    }
                    let (x, y) = sh.pos;
                    sh.end_drag(Outcome::Dropped { surface: target, x, y });
                    drop(sh);
                    (state.wake)();
                }
            }
            _ => {}
        }
    }

    event_created_child!(State, WlDataDevice, [
        wl_data_device::EVT_DATA_OFFER_OPCODE => (WlDataOffer, OfferMimes::default()),
    ]);
}

impl Dispatch<WlDataOffer, OfferMimes> for State {
    fn event(
        state: &mut Self,
        _: &WlDataOffer,
        event: wl_data_offer::Event,
        mimes: &OfferMimes,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let mut sh = state.lock();
        match event {
            wl_data_offer::Event::Offer { mime_type } => {
                if let Ok(mut m) = mimes.0.lock()
                    && m.len() < MAX_MIMES
                {
                    m.push(mime_type);
                }
            }
            wl_data_offer::Event::Action { dnd_action: WEnum::Value(a) } => {
                sh.offer_action = a.contains(DndAction::Move);
            }
            _ => {}
        }
    }
}

impl Dispatch<WlDataSource, SourceKind> for State {
    fn event(
        state: &mut Self,
        source: &WlDataSource,
        event: wl_data_source::Event,
        kind: &SourceKind,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let mut sh = state.lock();
        match (kind, event) {
            // Another app is pasting our text: write it on a thread so a slow reader
            // can't block the editor. Every offered type is UTF-8 compatible text.
            (SourceKind::Clipboard(text), wl_data_source::Event::Send { fd, .. }) => {
                // Too many writes already in flight: refuse by closing the pipe.
                if WRITERS.fetch_add(1, Ordering::AcqRel) >= MAX_WRITERS {
                    WRITERS.fetch_sub(1, Ordering::AcqRel);
                    return;
                }
                let text = Arc::clone(text);
                let spawned = std::thread::Builder::new().name("clipboard-write".into()).spawn(move || {
                    let mut f = std::fs::File::from(fd);
                    let _ = f.write_all(text.as_bytes());
                    WRITERS.fetch_sub(1, Ordering::AcqRel);
                });
                if spawned.is_err() {
                    WRITERS.fetch_sub(1, Ordering::AcqRel);
                }
            }
            (SourceKind::Clipboard(_), wl_data_source::Event::Cancelled) => {
                // Someone else took the clipboard.
                if sh.clip_source.as_ref() == Some(source) {
                    sh.clip_source = None;
                }
                source.destroy();
            }
            // Nothing outside this process accepts the tab MIME type, so no data is ever sent.
            (SourceKind::Drag, wl_data_source::Event::Send { fd, .. }) => drop(fd),
            (SourceKind::Drag, wl_data_source::Event::Cancelled) => {
                sh.end_drag(Outcome::Elsewhere);
                drop(sh);
                (state.wake)();
            }
            (SourceKind::Drag, wl_data_source::Event::DndFinished) => {
                let (target, (x, y)) = (sh.over, sh.pos);
                sh.end_drag(target.map_or(Outcome::Elsewhere, |surface| Outcome::Dropped { surface, x, y }));
                drop(sh);
                (state.wake)();
            }
            _ => {}
        }
    }
}
