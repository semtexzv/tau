//! Async timer.
//!
//! Creates a one-shot timer via the tau-rt reactor and provides a `Future`
//! implementation that resolves when the deadline expires.

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use async_ffi::ContextExt;

use crate::ffi;

/// A one-shot timer that resolves after a given duration.
///
/// Created via [`Timer::after`]. Implements `Future` so you can `.await` it.
/// Cancels the timer on drop if it hasn't fired yet.
pub struct Timer {
    handle: u64,
    fired: bool,
}

impl Timer {
    /// Create a timer that fires after the given duration.
    pub fn after(duration: Duration) -> Self {
        let nanos = duration.as_nanos().min(u64::MAX as u128) as u64;
        let handle = unsafe { ffi::tau_rt_timer_create(nanos) };
        Timer {
            handle,
            fired: false,
        }
    }
}

impl Future for Timer {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if self.fired {
            return Poll::Ready(());
        }
        cx.with_ffi_context(|ffi_cx| {
            let result = unsafe { ffi::tau_rt_timer_poll(self.handle, ffi_cx as *mut _) };
            match result {
                1 => {
                    self.fired = true;
                    Poll::Ready(())
                }
                0 => Poll::Pending,
                _ => {
                    // Shouldn't happen, but treat as ready to avoid hangs
                    self.fired = true;
                    Poll::Ready(())
                }
            }
        })
    }
}

impl Drop for Timer {
    fn drop(&mut self) {
        if !self.fired {
            unsafe { ffi::tau_rt_timer_cancel(self.handle) };
        }
    }
}
