//! C ABI exports for tau-rt.
//!
//! All functions are `#[no_mangle] pub extern "C"` and use only FFI-safe types.
//! These are the ONLY public interface of the shared library.

use async_ffi::{FfiContext, FfiFuture};

use crate::{executor, reactor};

// ── IO ──────────────────────────────────────────────────────────────

/// Register a file descriptor with the reactor. Returns an opaque handle.
#[no_mangle]
pub extern "C" fn tau_rt_io_register(fd: i32) -> u64 {
    reactor::get().io_register(fd)
}

/// Deregister and remove an IO source.
#[no_mangle]
pub extern "C" fn tau_rt_io_deregister(handle: u64) {
    reactor::get().io_deregister(handle);
}

/// Poll for readability. Returns 0=Pending, 1=Ready.
/// If Pending, stores the waker from `cx` and wakes it when readable.
#[no_mangle]
pub extern "C" fn tau_rt_io_poll_readable(handle: u64, cx: *mut FfiContext<'_>) -> u8 {
    let ffi_cx = unsafe { &mut *cx };
    ffi_cx.with_context(|std_cx| {
        let waker = std_cx.waker().clone();
        match reactor::get().io_poll_readable(handle, waker) {
            std::task::Poll::Pending => 0,
            std::task::Poll::Ready(()) => 1,
        }
    })
}

/// Poll for writability. Returns 0=Pending, 1=Ready.
#[no_mangle]
pub extern "C" fn tau_rt_io_poll_writable(handle: u64, cx: *mut FfiContext<'_>) -> u8 {
    let ffi_cx = unsafe { &mut *cx };
    ffi_cx.with_context(|std_cx| {
        let waker = std_cx.waker().clone();
        match reactor::get().io_poll_writable(handle, waker) {
            std::task::Poll::Pending => 0,
            std::task::Poll::Ready(()) => 1,
        }
    })
}

// ── Timers ──────────────────────────────────────────────────────────

/// Create a timer. Deadline is nanoseconds from now. Returns opaque handle.
#[no_mangle]
pub extern "C" fn tau_rt_timer_create(nanos_from_now: u64) -> u64 {
    reactor::get().timer_create(nanos_from_now)
}

/// Cancel a pending timer.
#[no_mangle]
pub extern "C" fn tau_rt_timer_cancel(handle: u64) {
    reactor::get().timer_cancel(handle);
}

/// Poll a timer. Returns 0=Pending, 1=Ready.
#[no_mangle]
pub extern "C" fn tau_rt_timer_poll(handle: u64, cx: *mut FfiContext<'_>) -> u8 {
    let ffi_cx = unsafe { &mut *cx };
    ffi_cx.with_context(|std_cx| {
        let waker = std_cx.waker().clone();
        match reactor::get().timer_poll(handle, waker) {
            std::task::Poll::Pending => 0,
            std::task::Poll::Ready(()) => 1,
        }
    })
}

// ── Executor ────────────────────────────────────────────────────────

/// Spawn a future onto the shared executor.
#[no_mangle]
pub extern "C" fn tau_rt_spawn(future: FfiFuture<()>) {
    executor::get().spawn(future);
}

/// Poll one ready task. Returns 0=no work, 1=did work.
#[no_mangle]
pub extern "C" fn tau_rt_try_tick() -> u8 {
    if executor::get().try_tick() {
        1
    } else {
        0
    }
}

/// Run the reactor once (process IO + timers, wake tasks).
/// timeout_ms: milliseconds to wait. 0 = non-blocking.
/// Returns 0=ok, -1=error.
#[no_mangle]
pub extern "C" fn tau_rt_react(timeout_ms: u64) -> i32 {
    let timeout = if timeout_ms == 0 {
        Some(std::time::Duration::ZERO)
    } else {
        Some(std::time::Duration::from_millis(timeout_ms))
    };
    match reactor::get().react(timeout) {
        Ok(()) => 0,
        Err(_) => -1,
    }
}

/// Block the current thread until the future completes.
/// Drives both reactor and executor internally.
#[no_mangle]
pub extern "C" fn tau_rt_block_on(future: FfiFuture<()>) {
    executor::get().block_on(future);
}
