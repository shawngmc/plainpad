//! Putting frames on screen on Wayland, with less memory than softbuffer.
//!
//! softbuffer always keeps two full-window buffers (rounded up to a power of
//! two, and never shrunk). Most compositors copy a frame and hand the buffer
//! back right away, so this uses a single buffer and only adds a second while
//! the compositor still holds the first. Buffers are the exact window size and
//! are dropped on resize. About a second after the last frame, the memory of any
//! buffer the compositor has handed back is returned to the kernel; every frame
//! is repainted in full, so a freed (zero-filled) buffer costs nothing to reuse.

use super::*;
use memmap2::{MmapMut, MmapOptions};
use std::os::fd::{AsFd, OwnedFd};
use std::sync::{Condvar, Weak};
use std::time::{Duration, Instant};
use wayland_client::protocol::wl_buffer::{self, WlBuffer};
use wayland_client::protocol::wl_shm::{self, WlShm};
use wayland_client::protocol::wl_shm_pool::WlShmPool;

/// Idle time after the last frame before handed-back buffers are freed.
const TRIM_AFTER: Duration = Duration::from_secs(1);

/// A buffer's state, shared by the main thread (drawing), the dispatch thread
/// (the compositor's release events) and the trimmer thread (freeing memory).
pub(super) struct BufState {
    fd: OwnedFd,
    len: u64,
    /// Attached and not yet released by the compositor: we mustn't write to it.
    busy: AtomicBool,
    /// Its memory has been returned to the kernel.
    trimmed: AtomicBool,
    /// Held while drawing and presenting, and while trimming.
    lock: Mutex<()>,
    /// Set when a frame is waiting for a buffer to be released.
    waiting: Arc<AtomicBool>,
}

struct ShmBuf {
    state: Arc<BufState>,
    map: MmapMut,
    pool: WlShmPool,
    buffer: WlBuffer,
    w: u32,
    h: u32,
}

impl Drop for ShmBuf {
    fn drop(&mut self) {
        self.buffer.destroy();
        self.pool.destroy();
    }
}

/// One window's frame buffers.
pub struct Framebuffer {
    conn: Connection,
    qh: QueueHandle<State>,
    shm: WlShm,
    surface: WlSurface,
    bufs: Vec<ShmBuf>,
    waiting: Arc<AtomicBool>,
    trimmer: Arc<Trimmer>,
}

impl Framebuffer {
    pub(super) fn new(
        conn: Connection,
        qh: QueueHandle<State>,
        shm: WlShm,
        surface: WlSurface,
        trimmer: Arc<Trimmer>,
    ) -> Self {
        Self { conn, qh, shm, surface, bufs: Vec::new(), waiting: Arc::default(), trimmer }
    }

    fn new_buf(&self, w: u32, h: u32) -> Option<ShmBuf> {
        let len = u64::from(w) * u64::from(h) * 4;
        let len_i32 = i32::try_from(len).ok()?;
        let fd = rustix::fs::memfd_create(
            "plainpad-frame",
            rustix::fs::MemfdFlags::CLOEXEC | rustix::fs::MemfdFlags::ALLOW_SEALING,
        )
        .ok()?;
        rustix::fs::ftruncate(&fd, len).ok()?;
        // The compositor maps this too; promise it won't shrink under it.
        let _ = rustix::fs::fcntl_add_seals(&fd, rustix::fs::SealFlags::SHRINK);
        // SAFETY: a fresh memfd of `len` bytes that only we and the compositor use.
        let map = unsafe { MmapOptions::new().len(len as usize).map_mut(&fd) }.ok()?;
        let pool = self.shm.create_pool(fd.as_fd(), len_i32, &self.qh, ());
        let state = Arc::new(BufState {
            fd,
            len,
            busy: AtomicBool::new(false),
            trimmed: AtomicBool::new(false),
            lock: Mutex::new(()),
            waiting: Arc::clone(&self.waiting),
        });
        let buffer = pool.create_buffer(
            0,
            w as i32,
            h as i32,
            (w * 4) as i32,
            wl_shm::Format::Xrgb8888,
            &self.qh,
            Arc::clone(&state),
        );
        Some(ShmBuf { state, map, pool, buffer, w, h })
    }

    /// Draws a `w`×`h` frame with `paint` (0x00RRGGBB pixels, row-major) and
    /// presents it. Returns false without drawing if every buffer is still held
    /// by the compositor; the event loop is woken when one is released.
    pub fn draw(&mut self, w: u32, h: u32, paint: impl FnOnce(&mut [u32])) -> bool {
        // A resize: drop buffers of the old size, freeing their memory.
        self.bufs.retain(|b| b.w == w && b.h == h);
        let idx = match self.bufs.iter().position(|b| !b.state.busy.load(Ordering::Acquire)) {
            Some(i) => i,
            None if self.bufs.len() < 2 => match self.new_buf(w, h) {
                Some(b) => {
                    self.bufs.push(b);
                    self.bufs.len() - 1
                }
                None => return true, // can't allocate; skip the frame rather than spin
            },
            None => {
                self.waiting.store(true, Ordering::Release);
                return false;
            }
        };
        let b = &mut self.bufs[idx];
        let state = Arc::clone(&b.state);
        let _guard = state.lock.lock().unwrap_or_else(|e| e.into_inner());
        state.trimmed.store(false, Ordering::Release);
        let n = (w * h) as usize;
        // SAFETY: the mapping is page-aligned and exactly w*h*4 bytes long, and the
        // compositor isn't reading it (not busy).
        let px = unsafe { std::slice::from_raw_parts_mut(b.map.as_mut_ptr().cast::<u32>(), n) };
        paint(px);
        self.surface.attach(Some(&b.buffer), 0, 0);
        if self.surface.version() >= 4 {
            self.surface.damage_buffer(0, 0, w as i32, h as i32);
        } else {
            self.surface.damage(0, 0, i32::MAX, i32::MAX);
        }
        self.surface.commit();
        state.busy.store(true, Ordering::Release);
        let _ = self.conn.flush();
        self.trimmer.touch(&state);
        true
    }
}

/// Frees the memory of idle, handed-back buffers after a quiet period. It sleeps
/// on a condition variable, so an idle editor causes no wakeups.
pub(super) struct Trimmer {
    inner: Mutex<TrimInner>,
    cv: Condvar,
}

#[derive(Default)]
struct TrimInner {
    bufs: Vec<Weak<BufState>>,
    /// When the last frame was presented; `None` once trimmed.
    last: Option<Instant>,
    stop: bool,
}

impl Trimmer {
    pub(super) fn start() -> (Arc<Self>, Option<std::thread::JoinHandle<()>>) {
        let t = Arc::new(Trimmer { inner: Mutex::default(), cv: Condvar::new() });
        let t2 = Arc::clone(&t);
        let h = std::thread::Builder::new().name("frame-trim".into()).spawn(move || t2.run()).ok();
        (t, h)
    }

    fn lock(&self) -> MutexGuard<'_, TrimInner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn touch(&self, b: &Arc<BufState>) {
        let mut g = self.lock();
        g.bufs.retain(|w| w.strong_count() > 0);
        if !g.bufs.iter().any(|w| std::ptr::eq(w.as_ptr(), Arc::as_ptr(b))) {
            g.bufs.push(Arc::downgrade(b));
        }
        g.last = Some(Instant::now());
        self.cv.notify_one();
    }

    pub(super) fn stop(&self) {
        self.lock().stop = true;
        self.cv.notify_one();
    }

    fn run(&self) {
        let mut g = self.lock();
        loop {
            if g.stop {
                return;
            }
            let Some(last) = g.last else {
                g = self.cv.wait(g).unwrap_or_else(|e| e.into_inner());
                continue;
            };
            let idle = last.elapsed();
            if idle < TRIM_AFTER {
                g = self.cv.wait_timeout(g, TRIM_AFTER - idle).unwrap_or_else(|e| e.into_inner()).0;
                continue;
            }
            g.last = None;
            let bufs: Vec<Arc<BufState>> = g.bufs.iter().filter_map(Weak::upgrade).collect();
            drop(g);
            for b in &bufs {
                trim(b);
            }
            g = self.lock();
        }
    }
}

/// Returns a handed-back buffer's memory to the kernel. Skips it if it's being
/// drawn into, held by the compositor, or already trimmed.
fn trim(b: &BufState) {
    let Ok(_guard) = b.lock.try_lock() else { return };
    if b.busy.load(Ordering::Acquire) || b.trimmed.swap(true, Ordering::AcqRel) {
        return;
    }
    #[cfg(target_os = "linux")]
    {
        use rustix::fs::FallocateFlags;
        let _ = rustix::fs::fallocate(&b.fd, FallocateFlags::PUNCH_HOLE | FallocateFlags::KEEP_SIZE, 0, b.len);
    }
}

impl Dispatch<WlShm, ()> for State {
    fn event(_: &mut Self, _: &WlShm, _: wl_shm::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<WlShmPool, ()> for State {
    fn event(
        _: &mut Self,
        _: &WlShmPool,
        _: <WlShmPool as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlBuffer, Arc<BufState>> for State {
    fn event(
        state: &mut Self,
        _: &WlBuffer,
        event: wl_buffer::Event,
        buf: &Arc<BufState>,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_buffer::Event::Release = event {
            buf.busy.store(false, Ordering::Release);
            // A frame was waiting for a free buffer: have the event loop redraw.
            if buf.waiting.swap(false, Ordering::AcqRel) {
                (state.wake)();
            }
        }
    }
}
