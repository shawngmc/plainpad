//! Returning freed memory to the operating system.

/// Tunes glibc's allocator for a long-running editor (call once, first thing).
///
/// - Large blocks (128 KB and up: pasted text, file buffers, big undo steps)
///   always get their own mapping, returned to the OS as soon as they're freed.
///   By default glibc raises this threshold after freeing a large block, up to
///   32 MB, after which big temporary buffers are carved out of the main heap
///   among small long-lived allocations, and the gaps they leave can't be
///   returned. A soak test of pasting, editing and reloading kept ~48 MB that
///   way after six minutes.
/// - At most two allocator arenas. Short-lived threads (clipboard reads, file
///   jobs) would otherwise each leave an arena behind, holding freed memory.
pub fn init() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        const M_MMAP_THRESHOLD: i32 = -3;
        const M_ARENA_MAX: i32 = -8;
        unsafe extern "C" {
            fn mallopt(param: i32, value: i32) -> i32;
        }
        // SAFETY: mallopt only changes glibc allocator settings; called before other threads exist.
        unsafe {
            mallopt(M_MMAP_THRESHOLD, 128 * 1024);
            mallopt(M_ARENA_MAX, 2);
        }
    }
}

/// Hands freed heap memory back to the OS. glibc's allocator otherwise keeps
/// it for reuse: after closing a large document the process would stay at its
/// peak size (33 MB after closing a 27 MB file, versus 1.8 MB after trimming).
/// A no-op elsewhere; other allocators release memory on their own terms.
pub fn trim() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        unsafe extern "C" {
            fn malloc_trim(pad: usize) -> i32;
        }
        // SAFETY: malloc_trim only walks and trims glibc's own heap.
        unsafe {
            malloc_trim(0);
        }
    }
}
