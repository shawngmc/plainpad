//! Returning freed memory to the operating system.

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
