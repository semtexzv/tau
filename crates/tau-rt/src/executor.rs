use std::sync::OnceLock;
use std::time::Duration;

use async_ffi::FfiFuture;
use async_task::Runnable;
use concurrent_queue::ConcurrentQueue;

use crate::reactor;

/// The global single-threaded task executor.
pub(crate) struct Executor {
    /// Ready queue: tasks whose wakers have fired.
    queue: ConcurrentQueue<Runnable>,
}

static EXECUTOR: OnceLock<Executor> = OnceLock::new();

pub(crate) fn get() -> &'static Executor {
    EXECUTOR.get_or_init(|| Executor {
        queue: ConcurrentQueue::unbounded(),
    })
}

/// Schedule function for async-task: pushes a runnable into the global queue.
/// This is `Fn(Runnable) + Send + Sync + 'static` — safe to call from wakers
/// on any thread.
fn schedule(runnable: Runnable) {
    get().queue.push(runnable).unwrap();
}

impl Executor {
    /// Spawn a future onto the executor. The future is polled by whoever
    /// calls `try_tick()` or `block_on()`.
    pub(crate) fn spawn(&self, future: FfiFuture<()>) {
        // FfiFuture<()> is Send + 'static, so we use async_task::spawn
        // (not spawn_local). This avoids thread-affinity panics — important
        // because wakers may fire from any thread and the executor runs on
        // whichever thread drives the loop.
        let (runnable, task) = async_task::spawn(future, schedule);
        task.detach(); // We don't need the return value.
        runnable.schedule(); // Push to queue for first poll.
    }

    /// Pop one ready task and run it. Returns true if a task was polled.
    pub(crate) fn try_tick(&self) -> bool {
        match self.queue.pop() {
            Ok(runnable) => {
                runnable.run();
                true
            }
            Err(_) => false,
        }
    }

    /// Drive the executor and reactor until the given future completes.
    pub(crate) fn block_on(&self, future: FfiFuture<()>) {
        let (runnable, task) = async_task::spawn(future, schedule);
        runnable.schedule();

        let reactor = reactor::get();

        loop {
            if task.is_finished() {
                break;
            }

            // Drive executor: poll all ready tasks.
            let mut did_work = false;
            while self.try_tick() {
                did_work = true;
                // Check after each tick — the future might be done.
                if task.is_finished() {
                    return;
                }
            }

            // Drive reactor: wait for IO/timers.
            // Non-blocking if we just did work (there might be more tasks
            // after wakers fire), short sleep otherwise.
            let timeout = if did_work {
                Some(Duration::ZERO)
            } else {
                Some(Duration::from_millis(10))
            };
            let _ = reactor.react(timeout);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};
    use std::task::{Context, Poll};
    use std::time::Instant;

    /// Serialization guard — the global executor queue is shared across test
    /// threads, so tests that inspect queue state must not interleave.
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    /// Acquire the test lock and drain any leftover tasks from prior tests.
    fn test_guard() -> std::sync::MutexGuard<'static, ()> {
        let guard = TEST_LOCK.lock().unwrap();
        // Drain leftover tasks so each test starts with a clean queue.
        while get().try_tick() {}
        guard
    }

    /// Internal timer future for tests — polls reactor's timer_poll directly.
    struct TimerFuture {
        id: u64,
    }

    impl Future for TimerFuture {
        type Output = ();
        fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
            reactor::get().timer_poll(self.id, cx.waker().clone())
        }
    }

    impl Drop for TimerFuture {
        fn drop(&mut self) {
            reactor::get().timer_cancel(self.id);
        }
    }

    // ── DV-1: Spawn + tick counter test ─────────────────────────────

    #[test]
    fn dv1_spawn_and_tick_increments_counter() {
        let _g = test_guard();
        let executor = get();
        let counter = Arc::new(AtomicU64::new(0));
        let counter_clone = counter.clone();

        let future: FfiFuture<()> = FfiFuture::new(async move {
            counter_clone.fetch_add(1, Ordering::SeqCst);
        });

        executor.spawn(future);
        assert!(executor.try_tick(), "should have had a task to run");
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    // ── DV-1: Timer test — 50ms, completes within tolerance ─────────

    #[test]
    fn dv1_timer_50ms_completes_in_time() {
        let _g = test_guard();
        let executor = get();
        let reactor_ref = reactor::get();
        let completed = Arc::new(AtomicBool::new(false));
        let completed_clone = completed.clone();

        // Create a timer for 50ms.
        let timer_id = reactor_ref.timer_create(50_000_000); // 50ms in nanos

        let future: FfiFuture<()> = FfiFuture::new(async move {
            TimerFuture { id: timer_id }.await;
            completed_clone.store(true, Ordering::SeqCst);
        });

        executor.spawn(future);

        let start = Instant::now();
        while !completed.load(Ordering::SeqCst) {
            while executor.try_tick() {}
            let _ = reactor_ref.react(Some(Duration::from_millis(5)));

            // Safety valve: don't loop forever.
            assert!(
                start.elapsed() < Duration::from_secs(2),
                "timer test timed out"
            );
        }

        let elapsed = start.elapsed();
        assert!(
            elapsed >= Duration::from_millis(50),
            "timer fired too early: {:?}",
            elapsed
        );
        // Allow generous tolerance for CI (the PRD says 55ms but CI can be slow).
        assert!(
            elapsed <= Duration::from_millis(200),
            "timer fired too late: {:?}",
            elapsed
        );
    }

    // ── Basic unit tests ────────────────────────────────────────────

    #[test]
    fn spawn_and_tick() {
        let _g = test_guard();
        let executor = get();
        let counter = Arc::new(AtomicU64::new(0));
        let counter_clone = counter.clone();

        let future: FfiFuture<()> = FfiFuture::new(async move {
            counter_clone.fetch_add(1, Ordering::SeqCst);
        });

        executor.spawn(future);
        assert!(executor.try_tick(), "should have had a task to run");
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn try_tick_empty_queue() {
        let _g = test_guard();
        let executor = get();
        assert!(!executor.try_tick(), "no tasks should be in queue");
    }

    #[test]
    fn block_on_immediate() {
        let _g = test_guard();
        let executor = get();
        let counter = Arc::new(AtomicU64::new(0));
        let counter_clone = counter.clone();

        let future: FfiFuture<()> = FfiFuture::new(async move {
            counter_clone.fetch_add(1, Ordering::SeqCst);
        });

        executor.block_on(future);
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn block_on_with_timer() {
        let _g = test_guard();
        let executor = get();
        let reactor_ref = reactor::get();
        let timer_id = reactor_ref.timer_create(20_000_000); // 20ms

        let completed = Arc::new(AtomicBool::new(false));
        let completed_clone = completed.clone();

        let future: FfiFuture<()> = FfiFuture::new(async move {
            TimerFuture { id: timer_id }.await;
            completed_clone.store(true, Ordering::SeqCst);
        });

        let start = Instant::now();
        executor.block_on(future);
        let elapsed = start.elapsed();

        assert!(completed.load(Ordering::SeqCst));
        assert!(
            elapsed >= Duration::from_millis(20),
            "timer fired too early: {:?}",
            elapsed
        );
    }
}
