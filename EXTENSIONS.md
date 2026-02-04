# tau Extensions: Investigation

## The Problem

tau needs a runtime extension system where users write Rust source files that:
- Register custom tools the LLM can call
- Subscribe to agent lifecycle events
- Do async work (HTTP, timers, file IO)
- Can be reloaded without restarting tau

Pi-mono solves this with `jiti` (JIT TypeScript compiler) — `.ts` files loaded at runtime, sharing the same JS event loop. Rust has no equivalent runtime, so we must choose a mechanism.

## Core Tension

Extensions need **async** (network IO, timers, etc.), but Rust's async ecosystem is deeply tied to a specific runtime (tokio). Sharing a runtime across a dynamic boundary is the central challenge.

---

## Option A: IPC / Child Process

**Extensions are compiled to binaries, spawned as child processes, communicate via JSON over stdin/stdout.**

```
tau (parent)                    extension (child)
    │                               │
    │── {"execute_tool":...} ──→    │
    │                               │  own tokio runtime
    │                               │  can await, spawn, etc.
    │  ←── {"result":...} ─────    │
```

| Aspect | Detail |
|--------|--------|
| **Async** | ✅ Full — own tokio runtime, own event loop. Timers, HTTP, channels all work. |
| **ABI** | ✅ None — JSON over pipes. Any language works. |
| **Isolation** | ✅ Extension crash doesn't crash tau. |
| **Reload** | ✅ Kill process, recompile, respawn. |
| **Performance** | ⚠️ JSON serialization + pipe IO per call. ~50-200µs overhead per message. Fine for tool execution (ms-scale). |
| **Complexity** | Medium — need IPC protocol, process lifecycle management. |
| **Developer UX** | Good — `tau-ext` crate hides IPC. Extension author writes normal async Rust. |
| **Multi-language** | ✅ Any language that reads/writes JSON lines. |

**Example extension:**
```rust
use tau_ext::prelude::*;

#[tau_ext::main]
async fn setup(api: &mut ExtensionApi) {
    api.register_tool("web_search", "Search the web", schema, |params| async {
        let resp = reqwest::get(&url).await?;   // just works
        tokio::time::sleep(Duration::from_secs(1)).await;  // just works
        Ok(ToolResult::text(resp.text().await?))
    });
}
```

**Verdict:** Safest, most flexible. Best async story. IPC overhead is negligible for tool-execution workloads (tools already take 100ms+). Only downside: can't share in-process state with tau.

---

## Option B: cdylib + C ABI (no async)

**Extensions compiled to `.so`/`.dylib`, loaded via `libloading`. Pure C ABI boundary — all data as JSON strings. Sync only.**

```rust
// Extension (cdylib)
#[no_mangle]
pub extern "C" fn tau_extension(api: *mut TauExtApi) { ... }

#[no_mangle]
pub extern "C" fn execute_tool(params: *const c_char) -> *const c_char { ... }
```

| Aspect | Detail |
|--------|--------|
| **Async** | ❌ None. Extensions are sync functions. Can only `std::thread::spawn` + block. |
| **ABI** | ✅ Stable C ABI. JSON strings cross the boundary. |
| **Isolation** | ❌ Extension panic/crash = tau crash (even with `catch_unwind`, some panics are UB). |
| **Reload** | ⚠️ `dlclose` + `dlopen`. Works but fragile (leaked state, thread-locals). |
| **Performance** | ✅ Near-native. One JSON serialize/deserialize per call. |
| **Complexity** | Medium — FFI safety, `#[repr(C)]`, memory ownership rules. |
| **Developer UX** | Poor — authors must understand C ABI, unsafe, memory ownership. |
| **Multi-language** | ✅ Anything that produces cdylib with C exports. |

**Verdict:** Fast, but no async kills it for our use case. Extensions that need HTTP would have to block a thread. No timers, no `tokio::time::sleep`. Not acceptable for a tool that might need to call APIs.

---

## Option C: cdylib + `async-ffi`

**Like Option B, but uses [`async-ffi`](https://docs.rs/async-ffi) crate to pass `FfiFuture<T>` (a `#[repr(C)]` future) across the dylib boundary. tau polls the future on its tokio runtime.**

```rust
// Extension (cdylib)
use async_ffi::{FfiFuture, FutureExt};

#[no_mangle]
pub extern "C" fn execute_tool(params: *const c_char) -> FfiFuture<*const c_char> {
    async move {
        // tau polls this future on its tokio runtime
        // BUT: tokio primitives (sleep, spawn, etc.) WON'T WORK here
        // because the extension has its own copy of tokio with different thread-locals
        let result = do_sync_work();
        CString::new(result).unwrap().into_raw() as *const c_char
    }.into_ffi()
}
```

| Aspect | Detail |
|--------|--------|
| **Async** | ⚠️ **Partial.** Can return futures that tau polls. BUT: `tokio::time::sleep()`, `tokio::spawn()`, `tokio::net` etc. **will panic** — the extension's tokio copy can't find a reactor because it's a different compilation unit. Only "pure computation" futures work. |
| **ABI** | ✅ `FfiFuture` is `#[repr(C)]`. Panic-safe. |
| **Isolation** | ❌ Same process. Panic caught by `async-ffi`, but resource leaks possible. |
| **Reload** | ⚠️ Same as Option B — fragile `dlclose`/`dlopen`. |
| **Performance** | ✅ Near-native + one Box allocation per future. |
| **Complexity** | High — C ABI + async-ffi + careful avoidance of tokio in extensions. |
| **Developer UX** | Poor — "you can use async/await but NOT any of the ecosystem" is confusing. |

**The fundamental problem:** `async-ffi` solves future *transport* across the boundary. It does NOT give the extension access to an async *runtime*. Tokio timers, IO, spawning — anything that needs a reactor — won't work because the extension has its own copy of tokio with its own thread-locals that aren't initialized.

**Verdict:** Async in name only. Extension authors would be constantly confused about what works and what panics. Not recommended.

---

## Option D: cdylib + Host-provided async primitives

**Like Option C, but tau provides async primitives (sleep, spawn, HTTP) as C ABI function pointers. Extensions never import tokio — they call back into tau for all async operations.**

```rust
// tau passes this vtable to the extension:
#[repr(C)]
pub struct TauHostApi {
    ctx: *mut c_void,
    sleep_ms: extern "C" fn(ctx: *mut c_void, ms: u64) -> FfiFuture<()>,
    spawn: extern "C" fn(ctx: *mut c_void, future: FfiFuture<()>),
    http_get: extern "C" fn(ctx: *mut c_void, url: *const c_char) -> FfiFuture<*const c_char>,
    send_event: extern "C" fn(ctx: *mut c_void, json: *const c_char),
}

// Extension uses host API:
async fn my_tool(api: &TauHostApi) {
    (api.sleep_ms)(api.ctx, 1000).await;  // calls tau's tokio::time::sleep
    let resp = (api.http_get)(api.ctx, c"https://...").await;
}
```

| Aspect | Detail |
|--------|--------|
| **Async** | ⚠️ Limited to what tau exposes. sleep, spawn, HTTP — yes. Extension's own IO reactor — no. Arbitrary ecosystem crates (reqwest) — no. |
| **ABI** | ✅ C ABI + `async-ffi` for futures. |
| **Isolation** | ❌ Same process. |
| **Reload** | ⚠️ Fragile. |
| **Performance** | ✅ Near-native. |
| **Complexity** | High — must design and maintain a host API surface. Every new async primitive = new function pointer. |
| **Developer UX** | Moderate — clean API, but can't use ecosystem crates. Want `reqwest`? Can't. Must use `api.http_get`. |

**Verdict:** Workable but constraining. The host API surface grows forever. Extension authors can't use normal Rust ecosystem crates for async work.

---

## Option E: WASM Component Model (wasmtime/extism)

**Extensions compiled to `.wasm` components. Loaded by `wasmtime` runtime embedded in tau. Interface defined via WIT (WebAssembly Interface Types).**

```wit
// extension.wit
package tau:ext;

interface tools {
    record tool-def {
        name: string,
        description: string,
        parameters-json: string,
    }
    
    execute: func(params-json: string) -> result<string, string>;
}

world extension {
    export tools;
}
```

| Aspect | Detail |
|--------|--------|
| **Async** | ⚠️ WASI Preview 2 adds async support. wasmtime supports `async_support(true)` — host can poll wasm calls. But wasm guest can't spawn tasks or use tokio. Host functions can be async. |
| **ABI** | ✅ Fully defined by WIT. Language-independent. |
| **Isolation** | ✅ Full sandbox. No filesystem, no network (unless host grants). CPU/memory limits. |
| **Reload** | ✅ Just load a new `.wasm` file. No dlclose issues. |
| **Performance** | ⚠️ 1.5-3x native. Memory copy overhead for data transfer. Fine for tool execution. |
| **Complexity** | High — WIT schema, wasmtime embedding, WASI configuration. Significant dependency (`wasmtime` is large). |
| **Developer UX** | Moderate — need `cargo-component`, can't use most ecosystem crates. Networking only via WASI. |
| **Multi-language** | ✅ Any language that compiles to Wasm Components (Rust, Go, Python, JS, C/C++). |

**Verdict:** Best isolation and multi-language story. But heavy dependency (wasmtime), limited ecosystem access for extensions, and WASI async is still maturing.

---

## Option F: cdylib + shared `dylib` tokio

**Both tau and extensions dynamically link against the same tokio `.so` file. Since it's the same binary, thread-locals are shared. Extensions get full tokio access.**

| Aspect | Detail |
|--------|--------|
| **Async** | ✅ Full — same tokio instance, same reactor, same timers. |
| **ABI** | ❌ Unstable Rust ABI. Must compile tau + extensions + tokio with same rustc version. |
| **Isolation** | ❌ Same process. |
| **Reload** | ❌ Extremely fragile — shared tokio state makes dlclose nearly impossible. |
| **Performance** | ✅ Native. |
| **Complexity** | Very high — managing shared dylib, ensuring version/compiler alignment. |
| **Developer UX** | Good once working — write normal Rust with tokio. But setup is hell. |

**Verdict:** Fragile. Requires exact compiler version match. Reload is dangerous. Not viable for distribution.

---

## Comparison Matrix

| | IPC (A) | C ABI sync (B) | async-ffi (C) | Host API (D) | WASM (E) | Shared dylib (F) |
|---|:---:|:---:|:---:|:---:|:---:|:---:|
| **Full async** | ✅ | ❌ | ⚠️ fake | ⚠️ limited | ⚠️ maturing | ✅ |
| **ABI stable** | ✅ | ✅ | ✅ | ✅ | ✅ | ❌ |
| **Isolation** | ✅ | ❌ | ❌ | ❌ | ✅ | ❌ |
| **Safe reload** | ✅ | ⚠️ | ⚠️ | ⚠️ | ✅ | ❌ |
| **Performance** | ⚠️ ok | ✅ | ✅ | ✅ | ⚠️ ok | ✅ |
| **Ecosystem crates** | ✅ | ✅ | ❌ | ❌ | ❌ | ✅ |
| **Multi-language** | ✅ | ✅ | ❌ | ❌ | ✅ | ❌ |
| **Complexity** | Medium | Medium | High | High | High | Very High |
| **Dev UX** | Good | Poor | Poor | Moderate | Moderate | Good* |

## Recommendation

**Option A (IPC)** is the strongest overall. It's the only approach that gives extensions:
- Unrestricted async (own tokio runtime)
- Full ecosystem access (reqwest, any crate)
- Safe isolation (crash safety)
- Clean reload (kill + respawn)
- Multi-language potential (any language that speaks JSON lines)

The IPC overhead (~100-200µs per message) is irrelevant for tool execution (tools take 100ms-10s+) and event notification (fire-and-forget). The `tau-ext` crate hides all plumbing — extension authors write normal `#[tokio::main]` async Rust.

**Option E (WASM)** is the second choice if isolation/sandboxing becomes critical (e.g., running untrusted extensions). Consider it for v2.

**Options B-D and F** are not recommended due to the async limitations or ABI fragility.

---

---

## Appendix: Why "just use smol" Doesn't Work

smol uses `async-io`, which has a global reactor singleton:

```rust
// async-io/src/reactor.rs
static REACTOR: OnceLock<Reactor> = OnceLock::new();
```

When an extension is compiled as a `cdylib`, it gets its **own copy** of this static. tau has another copy. Two separate epoll/kqueue instances, two separate timer heaps. The extension's reactor is never polled by anyone.

This is not a smol vs tokio issue. **Every Rust async runtime** uses process-global or thread-local statics for its reactor. A `cdylib` gets its own copy of all statics because Rust has no stable ABI and `cdylib` bundles everything.

The only way to share a reactor is `crate-type = "dylib"` (Rust dynamic linking), which requires the same compiler version for host and plugin — making it unsuitable for distribution.

**Conclusion:** The async-across-dylib problem is inherent to Rust's compilation model. No choice of runtime avoids it. IPC (Option A) is the clean escape hatch.

---

## Deferred

This investigation is captured for future reference. Extensions are not included in the current PRD scope. When we add them, start with Option A (IPC).
