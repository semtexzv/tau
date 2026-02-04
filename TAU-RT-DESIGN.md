# tau-rt: Shared Runtime Design

## Problem

tau needs async IO (HTTP streaming, process execution, timers) and an extension
system where plugins can also do async work. Rust async runtimes (tokio, smol,
async-io) all use process-global statics for their reactor. A plugin compiled as
`cdylib` gets its own copy of those statics, creating a second reactor nobody
drives.

We verified (see `EXTENSIONS.md`) that no annotation or linking trick avoids
this — it's inherent to separate compilation with static linking.

## Solution

Split the runtime into a **shared C-ABI dynamic library** that both the host
binary and plugin cdylibs link against. The dynamic linker loads it exactly
once, so all parties share the same reactor and executor.

```
┌──────────────────────────────────────────────────────────────────────┐
│  Process memory                                                      │
│                                                                      │
│  libtau_rt.dylib / libtau_rt.so   (loaded once by dyld/ld.so)       │
│  ┌────────────────────────────────────────────────────────────────┐  │
│  │  Reactor (polling::Poller + timer BTreeMap)                    │  │
│  │  Executor (task queue, single-threaded)                        │  │
│  │  #[no_mangle] extern "C" functions                             │  │
│  └────────────────────────────────────────────────────────────────┘  │
│          ↑                        ↑                                  │
│     dynamic link             dynamic link                            │
│          │                        │                                  │
│  ┌───────┴────────┐     ┌────────┴────────┐                         │
│  │  tau binary     │     │  plugin.cdylib  │                         │
│  │  (drives loop)  │     │  (submits work) │                         │
│  │  uses tau-iface │     │  uses tau-iface │                         │
│  └────────────────┘     └─────────────────┘                         │
│                                                                      │
└──────────────────────────────────────────────────────────────────────┘
```

Verified: on macOS `DYLD_PRINT_LIBRARIES` confirms one load; on Linux
`LD_DEBUG=libs` equivalent. A prototype with shared `AtomicU64` counter + task
queue proved host and plugin share the same globals.

---

## Crate Layout

### `tau-rt` (crate-type = ["cdylib"] only)

**cdylib only — no rlib.** If tau-rt were also an rlib, cargo would statically
link it into host and plugin binaries, creating duplicate statics and defeating
the shared-library design. By being cdylib-only, all access is forced through
the C ABI declarations in tau-iface. Nobody can `use tau_rt::anything` directly.

The **single source of truth** for all async runtime state:

- **Reactor** — wraps `polling::Poller` (epoll/kqueue/IOCP), manages registered
  IO sources and a timer heap. One global instance behind `OnceLock`.
- **Executor** — single-threaded task queue. Accepts `FfiFuture<()>` tasks.
  The host calls `tau_rt_tick()` to poll ready tasks.
- **C ABI exports** — every function is `#[no_mangle] pub extern "C"`. All
  types crossing the boundary are `#[repr(C)]` or primitive.

```toml
# crates/tau-rt/Cargo.toml
[lib]
crate-type = ["cdylib"]  # ONLY cdylib. No rlib. No staticlib.
```

Dependencies (all zero-global-state):
- `polling` — OS-level IO polling (epoll/kqueue/IOCP)
- `async-task` — low-level task allocation (no globals)
- `async-ffi` — `#[repr(C)]` future/waker bridge
- `slab` — indexed storage for IO sources
- `concurrent-queue` — timer operation queue

**Forbidden dependencies:** tokio, async-io, smol, async-std, reqwest.

### `tau-iface` (crate-type = rlib)

A **pure declaration** crate. Contains zero runtime state:

- `extern "C"` blocks declaring every `tau_rt_*` function
- `#[link(name = "tau_rt")]` to resolve against the shared library
- Safe Rust wrapper types: `AsyncFd`, `Timer`, `TcpStream`, `TcpListener`
- `spawn()`, `sleep()`, `block_on()` convenience functions
- Types implement `Future` by calling the C ABI poll functions internally

Dependencies: only `async-ffi` (for `FfiFuture`/`FfiContext` types).

**This is what extensions depend on.** No statics, no globals.

### `tau-http` (crate-type = rlib)

HTTP/1.1 client built entirely on `tau-iface` primitives:

- TCP connection via `tau-iface::TcpStream`
- TLS via `rustls` (pure Rust, no runtime dependency)
- HTTP parsing via `httparse` (no runtime dependency)
- Timeouts via `tau-iface::Timer`
- SSE streaming for LLM APIs

Dependencies: `tau-iface`, `rustls`, `httparse`. **No tokio, no reqwest.**

---

## Reactor Design

IO and timers share the same behavior: **register interest → sleep → wake when
ready**. The reactor treats them uniformly.

### IO Sources

```rust
// Inside tau-rt
pub(crate) struct Source {
    /// Registration in the OS poller
    raw: polling::Source,
    key: usize,
    /// Waker for readable interest
    read_waker: Option<FfiWaker>,
    /// Waker for writable interest
    write_waker: Option<FfiWaker>,
}
```

An IO source is a file descriptor registered with the OS poller. When polled for
readability/writability, the caller's waker is stored. When the reactor runs and
the OS reports the fd as ready, the stored waker fires.

### Timers

```rust
// Inside tau-rt
struct TimerEntry {
    deadline: Instant,
    id: u64,
    waker: FfiWaker,
}

// Stored in: BTreeMap<(Instant, u64), FfiWaker>
```

A timer is a deadline + waker. When the reactor runs, it checks for expired
timers before polling IO. The IO poll timeout is `min(caller_timeout,
next_timer_deadline)`.

### Unified react() Loop

```rust
// Inside tau-rt (simplified)
fn react(reactor: &Reactor, timeout: Option<Duration>) -> io::Result<()> {
    let mut wakers = Vec::new();

    // 1. Process expired timers → collect wakers
    let next_timer = reactor.process_timers(&mut wakers);

    // 2. Compute poll timeout
    let timeout = match (next_timer, timeout) {
        (None, None) => None,
        (Some(t), None) | (None, Some(t)) => Some(t),
        (Some(a), Some(b)) => Some(a.min(b)),
    };

    // 3. Poll OS for IO events
    reactor.poller.wait(&mut reactor.events, timeout)?;

    // 4. Collect wakers from ready IO sources
    for event in reactor.events.iter() {
        if let Some(source) = reactor.sources.get(event.key) {
            if event.readable { wakers.extend(source.read_waker.take()); }
            if event.writable { wakers.extend(source.write_waker.take()); }
        }
    }

    // 5. Wake all — tasks re-enter the executor's ready queue
    for waker in wakers {
        waker.wake();
    }

    Ok(())
}
```

The host's main loop:

```rust
// Host (tau binary or test harness)
loop {
    // Drive the executor — poll ready tasks
    while tau_iface::try_tick() {}

    // Drive the reactor — wait for IO/timers, wake tasks
    tau_iface::react(Some(Duration::from_millis(10)))?;

    // Also check for terminal input, etc.
}
```

---

## C ABI Surface

### Handles

```rust
// All handles are opaque u64 IDs. No pointers cross the boundary.
#[repr(transparent)]
pub struct IoHandle(u64);

#[repr(transparent)]
pub struct TimerHandle(u64);
```

### IO Functions

```c
// Register a file descriptor with the reactor. Returns an opaque handle.
IoHandle tau_rt_io_register(int32_t fd);

// Deregister and remove an IO source.
void tau_rt_io_deregister(IoHandle handle);

// Poll for readability. Returns FfiPoll (Ready/Pending/Panicked).
// If Pending, stores the waker from `cx` and wakes it when readable.
FfiPoll tau_rt_io_poll_readable(IoHandle handle, FfiContext* cx);

// Poll for writability. Same contract.
FfiPoll tau_rt_io_poll_writable(IoHandle handle, FfiContext* cx);
```

### Timer Functions

```c
// Create a timer. Deadline is nanoseconds from now.
TimerHandle tau_rt_timer_create(uint64_t deadline_nanos);

// Cancel a pending timer.
void tau_rt_timer_cancel(TimerHandle handle);

// Poll a timer. Ready when deadline passed, Pending otherwise.
FfiPoll tau_rt_timer_poll(TimerHandle handle, FfiContext* cx);
```

### Executor Functions

```c
// Spawn a future onto the shared executor.
void tau_rt_spawn(FfiFuture future);

// Poll one ready task. Returns true if a task was polled.
bool tau_rt_try_tick();

// Run the reactor once (process IO + timers, wake tasks).
// timeout_ms = 0 for non-blocking, u64::MAX for blocking.
int32_t tau_rt_react(uint64_t timeout_ms);

// Block the current thread until the future completes.
// Drives both reactor and executor internally.
void tau_rt_block_on(FfiFuture future);
```

### Waker Transport

`async-ffi` provides `FfiContext` — a `#[repr(C)]` wrapper around
`std::task::Context`. When tau-rt polls an `FfiFuture`, it converts its internal
`Waker` to `FfiContext`. The future's `poll` receives this `FfiContext`. When the
future calls back into tau-rt (e.g. `tau_rt_io_poll_readable`), it passes the
same `FfiContext`, which tau-rt converts back to a `Waker` and stores in the
reactor.

**Wakers never need to be created or understood by extensions.** They're opaque
tokens forwarded between tau-rt calls.

---

## tau-iface Safe Wrappers

### AsyncFd

```rust
/// Async wrapper around a raw file descriptor.
/// IO readiness is managed by tau-rt's reactor.
pub struct AsyncFd {
    fd: RawFd,
    handle: IoHandle,
}

impl AsyncFd {
    pub fn new(fd: RawFd) -> io::Result<Self> {
        let handle = unsafe { tau_rt_io_register(fd as i32) };
        Ok(Self { fd, handle })
    }

    pub async fn readable(&self) -> io::Result<()> {
        poll_fn(|cx| {
            let ffi_cx = FfiContext::from_std(cx);
            match unsafe { tau_rt_io_poll_readable(self.handle, &ffi_cx) } {
                FfiPoll::Ready(()) => Poll::Ready(Ok(())),
                FfiPoll::Pending => Poll::Pending,
                FfiPoll::Panicked => Poll::Ready(Err(io::Error::other("panicked"))),
            }
        }).await
    }

    pub async fn writable(&self) -> io::Result<()> { /* symmetric */ }
}

impl Drop for AsyncFd {
    fn drop(&mut self) {
        unsafe { tau_rt_io_deregister(self.handle); }
    }
}
```

### Timer

```rust
pub struct Timer {
    handle: TimerHandle,
}

impl Timer {
    pub fn after(duration: Duration) -> Self {
        let handle = unsafe { tau_rt_timer_create(duration.as_nanos() as u64) };
        Self { handle }
    }
}

impl Future for Timer {
    type Output = ();
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let ffi_cx = FfiContext::from_std(cx);
        match unsafe { tau_rt_timer_poll(self.handle, &ffi_cx) } {
            FfiPoll::Ready(()) => Poll::Ready(()),
            FfiPoll::Pending => Poll::Pending,
            FfiPoll::Panicked => panic!("timer panicked"),
        }
    }
}

impl Drop for Timer {
    fn drop(&mut self) {
        unsafe { tau_rt_timer_cancel(self.handle); }
    }
}
```

### TcpStream

```rust
pub struct TcpStream {
    fd: AsyncFd,
}

impl TcpStream {
    pub async fn connect(addr: SocketAddr) -> io::Result<Self> {
        let socket = std::net::TcpStream::connect_nonblocking(addr)?;
        let fd = AsyncFd::new(socket.as_raw_fd())?;
        fd.writable().await?; // wait for connect to complete
        Ok(Self { fd })
    }

    pub async fn read(&self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            self.fd.readable().await?;
            match rustix::net::recv(self.fd.fd, buf, RecvFlags::empty()) {
                Ok(n) => return Ok(n),
                Err(e) if e == rustix::io::Errno::WOULDBLOCK => continue,
                Err(e) => return Err(e.into()),
            }
        }
    }

    pub async fn write(&self, buf: &[u8]) -> io::Result<usize> {
        loop {
            self.fd.writable().await?;
            match rustix::net::send(self.fd.fd, buf, SendFlags::empty()) {
                Ok(n) => return Ok(n),
                Err(e) if e == rustix::io::Errno::WOULDBLOCK => continue,
                Err(e) => return Err(e.into()),
            }
        }
    }
}
```

### spawn / sleep / block_on

```rust
pub fn spawn(future: impl Future<Output = ()> + Send + 'static) {
    let ffi = FfiFuture::new(future);
    unsafe { tau_rt_spawn(ffi); }
}

pub async fn sleep(duration: Duration) {
    Timer::after(duration).await
}

pub fn block_on(future: impl Future<Output = ()> + Send + 'static) {
    let ffi = FfiFuture::new(future);
    unsafe { tau_rt_block_on(ffi); }
}
```

---

## Dependency Enforcement

### Rule: No foreign runtimes anywhere in the dependency tree

tau-rt, tau-iface, tau-http, tau-tui, tau-ai, tau-agent, tau-cli, and all
extensions must **never** depend on:

- `tokio` (any feature)
- `async-io`
- `smol`
- `async-std`
- `async-global-executor`
- `reqwest` (pulls tokio)
- `hyper` (pulls tokio)

### Enforcement: `tau-check-deps` build script

Every crate in the workspace and the extension template includes a build script:

```rust
// build.rs
fn main() {
    let forbidden = [
        "tokio", "async-io", "smol", "async-std",
        "async-global-executor", "reqwest", "hyper",
    ];

    // DEP_ env vars are set by cargo for each dependency
    for (key, _) in std::env::vars() {
        if key.starts_with("DEP_") {
            let dep_name = key
                .trim_start_matches("DEP_")
                .split('_')
                .next()
                .unwrap_or("")
                .to_lowercase();
            for f in &forbidden {
                if dep_name == *f {
                    panic!(
                        "FORBIDDEN DEPENDENCY: `{}` detected. \
                         tau extensions must use tau-iface for async, not {}.",
                        f, f
                    );
                }
            }
        }
    }
}
```

Additionally, a **workspace-level CI check**:

```bash
#!/bin/bash
# ci/check-deps.sh — run in CI and as a pre-commit hook
FORBIDDEN="tokio async-io smol async-std async-global-executor reqwest hyper"
for crate in $(cargo metadata --format-version 1 | jq -r '.packages[].name'); do
    deps=$(cargo tree -p "$crate" --depth 999 --prefix none 2>/dev/null | awk '{print $1}')
    for f in $FORBIDDEN; do
        if echo "$deps" | grep -q "^${f}$"; then
            echo "ERROR: $crate depends on forbidden crate '$f'"
            exit 1
        fi
    done
done
echo "OK: no forbidden runtime dependencies"
```

### Extension template

The `tau-ext-template` project ships with:

```toml
# Cargo.toml
[dependencies]
tau-iface = { version = "0.1" }
# No tokio. No reqwest. Use tau-iface for async IO.

[build-dependencies]
# Includes the forbidden-dep checker
```

Documentation clearly states: **"Use `tau_iface::TcpStream` for networking,
`tau_iface::Timer` for timeouts, `tau_iface::spawn` for tasks.
Do not add tokio, async-io, smol, or reqwest."**

---

## HTTP Client (tau-http)

No reqwest. HTTP is built from primitives:

```
tau-http
├── tau-iface::TcpStream     (async TCP, uses shared reactor)
├── tau-iface::Timer          (timeouts, uses shared reactor)
├── rustls                    (TLS, pure Rust, no runtime dep)
├── httparse                  (HTTP/1.1 parsing, no runtime dep)
└── manual SSE parser         (~50 lines for event-stream)
```

### Key types

```rust
pub struct HttpClient {
    // connection pool keyed by (host, port, tls)
}

pub struct HttpResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    body: TcpStream, // or TlsStream<TcpStream>
}

impl HttpResponse {
    /// Read the full body
    pub async fn text(self) -> io::Result<String> { ... }

    /// Stream SSE events
    pub fn sse_stream(self) -> impl Stream<Item = SseEvent> { ... }
}

pub struct SseEvent {
    pub event: Option<String>,
    pub data: String,
}
```

The Anthropic provider uses `tau_http::HttpClient` instead of `reqwest`.

---

## Impact on Existing PRD

The current PRD uses `tokio` throughout. This design replaces tokio with tau-rt:

| Before (tokio)              | After (tau-rt)                          |
|-----------------------------|-----------------------------------------|
| `tokio::spawn`              | `tau_iface::spawn`                      |
| `tokio::time::sleep`        | `tau_iface::sleep`                      |
| `tokio::sync::mpsc`         | `std::sync::mpsc` or `async-channel`    |
| `tokio::select!`            | manual poll in `block_on` loop          |
| `reqwest::Client`           | `tau_http::HttpClient`                  |
| `tokio::process::Command`   | `std::process::Command` + `AsyncFd`     |
| `#[tokio::main]`            | `tau_iface::block_on(async { ... })`    |
| `crossterm::EventStream`    | `AsyncFd` on stdin + crossterm parsing  |

---

## Extension API

With tau-rt shared, extensions get full async capabilities:

```rust
// my_extension/src/lib.rs (compiled as cdylib)
use tau_iface::{spawn, sleep, TcpStream, Timer};
use tau_ext_api::{ExtensionApi, ToolResult};
use std::time::Duration;

#[no_mangle]
pub extern "C" fn tau_ext_init(api: &mut ExtensionApi) {
    api.register_tool("web_fetch", "Fetch a URL", schema, |params| {
        // This closure returns an FfiFuture.
        // The host executor polls it. It uses the SHARED reactor.
        async move {
            let stream = TcpStream::connect(addr).await?;
            let resp = tau_http::get(&url).await?;
            sleep(Duration::from_millis(100)).await; // shared timer heap
            Ok(ToolResult::text(resp.text().await?))
        }.into_ffi()
    });
}
```

The extension uses `async/await` naturally. All IO goes through the shared
reactor in `libtau_rt.so`. No separate runtime, no duplicate globals.

---

## Verification Plan

Each PRD story below includes a **design verification test** ensuring the
tau-rt architecture works correctly at that stage.

### DV-1: Shared state across dylib boundary
Build tau-rt as cdylib, host binary and plugin cdylib both link it.
Host increments counter, loads plugin, plugin increments, host reads
final value. Counter is consistent. (Already proven in prototype.)

### DV-2: Reactor drives IO for both host and plugin
Host opens a localhost TCP listener. Plugin connects via
`tau_iface::TcpStream`. Host accepts, writes data. Plugin reads it.
All IO goes through the shared reactor.

### DV-3: Timers fire correctly across boundary
Host spawns a task with `sleep(100ms)`. Plugin spawns a task with
`sleep(200ms)`. Host drives reactor. Both timers fire at correct times
(±5ms tolerance).

### DV-4: FfiFuture polling works end-to-end
Plugin returns an `FfiFuture` from a tool execution. Host's executor
polls it. The future does async IO (TCP connect + read) and returns a
result. Host receives the result.

### DV-5: Forbidden dependency check catches violations
A test extension adds `tokio` to its deps. `cargo build` fails with
clear error message from the build script.
