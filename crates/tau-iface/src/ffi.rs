//! FFI declarations for tau-rt C ABI.
//!
//! These mirror the exports from `libtau_rt.dylib` / `libtau_rt.so` exactly.
//! Linked at load time via `#[link(name = "tau_rt")]`.

use async_ffi::{FfiContext, FfiFuture};

#[link(name = "tau_rt")]
extern "C" {
    // ── IO ──────────────────────────────────────────────────────────

    /// Register a file descriptor with the reactor. Returns an opaque handle.
    pub fn tau_rt_io_register(fd: i32) -> u64;

    /// Deregister and remove an IO source.
    pub fn tau_rt_io_deregister(handle: u64);

    /// Poll for readability. Returns 0=Pending, 1=Ready.
    pub fn tau_rt_io_poll_readable(handle: u64, cx: *mut FfiContext<'_>) -> u8;

    /// Poll for writability. Returns 0=Pending, 1=Ready.
    pub fn tau_rt_io_poll_writable(handle: u64, cx: *mut FfiContext<'_>) -> u8;

    // ── Timers ──────────────────────────────────────────────────────

    /// Create a timer. Deadline is nanoseconds from now. Returns opaque handle.
    pub fn tau_rt_timer_create(nanos_from_now: u64) -> u64;

    /// Cancel a pending timer.
    pub fn tau_rt_timer_cancel(handle: u64);

    /// Poll a timer. Returns 0=Pending, 1=Ready.
    pub fn tau_rt_timer_poll(handle: u64, cx: *mut FfiContext<'_>) -> u8;

    // ── Executor ────────────────────────────────────────────────────

    /// Spawn a future onto the shared executor.
    pub fn tau_rt_spawn(future: FfiFuture<()>);

    /// Poll one ready task. Returns 0=no work, 1=did work.
    pub fn tau_rt_try_tick() -> u8;

    /// Run the reactor once (process IO + timers, wake tasks).
    /// timeout_ms: milliseconds to wait. 0 = non-blocking.
    /// Returns 0=ok, -1=error.
    pub fn tau_rt_react(timeout_ms: u64) -> i32;

    /// Block the current thread until the future completes.
    pub fn tau_rt_block_on(future: FfiFuture<()>);
}
