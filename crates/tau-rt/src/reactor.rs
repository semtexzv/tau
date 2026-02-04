use std::collections::{BTreeMap, HashMap};
use std::io;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::task::{Poll, Waker};
use std::time::{Duration, Instant};

use polling::{Event, Events, Poller};
use slab::Slab;

/// An IO source registered with the reactor.
pub(crate) struct Source {
    raw_fd: i32,
    key: usize,
    /// Whether we've called poller.add() for this source.
    registered: bool,
    /// Waker to fire when readable.
    read_waker: Option<Waker>,
    /// Waker to fire when writable.
    write_waker: Option<Waker>,
    /// Set by react() when OS reports readable; cleared by poll_readable.
    read_ready: bool,
    /// Set by react() when OS reports writable; cleared by poll_writable.
    write_ready: bool,
}

/// Timer state: BTreeMap for ordered expiry iteration, HashMap for handle→deadline lookup.
struct TimerState {
    /// Timers ordered by (deadline, id) for efficient expiry scanning.
    heap: BTreeMap<(Instant, u64), Waker>,
    /// Reverse lookup: timer id → deadline, for cancel and poll by handle.
    deadlines: HashMap<u64, Instant>,
}

/// The global reactor: owns the OS poller, IO sources, and timer heap.
pub(crate) struct Reactor {
    poller: Poller,
    sources: Mutex<Slab<Source>>,
    timers: Mutex<TimerState>,
    timer_id: AtomicU64,
    events: Mutex<Events>,
}

static REACTOR: OnceLock<Reactor> = OnceLock::new();

pub(crate) fn get() -> &'static Reactor {
    REACTOR.get_or_init(|| Reactor {
        poller: Poller::new().expect("failed to create OS poller"),
        sources: Mutex::new(Slab::new()),
        timers: Mutex::new(TimerState {
            heap: BTreeMap::new(),
            deadlines: HashMap::new(),
        }),
        timer_id: AtomicU64::new(0),
        events: Mutex::new(Events::new()),
    })
}

impl Reactor {
    // ── IO ──────────────────────────────────────────────────────────

    /// Register a file descriptor. Returns an opaque handle (slab key).
    /// The fd is NOT added to the OS poller yet — that happens on first poll.
    pub(crate) fn io_register(&self, fd: i32) -> u64 {
        let mut sources = self.sources.lock().unwrap();
        let entry = sources.vacant_entry();
        let key = entry.key();
        entry.insert(Source {
            raw_fd: fd,
            key,
            registered: false,
            read_waker: None,
            write_waker: None,
            read_ready: false,
            write_ready: false,
        });
        key as u64
    }

    /// Deregister an IO source. Removes from OS poller if registered.
    pub(crate) fn io_deregister(&self, handle: u64) {
        let mut sources = self.sources.lock().unwrap();
        let key = handle as usize;
        if sources.contains(key) {
            let source = sources.remove(key);
            if source.registered {
                let borrowed = unsafe { std::os::fd::BorrowedFd::borrow_raw(source.raw_fd) };
                // Ignore errors — fd may already be closed by caller.
                let _ = self.poller.delete(&borrowed);
            }
        }
    }

    /// Poll for readability. Stores waker and registers interest.
    /// Returns Ready if already known readable, Pending otherwise.
    pub(crate) fn io_poll_readable(&self, handle: u64, waker: Waker) -> Poll<()> {
        let mut sources = self.sources.lock().unwrap();
        let key = handle as usize;
        let source = &mut sources[key];

        if source.read_ready {
            source.read_ready = false;
            return Poll::Ready(());
        }

        source.read_waker = Some(waker);
        self.update_interest(source);
        Poll::Pending
    }

    /// Poll for writability. Stores waker and registers interest.
    pub(crate) fn io_poll_writable(&self, handle: u64, waker: Waker) -> Poll<()> {
        let mut sources = self.sources.lock().unwrap();
        let key = handle as usize;
        let source = &mut sources[key];

        if source.write_ready {
            source.write_ready = false;
            return Poll::Ready(());
        }

        source.write_waker = Some(waker);
        self.update_interest(source);
        Poll::Pending
    }

    /// Sync OS poller interest with current waker state.
    fn update_interest(&self, source: &mut Source) {
        let interest = Event::new(
            source.key,
            source.read_waker.is_some(),
            source.write_waker.is_some(),
        );

        if source.registered {
            let borrowed = unsafe { std::os::fd::BorrowedFd::borrow_raw(source.raw_fd) };
            // modify() re-arms oneshot interest.
            let _ = self.poller.modify(&borrowed, interest);
        } else {
            // First registration — add() is unsafe because we must delete before fd close.
            unsafe {
                let _ = self.poller.add(source.raw_fd, interest);
            }
            source.registered = true;
        }
    }

    // ── Timers ──────────────────────────────────────────────────────

    /// Create a timer that fires `nanos_from_now` nanoseconds from now.
    /// Returns an opaque timer handle.
    pub(crate) fn timer_create(&self, nanos_from_now: u64) -> u64 {
        let id = self.timer_id.fetch_add(1, Ordering::Relaxed);
        let deadline = Instant::now() + Duration::from_nanos(nanos_from_now);
        let mut state = self.timers.lock().unwrap();
        state.deadlines.insert(id, deadline);
        // Waker is stored on first timer_poll, not here.
        id
    }

    /// Cancel a pending timer. The stored waker (if any) is dropped, not woken.
    pub(crate) fn timer_cancel(&self, id: u64) {
        let mut state = self.timers.lock().unwrap();
        if let Some(deadline) = state.deadlines.remove(&id) {
            state.heap.remove(&(deadline, id));
        }
    }

    /// Poll a timer. Returns Ready if deadline passed, Pending otherwise.
    pub(crate) fn timer_poll(&self, id: u64, waker: Waker) -> Poll<()> {
        let mut state = self.timers.lock().unwrap();
        let deadline = match state.deadlines.get(&id) {
            Some(&d) => d,
            None => return Poll::Ready(()), // Already fired or cancelled.
        };

        if Instant::now() >= deadline {
            state.deadlines.remove(&id);
            state.heap.remove(&(deadline, id));
            return Poll::Ready(());
        }

        // Not yet expired — store/replace waker.
        state.heap.insert((deadline, id), waker);
        Poll::Pending
    }

    // ── React (drives IO + timers) ─────────────────────────────────

    /// Process expired timers, poll OS for IO events, wake ready tasks.
    pub(crate) fn react(&self, timeout: Option<Duration>) -> io::Result<()> {
        let mut wakers = Vec::new();

        // 1. Process expired timers.
        let next_timer = {
            let now = Instant::now();
            let mut state = self.timers.lock().unwrap();
            loop {
                match state.heap.keys().next().copied() {
                    Some((deadline, id)) if deadline <= now => {
                        let waker = state.heap.remove(&(deadline, id)).unwrap();
                        state.deadlines.remove(&id);
                        wakers.push(waker);
                    }
                    Some((deadline, _)) => break Some(deadline.duration_since(now)),
                    None => break None,
                }
            }
        };

        // 2. Compute effective timeout: min(caller, next_timer).
        let effective_timeout = match (timeout, next_timer) {
            (None, None) => None,
            (Some(t), None) | (None, Some(t)) => Some(t),
            (Some(a), Some(b)) => Some(a.min(b)),
        };

        // 3. Poll OS for IO events.
        let event_list: Vec<(usize, bool, bool)> = {
            let mut events = self.events.lock().unwrap();
            events.clear();
            self.poller.wait(&mut events, effective_timeout)?;
            events
                .iter()
                .map(|ev| (ev.key, ev.readable, ev.writable))
                .collect()
        };

        // 4. Process IO events — collect wakers.
        {
            let mut sources = self.sources.lock().unwrap();
            for (key, readable, writable) in event_list {
                if let Some(source) = sources.get_mut(key) {
                    if readable {
                        source.read_ready = true;
                        if let Some(waker) = source.read_waker.take() {
                            wakers.push(waker);
                        }
                    }
                    if writable {
                        source.write_ready = true;
                        if let Some(waker) = source.write_waker.take() {
                            wakers.push(waker);
                        }
                    }
                }
            }
        }

        // 5. Wake all — tasks re-enter the executor's ready queue.
        for waker in wakers {
            waker.wake();
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reactor_initializes() {
        let reactor = get();
        // Verify we can lock all mutexes (no poisoning).
        drop(reactor.sources.lock().unwrap());
        drop(reactor.timers.lock().unwrap());
        drop(reactor.events.lock().unwrap());
    }

    #[test]
    fn timer_create_and_poll_expired() {
        let reactor = get();

        // Create a timer that fires immediately (0 nanos).
        let id = reactor.timer_create(0);

        // Small sleep to ensure Instant::now() >= deadline.
        std::thread::sleep(Duration::from_millis(1));

        // Poll should return Ready.
        let waker = futures_waker();
        assert_eq!(reactor.timer_poll(id, waker), Poll::Ready(()));
    }

    #[test]
    fn timer_create_and_poll_pending() {
        let reactor = get();

        // Create a timer 1 second from now.
        let id = reactor.timer_create(1_000_000_000);

        // Poll should return Pending (not expired yet).
        let waker = futures_waker();
        assert_eq!(reactor.timer_poll(id, waker), Poll::Pending);

        // Clean up.
        reactor.timer_cancel(id);
    }

    #[test]
    fn timer_cancel_removes_entry() {
        let reactor = get();
        let id = reactor.timer_create(1_000_000_000);
        let waker = futures_waker();
        assert_eq!(reactor.timer_poll(id, waker), Poll::Pending);

        // Cancel the timer.
        reactor.timer_cancel(id);

        // Poll after cancel should return Ready (no entry found).
        let waker = futures_waker();
        assert_eq!(reactor.timer_poll(id, waker), Poll::Ready(()));
    }

    #[test]
    fn react_fires_expired_timers() {
        let reactor = get();

        // Create a timer that fires in 10ms.
        let id = reactor.timer_create(10_000_000);
        let flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag_clone = flag.clone();

        // Store a waker that sets the flag.
        let waker = waker_from_fn(move || {
            flag_clone.store(true, Ordering::SeqCst);
        });
        assert_eq!(reactor.timer_poll(id, waker), Poll::Pending);

        // React with enough timeout for the timer to fire.
        std::thread::sleep(Duration::from_millis(15));
        reactor.react(Some(Duration::ZERO)).unwrap();

        assert!(flag.load(Ordering::SeqCst), "timer waker should have fired");
    }

    // ── Test helpers ────────────────────────────────────────────────

    /// Create a no-op waker for testing.
    fn futures_waker() -> Waker {
        waker_from_fn(|| {})
    }

    /// Create a waker that calls the given closure when woken.
    fn waker_from_fn(f: impl Fn() + Send + Sync + 'static) -> Waker {
        use std::sync::Arc;
        use std::task::{RawWaker, RawWakerVTable};

        struct WakerData(Box<dyn Fn() + Send + Sync>);

        unsafe fn clone_fn(data: *const ()) -> RawWaker {
            let arc = Arc::from_raw(data as *const WakerData);
            let cloned = arc.clone();
            std::mem::forget(arc); // Don't drop the original.
            RawWaker::new(Arc::into_raw(cloned) as *const (), &VTABLE)
        }
        unsafe fn wake_fn(data: *const ()) {
            let arc = Arc::from_raw(data as *const WakerData);
            (arc.0)();
        }
        unsafe fn wake_by_ref_fn(data: *const ()) {
            let arc = Arc::from_raw(data as *const WakerData);
            (arc.0)();
            std::mem::forget(arc); // Don't drop — we borrowed.
        }
        unsafe fn drop_fn(data: *const ()) {
            drop(Arc::from_raw(data as *const WakerData));
        }

        static VTABLE: RawWakerVTable =
            RawWakerVTable::new(clone_fn, wake_fn, wake_by_ref_fn, drop_fn);

        let data = Arc::new(WakerData(Box::new(f)));
        let raw = RawWaker::new(Arc::into_raw(data) as *const (), &VTABLE);
        unsafe { Waker::from_raw(raw) }
    }
}
