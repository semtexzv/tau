//! tau-iface: Safe Rust wrappers around the tau-rt C ABI.
//!
//! This crate provides idiomatic async/await APIs for IO, timers, and task
//! spawning. All operations delegate to the shared `libtau_rt` runtime
//! through FFI — no statics, globals, or thread-locals in this crate.
//!
//! # Quick Start
//!
//! ```ignore
//! use tau_iface::{spawn, sleep, block_on, TcpStream};
//! use std::time::Duration;
//!
//! block_on(async {
//!     spawn(async {
//!         sleep(Duration::from_millis(100)).await;
//!         println!("timer fired!");
//!     });
//!
//!     let stream = TcpStream::connect("127.0.0.1:8080".parse().unwrap()).await.unwrap();
//!     stream.write(b"hello").await.unwrap();
//! });
//! ```

pub mod ffi;

pub mod async_fd;
pub mod timer;
pub mod tcp;
pub mod udp;

// Re-exports for convenience
pub use async_fd::AsyncFd;
pub use tcp::{TcpListener, TcpStream};
pub use timer::Timer;
pub use udp::UdpSocket;

use std::future::Future;
use std::io;
use std::time::Duration;

use async_ffi::{FfiFuture, FutureExt};

/// Spawn a future onto the shared executor.
///
/// The future will be polled by `try_tick()` or `block_on()`.
pub fn spawn<F>(future: F)
where
    F: Future<Output = ()> + Send + 'static,
{
    let ffi_future: FfiFuture<()> = future.into_ffi();
    unsafe { ffi::tau_rt_spawn(ffi_future) };
}

/// Sleep for the given duration.
///
/// This is a convenience wrapper around `Timer::after(duration).await`.
pub async fn sleep(duration: Duration) {
    Timer::after(duration).await
}

/// Block the current thread until the future completes.
///
/// Drives both the reactor (IO + timers) and executor (spawned tasks)
/// internally until the future resolves.
pub fn block_on<F>(future: F)
where
    F: Future<Output = ()> + Send + 'static,
{
    let ffi_future: FfiFuture<()> = future.into_ffi();
    unsafe { ffi::tau_rt_block_on(ffi_future) };
}

/// Poll one ready task from the executor queue.
///
/// Returns `true` if a task was polled, `false` if the queue was empty.
pub fn try_tick() -> bool {
    unsafe { ffi::tau_rt_try_tick() != 0 }
}

/// Run the reactor once: process expired timers, poll OS for IO events.
///
/// - `Some(duration)` — wait up to `duration` for events
/// - `None` — wait indefinitely until an event occurs
pub fn react(timeout: Option<Duration>) -> io::Result<()> {
    let timeout_ms = match timeout {
        Some(d) => {
            let ms = d.as_millis();
            if ms > u64::MAX as u128 {
                u64::MAX
            } else {
                ms as u64
            }
        }
        None => u64::MAX, // effectively infinite
    };
    let result = unsafe { ffi::tau_rt_react(timeout_ms) };
    if result < 0 {
        Err(io::Error::other("reactor error"))
    } else {
        Ok(())
    }
}
