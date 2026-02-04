//! Async file descriptor wrapper.
//!
//! Registers a raw fd with the tau-rt reactor and provides async
//! readability/writability polling.

use std::io;
use std::os::unix::io::RawFd;
use std::task::Poll;

use async_ffi::ContextExt;

use crate::ffi;

/// A file descriptor registered with the tau-rt reactor for async IO.
///
/// `AsyncFd` does NOT own the file descriptor — it only manages the reactor
/// registration. The caller is responsible for closing the fd (e.g., via `OwnedFd`).
pub struct AsyncFd {
    handle: u64,
    fd: RawFd,
}

impl AsyncFd {
    /// Register a file descriptor with the reactor.
    pub fn new(fd: RawFd) -> io::Result<Self> {
        let handle = unsafe { ffi::tau_rt_io_register(fd) };
        Ok(AsyncFd { handle, fd })
    }

    /// Returns the raw file descriptor.
    pub fn as_raw_fd(&self) -> RawFd {
        self.fd
    }

    /// Returns the reactor handle.
    pub fn handle(&self) -> u64 {
        self.handle
    }

    /// Wait until the fd is readable.
    ///
    /// After this returns `Ok(())`, you should attempt the read operation.
    /// If it returns `WouldBlock`, call `readable()` again (spurious wake).
    pub async fn readable(&self) -> io::Result<()> {
        std::future::poll_fn(|cx| {
            cx.with_ffi_context(|ffi_cx| {
                let result =
                    unsafe { ffi::tau_rt_io_poll_readable(self.handle, ffi_cx as *mut _) };
                match result {
                    1 => Poll::Ready(Ok(())),
                    0 => Poll::Pending,
                    _ => Poll::Ready(Err(io::Error::other(
                        "unexpected poll_readable result",
                    ))),
                }
            })
        })
        .await
    }

    /// Wait until the fd is writable.
    ///
    /// After this returns `Ok(())`, you should attempt the write operation.
    /// If it returns `WouldBlock`, call `writable()` again (spurious wake).
    pub async fn writable(&self) -> io::Result<()> {
        std::future::poll_fn(|cx| {
            cx.with_ffi_context(|ffi_cx| {
                let result =
                    unsafe { ffi::tau_rt_io_poll_writable(self.handle, ffi_cx as *mut _) };
                match result {
                    1 => Poll::Ready(Ok(())),
                    0 => Poll::Pending,
                    _ => Poll::Ready(Err(io::Error::other(
                        "unexpected poll_writable result",
                    ))),
                }
            })
        })
        .await
    }
}

impl Drop for AsyncFd {
    fn drop(&mut self) {
        unsafe { ffi::tau_rt_io_deregister(self.handle) };
    }
}
