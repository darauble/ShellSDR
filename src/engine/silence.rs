//! Temporary stdout/stderr silencer (hardware noise suppression).
//!
//! Some SoapySDR drivers print directly to the process's file descriptors
//! instead of (or in addition to) routing through `SoapySDR_logf`. When that
//! text lands on fd 1 while the ratatui alternate screen is active, it
//! scrolls and corrupts the display until the terminal is resized.
//!
//! `StdioSilencer` redirects fds 1 and 2 to `/dev/null` with `dup2` for its
//! lifetime and restores the originals on drop. It is scoped tightly around
//! the calls that provoke native prints (device open, stream activate) so the
//! terminal is only "deaf" for a few milliseconds.
//!
//! The redirect is process-wide: used on the DSP thread (the stream start),
//! it also swallows what the UI thread draws meanwhile, so the DSP thread
//! asks the UI for a full repaint afterwards (`Engine::take_repaint`).
//!
//! The primary defense is the no-op `log` handler installed in `main`; this
//! guard is the belt-and-suspenders layer for drivers that bypass it. It is
//! Unix-only and verified on hardware, so it is excluded from the unit suite.

#[cfg(unix)]
use std::os::fd::RawFd;

/// Redirects stdout (fd 1) and stderr (fd 2) to `/dev/null` until dropped.
#[cfg(unix)]
pub struct StdioSilencer {
    saved_out: Option<RawFd>,
    saved_err: Option<RawFd>,
    devnull: Option<RawFd>,
}

#[cfg(unix)]
impl StdioSilencer {
    /// Begin silencing. On any failure the affected stream is left untouched,
    /// so this never panics and degrades gracefully to "no silencing".
    pub fn new() -> Self {
        // SAFETY: all args are valid; failures are checked and handled.
        unsafe {
            let devnull = libc_open_devnull();
            let (saved_out, saved_err) = if devnull >= 0 {
                let so = dup_and_redirect(1, devnull);
                let se = dup_and_redirect(2, devnull);
                (so, se)
            } else {
                (None, None)
            };
            StdioSilencer {
                saved_out,
                saved_err,
                devnull: if devnull >= 0 { Some(devnull) } else { None },
            }
        }
    }
}

#[cfg(unix)]
impl Default for StdioSilencer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(unix)]
impl Drop for StdioSilencer {
    fn drop(&mut self) {
        // SAFETY: fds are ones we created/saved; restore then close.
        unsafe {
            if let Some(fd) = self.saved_out {
                dup2(fd, 1);
                close(fd);
            }
            if let Some(fd) = self.saved_err {
                dup2(fd, 2);
                close(fd);
            }
            if let Some(fd) = self.devnull {
                close(fd);
            }
        }
    }
}

#[cfg(unix)]
unsafe extern "C" {
    fn open(path: *const std::ffi::c_char, oflag: i32, ...) -> i32;
    fn dup(oldfd: i32) -> i32;
    fn dup2(oldfd: i32, newfd: i32) -> i32;
    fn close(fd: i32) -> i32;
}

/// `O_WRONLY` on Linux.
#[cfg(unix)]
const O_WRONLY: i32 = 1;

#[cfg(unix)]
unsafe fn libc_open_devnull() -> i32 {
    let path = c"/dev/null";
    unsafe { open(path.as_ptr(), O_WRONLY) }
}

/// Save `target`'s current fd (via `dup`) and point `target` at `devnull`.
/// Returns the saved fd to restore later, or `None` if `dup` failed.
#[cfg(unix)]
unsafe fn dup_and_redirect(target: i32, devnull: i32) -> Option<i32> {
    unsafe {
        let saved = dup(target);
        if saved < 0 {
            return None;
        }
        dup2(devnull, target);
        Some(saved)
    }
}

/// No-op silencer on non-Unix platforms.
#[cfg(not(unix))]
pub struct StdioSilencer;

#[cfg(not(unix))]
impl StdioSilencer {
    pub fn new() -> Self {
        StdioSilencer
    }
}

#[cfg(not(unix))]
impl Default for StdioSilencer {
    fn default() -> Self {
        Self::new()
    }
}
