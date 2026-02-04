# AGENTS.md — Reusable Patterns for rtui

## Git Workflow
- **Push frequently.** After completing every 1–2 user stories (or any review task), commit and `git push`.
- Don't let work accumulate locally — if tests pass and the task is done, push it.
- Use descriptive commit messages: `feat:`, `fix:`, `chore:`, `docs:` prefixes.

## Trait Object Downcasting
- Terminal trait has `as_any(&self) -> &dyn Any` and `as_any_mut(&mut self) -> &mut dyn Any`
- In tests, downcast via: `tui.terminal.as_any().downcast_ref::<MockTerminal>().unwrap()`
- Standard Rust pattern — use this whenever trait objects need test inspection

## Testing with MockTerminal
- `MockTerminal::new(cols, rows)` — captures all writes to `pub writes: Vec<String>`
- `mock.output()` — concatenates all writes for full output inspection
- `mock.writes.len()` — verify number of write calls (1 per changed frame, 0 if no changes)
- Access via TUI: use `as_any()` downcasting from private `terminal` field (tests in same module can access)
- `mock.set_size()` can simulate resize between renders (triggers full redraw)
- To change content between renders: `tui.root().clear()` + `tui.root().add_child(new_component)`

## TUI Rendering Model
- Scrolling model (not alternate screen) — content grows downward
- Each line gets `\x1b[0m\r\n` suffix — reset prevents style bleeding, \r\n positions cursor on next line
- Buffer wrapped in synchronized output: `\x1b[?2026h` ... `\x1b[?2026l`
- Single `write()` + `flush()` per render frame — never multiple writes
- `previous_lines` and `previous_width` stored for differential rendering

## Differential Rendering (US-007)
- Three render paths: first render (previous_width==0), width changed, differential
- `previous_width == 0` used as "never rendered" sentinel — not a separate flag
- `hardware_cursor_row` tracks actual cursor position (may differ from `cursor_row`/`lines.len()`)
- Cursor movement: `\x1b[nA` (up), `\x1b[nB` (down), only emitted when n > 0
- Line clear: `\x1b[2K` before content, clears entire line regardless of cursor column
- Content shrunk: unified loop from first_changed..=last_changed clears old lines naturally
- No output written when nothing changed — `buffer.is_empty()` check before write
- After differential render, cursor may NOT be at content end — `hardware_cursor_row` tracks actual position

## Component Pattern
- `Component` trait: `render(width) -> Vec<String>` is the only required method
- Components are `Box<dyn Component>` in containers — object-safe
- Components don't know about user event type `E` — only TUI is generic over it

## Tokio Channels
- `unbounded_channel()` can be created without runtime; only `recv()` needs async
- `event_rx` is `Option<>` so the run loop can `.take()` it

## Async Event Loop Testing
- `crossterm_event_tx()` returns a sender for injecting terminal events in tests
- Run loop spawns crossterm EventStream reader; in tests it just blocks on stdin (harmless)
- `#[tokio::test]` for async run() tests; send events via spawned tasks, handler calls `quit()` to exit
- Use `Arc<Mutex<Vec<_>>>` for components that need to share state with test assertions

## Container child access
- `Container::child_mut(index)` returns `Option<&mut Box<dyn Component>>` — NOT `&mut dyn Component` (lifetime issues with 'static trait objects)
- Auto-deref through Box means you can call Component methods directly on the result

## Overlay System (US-014)
- Overlays composite onto base content between rendering and differential comparison — differential rendering then handles changes naturally
- `Rc<Cell<bool>>` for shared overlay visibility (TUI is !Send, single-threaded)
- `show_overlay()` saves focus, `hide_overlay()` pops and restores — nested overlays correctly unwind
- Key forwarding: overlays checked first via `iter_mut().rev()`, falls through to focused component only if no visible overlays
- `splice_overlay_into_line()` uses ANSI resets as boundaries: `before + \x1b[0m + overlay + \x1b[0m + sgr_state + after`
- `slice_from_column()` in utils.rs returns `(sgr_prefix, remaining)` — tracks active SGR state while skipping columns
- `truncate_to_width(s, col, "")` is the "slice before column" operation
- SGR utilities (`is_sgr`, `update_sgr_state`, `sgr_prefix`) are `pub(crate)` in utils.rs

## tau-rt Shared Runtime (US-RT-001)
- cdylib-only crate: `crate-type = ["cdylib"]`. `cargo test -p tau-rt` still works (test harness compiles differently from library target)
- Global singletons via `OnceLock<Reactor>` and `OnceLock<Executor>` — initialized lazily on first access
- Use `async_task::spawn` (not `spawn_local`) for the executor — `spawn_local` panics when Runnables are run/dropped on a different thread, which happens in parallel test execution
- Executor tests need a `Mutex<()>` serialization guard + queue drain at test start, because the global ConcurrentQueue is shared across test threads
- IO sources lazily registered with OS poller: `io_register` only adds to slab, `io_poll_readable/writable` calls `Poller::add` on first interest. This avoids `Event::none` registration issues on some platforms
- Timers use dual data structure: `BTreeMap<(Instant, u64), Waker>` for ordered expiry scan + `HashMap<u64, Instant>` for O(1) cancel/poll by handle
- `react()` collects events into Vec<(usize, bool, bool)> before locking sources — avoids holding events and sources locks simultaneously
- `FfiContext<'_>` has a `with_context()` inherent method (NOT `ContextExt` trait) to convert back to `std::task::Context` and extract the Waker
- All 11 C ABI exports visible via `nm -gU libtau_rt.dylib | grep tau_rt`
