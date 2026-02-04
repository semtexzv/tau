# PRD: tau — Extensible LLM Agent in Rust

## Introduction

**tau** is a Rust monorepo for building extensible LLM-powered coding agents, inspired by [pi-mono](https://github.com/badlogic/pi-mono). It follows the same layered architecture:

| pi-mono package | tau crate | Purpose |
|---|---|---|
| `@mariozechner/pi-tui` | `tau-tui` | Terminal UI with differential rendering |
| `@mariozechner/pi-ai` | `tau-ai` | LLM abstraction: types, streaming, providers |
| `@mariozechner/pi-agent-core` | `tau-agent` | Agent loop: tool execution, conversation, events |
| `@mariozechner/pi-coding-agent` | `tau-cli` | Coding agent: tools, system prompt, interactive TUI |

The project starts with Anthropic as the sole LLM provider (Messages API with SSE streaming), but the provider trait is designed for extensibility.

```
~/tau/
  Cargo.toml            (workspace)
  crates/
    tau-tui/            (terminal UI library)
    tau-ai/             (LLM abstraction + Anthropic provider)
    tau-agent/          (agent loop + tool framework)
    tau-cli/            (coding agent binary)
```

## Goals

### tau-tui
- Provide a `Component` trait where `render(width) → Vec<String>` is the only required method
- Differential rendering: compare previous vs. new lines, emit only changed lines via ANSI cursor movement
- Correct visible-width calculation for Unicode (wide chars, emoji, grapheme clusters) and ANSI escape codes
- Focus management: one component receives input at a time
- Overlay system: modal components composited on top of base content
- Ship basic components: Text (word-wrap), Box (padding/bg), Spacer, Input (single-line), SelectList
- Async event loop with tau-rt — manual poll loop on terminal events + user event channel
- User event channel: `TUI<E>` provides `UnboundedSender<E>` for applications to push custom events from spawned tasks

### tau-ai
- Unified message types: `UserMessage`, `AssistantMessage`, `ToolResultMessage` with text, thinking, image, and tool-call content blocks
- `Provider` trait: `fn stream(model, context, options) → Stream<StreamEvent>` — async at the IO boundary only
- Anthropic Messages API: async HTTP POST via `tau-http`, SSE parsing, posts `StreamEvent`s to a channel
- Tool definitions with JSON Schema parameters (via `schemars`)
- Usage tracking (tokens, cost)

### tau-agent
- **Sync core:** `Agent` is a state machine. All state transitions happen synchronously when events arrive. No async in the agent logic itself.
- `AgentTool` trait: sync definition + async `execute()` (runs in spawned tasks, posts results back)
- Event-driven loop: async IO (HTTP streaming, tool execution) happens in spawned tasks → posts `AgentEvent`s to TUI's `event_tx` channel → TUI handler calls sync `agent.handle_event()` → state transitions + render
- `AgentEvent` enum for UI to observe lifecycle (turn start/end, tool execution, streaming deltas)
- Steering: interrupt the agent mid-run with new user messages

### tau-cli
- Coding tools: read, write, edit, bash
- System prompt for coding tasks
- Interactive TUI mode tying everything together

### Architecture: Sync Core + Async IO Edges

```
┌─────────────────────────────────────────────────┐
│  TUI async select loop                          │
│  ┌───────────────┐   ┌───────────────────────┐  │
│  │ crossterm      │   │ event_rx (channel)    │  │
│  │ key/resize     │   │ AgentEvents from      │  │
│  │                │   │ spawned tasks         │  │
│  └───────┬───────┘   └───────────┬───────────┘  │
│          │                       │               │
│          └───────┬───────────────┘               │
│                  ▼                               │
│  ┌─────────────────────────────┐                 │
│  │  SYNC handler               │                 │
│  │  agent.handle_event(e)      │  ← pure state   │
│  │  update components          │    transitions   │
│  │  tui.render()               │                 │
│  └──────────┬──────────────────┘                 │
│             │ returns actions                    │
│             ▼                                    │
│  ┌─────────────────────────────┐                 │
│  │  tau_iface::spawn tasks     │  ← IO edges     │
│  │  • HTTP stream to Anthropic │    only          │
│  │  • bash process execution   │                 │
│  │  • file read/write          │                 │
│  │  posts results → event_tx   │                 │
│  └─────────────────────────────┘                 │
└─────────────────────────────────────────────────┘
```

The `Agent` struct never awaits. It receives events, updates state, and returns `Vec<AgentAction>` describing what async work to spawn next. The caller (TUI handler) spawns the tasks.

### General
- Minimal dependencies: `crossterm`, `unicode-width`, `unicode-segmentation`, `tau-rt`/`tau-iface` (custom runtime), `tau-http`, `serde`, `schemars`
- **No tokio, no reqwest, no smol, no async-io** — all async goes through `tau-rt` shared library (see Phase 2b)

## User Stories

### US-001: Project scaffolding and dependencies [x]

**Description:** As a developer, I need the Rust project structure so I can start building the library.

**Acceptance Criteria:**
- [x] `cargo init --lib` at `~/rtui`
- [x] `Cargo.toml` with dependencies:
  - `crossterm = { version = "0.28", features = ["event-stream"] }` (event-stream enables async `EventStream`)
  - `unicode-width = "0.2"`
  - `unicode-segmentation = "1.11"`
  - `tokio = { version = "1", features = ["rt", "macros", "sync"] }`
  - `futures = "0.3"` (for `StreamExt` on crossterm's `EventStream`)
- [x] Dev-dependencies: `image = "0.25"` (GIF decoding for load test example)
- [x] Module structure in `src/`: `lib.rs`, `terminal.rs`, `utils.rs`, `component.rs`, `tui.rs`, `components/mod.rs`
- [x] Each module file exists with a placeholder comment
- [x] `cargo check` passes

### US-002: Terminal trait and CrosstermTerminal [x]

**Description:** As a developer, I need a terminal abstraction so the rendering engine is decoupled from the actual terminal.

**Acceptance Criteria:**
- [x] `Terminal` trait in `src/terminal.rs` with methods:
  - `fn start(&mut self)` — enable raw mode, hide cursor
  - `fn stop(&mut self)` — disable raw mode, show cursor, move cursor past content
  - `fn write(&mut self, data: &str)` — write to stdout
  - `fn flush(&mut self)` — flush stdout
  - `fn size(&self) -> (u16, u16)` — returns `(cols, rows)`
  - `fn hide_cursor(&mut self)` / `fn show_cursor(&mut self)`
- [x] Note: event reading is NOT on the trait — crossterm's `EventStream` is used directly by the TUI run loop, and `MockTerminal` uses a channel. This keeps the trait sync and simple.
- [x] `CrosstermTerminal` struct implementing `Terminal` using `crossterm`
  - `start()`: enables raw mode, hides cursor
  - `stop()`: shows cursor, disables raw mode
  - `write()`: writes to stdout buffer with `io::Write`
  - `flush()`: flushes stdout
  - `size()`: delegates to `crossterm::terminal::size()`
- [x] `MockTerminal` struct implementing `Terminal` that captures writes to a `Vec<String>` for testing
- [x] Tests: both terminals can be constructed, MockTerminal captures writes
- [x] `cargo check` passes

### US-003: ANSI escape code utilities [x]

**Description:** As a developer, I need utilities to parse and strip ANSI escape codes so I can correctly measure visible text width.

**Acceptance Criteria:**
- [x] `src/utils.rs` with functions:
  - `strip_ansi(s: &str) -> String` — remove all ANSI escape sequences (CSI `\x1b[...m/G/K/H/J`, OSC `\x1b]...\x07`, APC `\x1b_...\x07`)
  - `extract_ansi_code(s: &str, pos: usize) -> Option<(String, usize)>` — extract escape code at position, return code and byte length consumed
- [x] Tests:
  - `strip_ansi` on plain text returns text unchanged
  - `strip_ansi` on `"\x1b[31mhello\x1b[0m"` returns `"hello"`
  - `strip_ansi` strips OSC hyperlinks (`\x1b]8;;url\x07text\x1b]8;;\x07`)
  - `extract_ansi_code` returns `None` for non-escape positions
  - `extract_ansi_code` returns SGR code and length for `\x1b[31m`
- [x] `cargo test` passes
- [x] `cargo check` passes

### US-004: Visible width calculation [x]

**Description:** As a developer, I need `visible_width()` to measure how many terminal columns a string occupies, ignoring ANSI codes and correctly handling wide/emoji characters.

**Acceptance Criteria:**
- [x] `visible_width(s: &str) -> usize` in `src/utils.rs`
  - Strips ANSI codes first
  - Uses `unicode_width::UnicodeWidthStr` for base measurement
  - Handles tabs as 3 spaces (matching pi-mono)
- [x] `truncate_to_width(s: &str, max_width: usize, ellipsis: &str) -> String`
  - Truncates to `max_width` visible columns, appending ellipsis if truncated
  - Preserves ANSI codes (they don't count toward width)
  - Grapheme-cluster-aware: doesn't split multi-byte characters
- [x] Tests:
  - ASCII: `visible_width("hello")` == 5
  - ANSI: `visible_width("\x1b[31mhello\x1b[0m")` == 5
  - Wide chars: `visible_width("你好")` == 4
  - Tabs: `visible_width("\t")` == 3
  - Truncation: `truncate_to_width("hello world", 8, "...")` == `"hello..."`
  - Truncation preserves ANSI: input with colors → output still has colors up to truncation point
- [x] `cargo test` passes
- [x] `cargo check` passes

### US-005: Component trait and Container [x]

**Description:** As a developer, I need the core `Component` trait and a `Container` that holds children, so I can compose UI elements.

**Acceptance Criteria:**
- [x] `Component` trait in `src/component.rs`:
  ```rust
  pub trait Component {
      fn render(&self, width: u16) -> Vec<String>;
      fn handle_input(&mut self, _event: &crossterm::event::KeyEvent) {}
      fn invalidate(&mut self) {}
  }
  ```
- [x] `Container` struct implementing `Component`:
  - Holds `Vec<Box<dyn Component>>`
  - `add_child()`, `remove_child(index)`, `clear()`
  - `render()` concatenates all children's rendered lines
  - `invalidate()` propagates to all children
- [x] Tests:
  - Empty container renders to empty `Vec`
  - Container with mock components concatenates their output
- [x] `cargo test` passes
- [x] `cargo check` passes

### US-006: TUI engine with full rendering [x]

**Description:** As a developer, I need the TUI engine that renders a component tree to the terminal, initially with full redraws (differential rendering comes next).

**Acceptance Criteria:**
- [x] `TUI<E>` struct in `src/tui.rs`, generic over user event type `E: Send + 'static`
  - Wraps a `Box<dyn Terminal>` and a root `Container`
  - `new(terminal) -> Self`
  - `root(&mut self) -> &mut Container` — access to root container
  - `render(&mut self)` — renders root container, writes all lines to terminal
  - `start(&mut self)` — calls `terminal.start()`
  - `stop(&mut self)` — moves cursor past content, calls `terminal.stop()`
  - `event_tx(&self) -> UnboundedSender<E>` — returns cloneable sender for user events
- [x] Rendering builds a **single `String` buffer** with all output (cursor moves, line clears, content), then calls `terminal.write(&buffer)` + `terminal.flush()` **once** — never multiple writes per frame
- [x] Buffer is wrapped in synchronized output (`\x1b[?2026h` at start, `\x1b[?2026l` at end)
- [x] Each line gets `\x1b[0m` appended (reset, prevents style bleeding)
- [x] Stores `previous_lines: Vec<String>` and `previous_width: u16` for next story's diffing
- [x] Tests: TUI with MockTerminal renders expected output
- [x] `cargo test` passes
- [x] `cargo check` passes

### US-007: Differential rendering [x]

**Description:** As a developer, I need the TUI to only redraw changed lines so terminal output is efficient and flicker-free.

**Acceptance Criteria:**
- [x] `TUI::render()` compares new lines vs `previous_lines` and builds a **single `String` buffer**:
  - If width changed: full re-render (clear screen `\x1b[3J\x1b[2J\x1b[H` + write all lines)
  - If first render (previous empty): write all lines without clearing
  - Otherwise: find `first_changed` and `last_changed` indices, append cursor movement to `first_changed` (`\x1b[{n}A` / `\x1b[{n}B`), append `\x1b[2K` + new content for each changed line — all into the same buffer
  - Wrap entire buffer in `\x1b[?2026h` ... `\x1b[?2026l`
  - **One** `terminal.write(&buffer)` + `terminal.flush()` call at the end
- [x] Tracks `cursor_row` (logical end of content) and `hardware_cursor_row` (actual terminal cursor position) for correct cursor movement math
- [x] If content shrunk: appends line-clear sequences (`\x1b[2K\r\n`) for extra lines, then cursor-up to return
- [x] Tests with MockTerminal:
  - No changes → no output written
  - Single line changed → only that line rewritten (verify buffer contains exactly one `\x1b[2K` + content)
  - Width change → full redraw (buffer contains clear-screen)
  - Content grew → appends new lines
  - Content shrunk → clears old lines
- [x] `cargo test` passes
- [x] `cargo check` passes

### US-008: Async event loop with user events [x]

**Description:** As a developer, I need an async event loop that handles terminal input, resize, AND user-defined events from spawned tasks.

**Acceptance Criteria:**
- [x] `Event<E>` enum in `src/tui.rs`:
  ```rust
  pub enum Event<E> {
      Key(crossterm::event::KeyEvent),
      Resize(u16, u16),
      User(E),
  }
  ```
- [x] `TUI<E>` has an `tokio::sync::mpsc::unbounded_channel` internally:
  - `event_tx()` returns `UnboundedSender<E>` — users clone this and send from `tokio::spawn`ed tasks
  - The receiver is consumed by the run loop
- [x] `TUI::run<F>(&mut self, handler: F)` where `F: FnMut(Event<E>, &mut TUI<E>)`:
  - Calls `self.start()`
  - Uses `crossterm::event::EventStream` (from `event-stream` feature) for async terminal events
  - `tokio::select!` on:
    - `crossterm_stream.next()` → maps to `Event::Key` / `Event::Resize`
    - `user_rx.recv()` → maps to `Event::User(e)`
  - Calls `handler(event, self)` for each event
  - Calls `self.render()` after each handler invocation
  - Breaks when `self.should_quit` is true
  - Calls `self.stop()` on exit
- [x] `TUI::quit(&mut self)` — sets `should_quit = true`
- [x] Focus management: `set_focus()` and `handle_key()` forward to focused component (from previous US-008, now integrated here)
  - `set_focus(component)` — tracks focused component
  - When a `Key` event arrives and there's a focused component, forwards via `handle_input()`
- [x] Tests:
  - User event arrives via channel → handler receives `Event::User(...)`
  - Key event → handler receives `Event::Key(...)`
  - `quit()` breaks the loop
- [x] `cargo test` passes
- [x] `cargo check` passes

### US-007a: Fix cursor repositioning on TUI stop [x]

**Description:** After differential rendering, `hardware_cursor_row` may be in the middle of content (not at the end). `TUI::stop()` must move the cursor past all content before restoring the terminal, otherwise the shell prompt appears mid-output.

**Acceptance Criteria:**
- [x] `TUI::stop()` emits `\x1b[{n}B` to move cursor from `hardware_cursor_row` to `cursor_row` (end of content) before calling `terminal.stop()`, but only when `hardware_cursor_row < cursor_row`
- [x] Test: render 5 lines, differential-update line 1 only (hardware_cursor_row=2, cursor_row=5), call stop(), verify output contains cursor-down to row 5
- [x] Test: render where hardware_cursor_row == cursor_row (e.g. first render or full redraw) — stop() emits no cursor movement
- [x] `cargo test -p tau-tui` passes
- [x] `cargo check` passes

### US-REVIEW-PHASE1: Review foundation (US-001 through US-008) [x]

**Description:** Review the foundation layer as a cohesive system.

**Acceptance Criteria:**
- [x] Identify phase scope: US-001 to US-008
- [x] Review all phase code files together
- [x] Evaluate quality:
  - Good taste: Simple and elegant across all tasks?
  - No special cases: Edge cases handled through design?
  - Data structures: Consistent and appropriate?
  - Complexity: Can anything be simplified?
  - Duplication: Any repeated logic between tasks?
  - Integration: Do components work together cleanly?
- [x] Cross-task analysis:
  - Verify `Component` trait is ergonomic (not too many required methods)
  - Verify `Terminal` trait is minimal but sufficient (and that event reading being outside the trait works)
  - Verify differential rendering math is correct
  - Verify `visible_width` is used consistently wherever line widths matter
  - Verify async event loop handles edge cases (channel closed, stream ended)
  - Verify focus management integrates cleanly with the event loop
  - Verify `TUI<E>` generic is not overly constraining
- [x] If issues found:
  - Insert fix tasks after the failing task (US-XXXa, US-XXXb, etc.)
  - Append review findings to progress.txt
  - Do NOT mark this review task [x]
- [x] If no issues:
  - Append "## Phase 1 review PASSED" to progress.txt
  - Mark this review task [x]
  - Commit: "docs: phase 1 review complete"

### US-009: Spacer component [x]

**Description:** As a developer, I want a Spacer component that renders N empty lines for vertical spacing.

**Acceptance Criteria:**
- [x] `Spacer` struct in `src/components/spacer.rs` implementing `Component`
- [x] Constructor takes `lines: usize` (default 1)
- [x] `render()` returns `lines` empty strings
- [x] `set_lines(n)` to update count
- [x] Tests: `Spacer::new(3).render(80)` returns 3 empty strings
- [x] `cargo test` passes
- [x] `cargo check` passes

### US-010: Text component with word wrapping [x]

**Description:** As a developer, I want a Text component that word-wraps content and preserves ANSI styles across line breaks.

**Acceptance Criteria:**
- [x] `Text` struct in `src/components/text.rs` implementing `Component`
- [x] Constructor: `new(text, padding_x, padding_y)`
- [x] `set_text(text)` to update content (invalidates cache)
- [x] `render(width)`:
  - Wraps text at `width - 2*padding_x` columns using word boundaries
  - Preserves ANSI codes across line breaks (tracks active SGR state, re-emits at start of continuation lines)
  - Adds `padding_y` empty lines above and below
  - Adds `padding_x` spaces on left, pads right to full width
- [x] `wrap_text_with_ansi(text: &str, width: usize) -> Vec<String>` utility in `utils.rs`:
  - Splits on word boundaries
  - Breaks words longer than width character-by-character (grapheme-aware)
  - Tracks ANSI SGR state, re-applies at start of each wrapped line
- [x] Caches rendered output — returns cached result if text and width unchanged
- [x] Tests:
  - Short text (fits in one line): no wrapping
  - Long text: wraps at word boundary
  - ANSI styled text: style preserved across wrap
  - Empty text: returns empty vec
- [x] `cargo test` passes
- [x] `cargo check` passes

### US-011: Box component [x]

**Description:** As a developer, I want a Box component that wraps children with padding and optional background color.

**Acceptance Criteria:**
- [x] `BoxComponent` struct in `src/components/box_component.rs` implementing `Component`
- [x] Holds `Vec<Box<dyn Component>>` children
- [x] Constructor: `new(padding_x, padding_y)`
- [x] `set_bg(ansi_code: &str)` — sets background color as raw ANSI code (e.g., `"\x1b[48;5;236m"`)
- [x] `add_child()`, `remove_child(index)`, `clear()`
- [x] `render(width)`:
  - Renders children at `width - 2*padding_x`
  - Prepends `padding_x` spaces to each child line
  - Pads each line to full `width`
  - Applies background color to entire padded line if set
  - Adds `padding_y` empty (background-filled) lines above and below
- [x] Tests:
  - Box with one Text child renders with correct padding
  - Box with background applies bg to all lines including padding
  - Empty box renders nothing
- [x] `cargo test` passes
- [x] `cargo check` passes

### US-012: Input component [x]

**Description:** As a user, I want a single-line text input with cursor, horizontal scrolling, and basic editing keybindings.

**Acceptance Criteria:**
- [x] `Input` struct in `src/components/input.rs` implementing `Component`
- [x] Displays `"> "` prompt followed by text with inverse-video cursor
- [x] Cursor movement: Left, Right, Home, End, Ctrl+Left (word back), Ctrl+Right (word forward)
- [x] Editing: printable char insertion, Backspace, Delete, Ctrl+Backspace (delete word), Ctrl+U (delete to start), Ctrl+K (delete to end)
- [x] Horizontal scrolling when text exceeds available width
- [x] Callbacks: `on_submit: Option<Box<dyn FnMut(&str)>>` (Enter), `on_escape: Option<Box<dyn FnMut()>>` (Escape)
- [x] `value()` → `&str`, `set_value(s)` to get/set content
- [x] `focused: bool` field — renders cursor only when focused
- [x] Tests:
  - Initial render shows `"> "` with cursor
  - After typing "abc", value is "abc" and render shows it
  - Backspace removes last char
  - Left/Right moves cursor, render shows cursor at correct position
- [x] `cargo test` passes
- [x] `cargo check` passes

### US-013: SelectList component [x]

**Description:** As a user, I want a selectable list with arrow-key navigation so I can pick from options.

**Acceptance Criteria:**
- [x] `SelectList` struct in `src/components/select_list.rs` implementing `Component`
- [x] `SelectItem { value: String, label: String, description: Option<String> }`
- [x] Constructor: `new(items, max_visible)`
- [x] Renders visible window of items, selected item has `→` prefix and distinct styling (bold/inverse)
- [x] Arrow Up/Down changes selection (wraps around)
- [x] Enter triggers `on_select` callback, Escape triggers `on_cancel`
- [x] Scrolls when selection moves outside visible window
- [x] Shows scroll indicator `(N/M)` when list is scrollable
- [x] `set_filter(query)` — filters items by prefix match
- [x] `selected_item() -> Option<&SelectItem>`
- [x] Tests:
  - Renders correct number of visible items
  - Selection moves with Up/Down
  - Wraps from top to bottom and vice versa
  - Filter narrows visible items
- [x] `cargo test` passes
- [x] `cargo check` passes

### US-014: Overlay system [x]

**Description:** As a developer, I need an overlay system to render modal components (like SelectList popups) on top of base content.

**Acceptance Criteria:**
- [x] `TUI` gets overlay methods:
  - `show_overlay(component, options) -> OverlayHandle`
  - `hide_overlay()` — hides topmost overlay
  - `has_overlay() -> bool`
- [x] `OverlayOptions` struct: `width`, `max_height`, `anchor` (Center, TopLeft, BottomLeft, etc.), `offset_x`, `offset_y`
- [x] `OverlayHandle` with `hide()` and `set_hidden(bool)`
- [x] Overlay compositing in `render()`:
  - Renders base content first
  - For each visible overlay: renders at its configured width, composites onto base lines at calculated row/col position
  - Compositing: splice overlay content into base line at column offset (slice before + overlay + slice after)
- [x] Focus saves/restores: showing overlay saves current focus, hiding restores it
- [x] Overlay stack: multiple overlays, topmost gets input
- [x] Tests:
  - Single overlay composited at correct position
  - Overlay hide restores focus
  - Overlay stack: topmost gets input
- [x] `cargo test` passes
- [x] `cargo check` passes

### US-012a: Fix Input horizontal scrolling and padding for wide characters [x]

**Description:** Input's `compute_scroll()` and render padding calculation both conflate char indices with column widths. With wide characters (CJK, emoji), this causes rendered lines to exceed terminal width (triggering terminal line wrapping) or be shorter than expected.

**Acceptance Criteria:**
- [x] `compute_scroll()` works in column widths, not char indices. The `available` parameter is already in columns; `cursor` and `offset` must also be tracked/converted to column offsets before comparison.
- [x] Render padding calculation uses actual visible column widths, not `cursor_in_view + 1` (which counts chars, not columns).
- [x] Tests with wide characters:
  - `Input` with "你好世界" (8 cols) at width 12: rendered line is exactly 12 visible columns
  - `Input` with long CJK string: horizontal scrolling works, visible content doesn't exceed available columns
  - `Input` with cursor at end of wide-char text: padding is correct
  - `Input` with cursor in middle of wide-char text: cursor renders at correct column position
- [x] All existing Input tests still pass
- [x] `cargo test -p tau-tui` passes
- [x] `cargo check` passes

### US-REVIEW-PHASE2: Review components and overlays (US-009 through US-014) [x]

**Description:** Review all components and the overlay system as a cohesive layer.

**Acceptance Criteria:**
- [x] Identify phase scope: US-009 to US-014
- [x] Review all component files together
- [x] Evaluate quality:
  - Consistent API patterns across components
  - Component trait is not fighting Rust's ownership model
  - Overlay compositing handles edge cases (wide chars at boundaries, ANSI codes)
  - visible_width used correctly everywhere
- [x] Cross-task analysis:
  - Verify all components pad output to full width (no rendering artifacts)
  - Verify Input cursor math is correct with Unicode
  - Verify overlay focus save/restore works with nested overlays
  - Check SelectList + overlay integration works (common pattern: popup select list)
  - Verify `TUI<E>` generic doesn't leak into Component trait (components shouldn't care about E)
- [x] If issues found:
  - Insert fix tasks
  - Append findings to progress.txt
- [x] If no issues:
  - Append "## Phase 2 review PASSED" to progress.txt
  - Mark this review task [x]

### US-015: Example application with async user events [x]

**Description:** As a developer, I want a working example app that demonstrates all components and async user events so I can verify everything works together.

**Acceptance Criteria:**
- [x] `examples/demo.rs` with a runnable app using `tau_iface::block_on`
  (initially may use `#[tokio::main]` — migrated to tau-rt in US-RT-006)
- [x] Shows: Text with styled content, Box with background, Input that echoes typed text
- [x] Demonstrates user events: spawns a task (via `tau_iface::spawn` after migration) that sends a timer event every second via `event_tx`, updating a counter in the UI
- [x] SelectList overlay triggered by a key (e.g., Ctrl+P)
- [x] Quit with Ctrl+C or Escape (when no overlay)
- [x] Demonstrates focus switching between Input and SelectList
- [x] `cargo run --example demo` works
- [x] `cargo check` passes

### US-016: GIF-to-ANSI load test [x]

**Description:** As a developer, I want a load test that plays a DOOM GIF as ANSI-colored block art through the TUI, measuring rendering performance to verify the differential rendering engine is fast enough for real-world use.

**Acceptance Criteria:**
- [x] `examples/loadtest.rs` using `tau_iface::block_on`
  (initially may use `#[tokio::main]` — migrated to tau-rt in US-RT-006)
- [x] Add dev-dependencies: `image = "0.25"` (GIF decoding + frame extraction)
- [x] Accepts a GIF file path as CLI argument: `cargo run --example loadtest -- doom.gif`
- [x] GIF frame → ANSI conversion:
  - Decode each GIF frame into RGB pixels
  - Scale frame to fit terminal dimensions (maintain aspect ratio, account for ~2:1 cell height:width ratio)
  - Convert each pixel pair (top + bottom) to a `▀` (upper half block) character with truecolor ANSI: `\x1b[38;2;R;G;Bm\x1b[48;2;R;G;Bm▀` — packs 2 vertical pixels per cell
  - Each frame becomes a `Vec<String>` of these colored lines
- [x] Playback loop using `event_tx` channel:
  - Spawns a task that sends `Frame(usize)` events at the GIF's native frame delay (or 30fps if unspecified)
  - Handler updates a `Text`-like component with the current frame's pre-rendered lines
  - TUI differential rendering picks up the changes
- [x] Performance measurement:
  - Tracks per-frame render time (time from `render()` call start to `flush()` complete)
  - Tracks bytes written per frame to terminal
  - Displays an FPS counter and stats overlay (top-right corner): current FPS, avg frame time, avg bytes/frame
  - On exit (Ctrl+C / Escape), prints summary to stderr: total frames, avg FPS, avg/p95/max frame time, avg bytes/frame
- [x] Pre-renders all frames on startup (conversion shouldn't be part of the render benchmark)
- [x] Quit with Ctrl+C or Escape
- [x] `cargo run --example loadtest -- doom.gif` works and shows smooth playback
- [x] `cargo check` passes

---

## Phase 2b: tau-rt — Shared Async Runtime

> **Design reference:** `TAU-RT-DESIGN.md`
>
> tau-rt is a custom single-threaded async runtime compiled as a **cdylib only**
> (`libtau_rt.so` / `libtau_rt.dylib`). Both the host binary and future extension
> cdylibs link against it dynamically, sharing one reactor and executor instance.
> This eliminates tokio and all foreign runtimes from the project.
>
> **No crate in the workspace may depend on tokio, async-io, smol, async-std, or reqwest.**
> HTTP is built from TCP + TLS + httparse in `tau-http`.

### US-RT-001: tau-rt cdylib with reactor and executor [ ]

**Description:** As a developer, I need the core shared runtime library that owns
the reactor (IO polling + timers) and a single-threaded task executor, exposed
entirely through C ABI.

**Acceptance Criteria:**
- [ ] `crates/tau-rt/Cargo.toml`:
  ```toml
  [lib]
  crate-type = ["cdylib"]  # ONLY cdylib. No rlib.
  
  [dependencies]
  polling = "3"
  async-task = "4"
  async-ffi = "0.5"
  slab = "0.4"
  concurrent-queue = "2"
  ```
- [ ] `crates/tau-rt/src/lib.rs` — re-exports C ABI functions only
- [ ] **Reactor** (`src/reactor.rs`):
  - Global `OnceLock<Reactor>` singleton (inside this .so only)
  - `Reactor` struct: `polling::Poller`, `Mutex<Slab<Arc<Source>>>` for IO sources,
    `Mutex<BTreeMap<(Instant, u64), Waker>>` for timers, `Mutex<Events>` for polling buffer
  - `react(timeout: Option<Duration>)`: process expired timers → poll OS → wake ready tasks
  - IO and timers share the same wake behavior: store waker on register, fire waker on ready
- [ ] **Executor** (`src/executor.rs`):
  - Task queue backed by `ConcurrentQueue<Runnable>` (from `async-task`)
  - `spawn(FfiFuture<()>)`: wraps in `async-task::spawn_local`, pushes `Runnable` to queue
  - `try_tick() -> bool`: pops one `Runnable`, polls it, returns whether work was done
  - `block_on(FfiFuture<()>)`: loop { try_tick(); react(timeout); } until future completes
- [ ] **C ABI exports** (`src/ffi.rs`) — all `#[no_mangle] pub extern "C"`:
  - `tau_rt_io_register(fd: i32) -> u64`
  - `tau_rt_io_deregister(handle: u64)`
  - `tau_rt_io_poll_readable(handle: u64, cx: *mut FfiContext) -> u8` (0=Pending, 1=Ready)
  - `tau_rt_io_poll_writable(handle: u64, cx: *mut FfiContext) -> u8`
  - `tau_rt_timer_create(nanos_from_now: u64) -> u64`
  - `tau_rt_timer_cancel(handle: u64)`
  - `tau_rt_timer_poll(handle: u64, cx: *mut FfiContext) -> u8`
  - `tau_rt_spawn(future: FfiFuture<()>)`
  - `tau_rt_try_tick() -> u8` (0=no work, 1=did work)
  - `tau_rt_react(timeout_ms: u64) -> i32` (0=ok, -1=error)
  - `tau_rt_block_on(future: FfiFuture<()>)`
- [ ] `cargo build -p tau-rt` produces `libtau_rt.dylib` / `libtau_rt.so`
- [ ] No dependency on tokio, async-io, smol, async-std, reqwest

**Design Verification (DV-1):**
- [ ] Unit test inside tau-rt: spawn a future that increments an `AtomicU64`, call `try_tick()`,
  assert counter incremented. Timer test: create timer 50ms, call `react()` in loop, assert
  future completes within 55ms.

### US-RT-002: tau-iface safe wrapper crate [ ]

**Description:** As a developer, I need a pure-declaration crate that provides safe
Rust wrappers around the tau-rt C ABI, so all other crates and extensions use
idiomatic Rust async/await.

**Acceptance Criteria:**
- [ ] `crates/tau-iface/Cargo.toml`:
  ```toml
  [dependencies]
  async-ffi = "0.5"
  
  # NO dependency on tau-rt. Linked at load time via #[link(name = "tau_rt")]
  ```
- [ ] `crates/tau-iface/src/ffi.rs`:
  - `#[link(name = "tau_rt")]` extern "C" block declaring every `tau_rt_*` function
  - Mirrors tau-rt's exports exactly
- [ ] `crates/tau-iface/src/async_fd.rs` — `AsyncFd` struct:
  - `new(fd: RawFd) -> io::Result<Self>` — calls `tau_rt_io_register`
  - `async fn readable(&self) -> io::Result<()>` — polls via `tau_rt_io_poll_readable`
  - `async fn writable(&self) -> io::Result<()>` — polls via `tau_rt_io_poll_writable`
  - `Drop` calls `tau_rt_io_deregister`
- [ ] `crates/tau-iface/src/timer.rs` — `Timer` struct:
  - `after(duration: Duration) -> Self` — calls `tau_rt_timer_create`
  - `impl Future for Timer` — polls via `tau_rt_timer_poll`
  - `Drop` calls `tau_rt_timer_cancel`
- [ ] `crates/tau-iface/src/tcp.rs` — `TcpStream` and `TcpListener`:
  - `TcpStream::connect(addr) -> io::Result<Self>` — non-blocking socket + `AsyncFd`, await writable for connect completion
  - `async fn read(&self, buf) -> io::Result<usize>` — await readable, then `recv()`
  - `async fn write(&self, buf) -> io::Result<usize>` — await writable, then `send()`
  - `TcpListener::bind(addr)` + `async fn accept()`
- [ ] `crates/tau-iface/src/udp.rs` — `UdpSocket`:
  - `bind(addr) -> io::Result<Self>` — non-blocking socket + `AsyncFd`
  - `async fn send_to(&self, buf, addr) -> io::Result<usize>`
  - `async fn recv_from(&self, buf) -> io::Result<(usize, SocketAddr)>`
  - `connect(addr)` + `async fn send()` / `async fn recv()` for connected mode
- [ ] `crates/tau-iface/src/lib.rs` — re-exports + convenience:
  - `pub fn spawn(future)` — wraps in `FfiFuture`, calls `tau_rt_spawn`
  - `pub async fn sleep(duration)` — `Timer::after(duration).await`
  - `pub fn block_on(future)` — wraps in `FfiFuture`, calls `tau_rt_block_on`
  - `pub fn try_tick() -> bool` — calls `tau_rt_try_tick`
  - `pub fn react(timeout) -> io::Result<()>` — calls `tau_rt_react`
- [ ] No statics, no globals, no thread-locals anywhere in this crate
- [ ] `cargo check -p tau-iface` passes (link errors are expected without tau-rt.so present;
  the real test is in US-RT-003)

### US-RT-002b: Vendor async crates with tau-rt backend feature [ ]

**Description:** Vendor `async-io`, `async-executor`, and dependencies as git submodules.
Patch them to add a `tau-rt` feature flag that replaces their internal globals
(reactor singleton, driver thread) with calls through `tau-iface` externs.
Workspace `[patch]` section points to vendored copies. PRs upstream come later.

**Acceptance Criteria:**
- [ ] Git submodules under `vendor/` for the async ecosystem crates and key
  dependencies that touch the runtime: `async-io`, `async-executor`, `polling`,
  `crossterm`, HTTP client crate (e.g. `async-h1`, `isahc`, or `httparse`+`async-net`),
  `rustls`/TLS if needed
- [ ] Each vendored crate gets a `tau-rt` cargo feature. When enabled, internal
  globals (reactor, driver threads, runtime detection) are replaced by
  `tau-iface` calls
- [ ] Workspace `[patch.crates-io]` maps to vendored paths
- [ ] Ecosystem crates built on these (e.g. crossterm `event-stream`, HTTP
  streaming) work transparently with the shared runtime
- [ ] `cargo check --workspace` passes with vendored deps
- [ ] No upstream PRs required at this stage — just local patches

### US-RT-003: Integration test — host and plugin share reactor [ ]

**Description:** Verify that the shared runtime architecture works end-to-end: a host
binary and a plugin cdylib both link `libtau_rt.so` and share the same reactor,
executor, and timer heap.

**Acceptance Criteria:**
- [ ] `tests/rt-integration/` directory with:
  - `host/` — binary crate depending on `tau-iface`
  - `plugin/` — cdylib crate depending on `tau-iface`
  - `run_test.sh` — builds tau-rt, plugin, host; runs host with correct library path
- [ ] **Shared counter test:** host increments via tau-rt, loads plugin, plugin increments,
  host reads final value — counter is consistent (DV-1 from design doc)
- [ ] **Shared reactor IO test (DV-2):** host opens a TCP listener on localhost. Plugin
  connects via `tau_iface::TcpStream`. Host accepts, writes "hello". Plugin reads it.
  All IO goes through the single shared reactor.
- [ ] **Shared timer test (DV-3):** host spawns a task that sleeps 50ms and records timestamp.
  Plugin spawns a task that sleeps 100ms and records timestamp. Host drives reactor.
  Both timers fire at correct times (±10ms tolerance). Timestamps collected through
  a shared `AtomicU64` or C ABI callback.
- [ ] **FfiFuture round-trip (DV-4):** plugin exports `extern "C" fn plugin_task() -> FfiFuture<u64>`
  that does async TCP connect + read + returns byte count. Host spawns it, drives executor,
  receives the result.
- [ ] `run_test.sh` exits 0 on success, non-zero with clear error on failure

### US-RT-004: Dependency enforcement and forbidden-dep check [ ]

**Description:** As a developer, I need build-time enforcement that no crate in the
workspace (and no future extension) accidentally depends on tokio or other
foreign async runtimes.

**Acceptance Criteria:**
- [ ] `ci/check-deps.sh` script:
  ```bash
  FORBIDDEN="tokio async-io smol async-std async-global-executor reqwest hyper"
  ```
  Runs `cargo tree` for each workspace member, fails if any forbidden crate appears
  in the dependency tree
- [ ] Workspace root `Cargo.toml` does NOT list tokio, reqwest, or any forbidden dep
  in `[workspace.dependencies]`
- [ ] `crates/tau-iface/build.rs` — compile-time check:
  - Scans `cargo metadata` (via `CARGO_MANIFEST_DIR`) or `DEP_` env vars
  - `panic!()` with clear message if a forbidden dependency is detected
  - Message: `"FORBIDDEN DEPENDENCY: 'tokio' detected. Use tau-iface for async, not tokio."`
- [ ] **DV-5 test:** create a throwaway crate that depends on `tau-iface` + `tokio`.
  `cargo check` must fail with the forbidden-dependency error. Verify the error message
  is clear and actionable.
- [ ] Document the rule in workspace README: *"No foreign runtimes. Use `tau_iface::spawn`,
  `tau_iface::sleep`, `tau_iface::TcpStream`. See TAU-RT-DESIGN.md."*

### US-RT-005: tau-http — HTTP/1.1 client on tau-rt primitives [ ]

**Description:** As a developer, I need an HTTP/1.1 client built entirely on tau-iface
(TCP + timers) so the AI layer can stream from the Anthropic API without reqwest or tokio.

**Acceptance Criteria:**
- [ ] `crates/tau-http/Cargo.toml`:
  ```toml
  [dependencies]
  tau-iface = { path = "../tau-iface" }
  httparse = "1"
  rustls = { version = "0.23", default-features = false, features = ["ring", "std"] }
  webpki-roots = "0.26"
  ```
- [ ] `crates/tau-http/src/client.rs`:
  - `HttpClient::new() -> Self` (reusable, holds rustls `ClientConfig`)
  - `async fn request(method, url, headers, body) -> Result<HttpResponse>`
  - Opens `tau_iface::TcpStream`, wraps in `rustls::StreamOwned` for HTTPS
  - Sends request manually (method + headers + body)
  - Parses response status + headers via `httparse`
  - Supports `Transfer-Encoding: chunked` (required for SSE)
- [ ] `crates/tau-http/src/sse.rs` — SSE streaming:
  - `HttpResponse::sse_stream() -> impl Stream<Item = SseEvent>`
  - Parses `event:`, `data:`, empty-line-delimited events
  - `SseEvent { event: Option<String>, data: String }`
- [ ] `crates/tau-http/src/lib.rs` — convenience:
  - `pub async fn get(url, headers) -> Result<HttpResponse>`
  - `pub async fn post(url, headers, body) -> Result<HttpResponse>`
- [ ] Timeout support via `tau_iface::Timer` — configurable connect and read timeouts
- [ ] No dependency on tokio, reqwest, hyper, async-io
- [ ] Integration test: `POST` to `https://httpbin.org/post` with JSON body,
  verify 200 response and echoed body (requires network; mark `#[ignore]` for CI)

### US-RT-006: Migrate tau-tui event loop from tokio to tau-rt [ ]

**Description:** As a developer, I need to replace all tokio usage in tau-tui with
tau-iface so the TUI crate uses the shared runtime.

**Acceptance Criteria:**
- [ ] Remove `tokio` and `futures` from `crates/tau-tui/Cargo.toml`
- [ ] Add `tau-iface = { path = "../tau-iface" }` dependency
- [ ] Replace `tokio::sync::mpsc` with `std::sync::mpsc` (sync channel is fine — the
  event loop polls it non-blockingly each tick)
- [ ] Replace `tokio::select!` event loop with manual poll loop:
  ```rust
  // Pseudocode
  loop {
      // Check stdin for key events (via AsyncFd on stdin fd)
      if let Some(key) = poll_stdin()? { handle_key(key); }
      // Check user event channel
      while let Ok(ev) = user_rx.try_recv() { handle_user_event(ev); }
      // Render if dirty
      if dirty { tui.render(); }
      // Drive reactor (IO + timers, with short timeout)
      tau_iface::react(Some(Duration::from_millis(16)))?;
      // Drive executor (poll spawned tasks)
      while tau_iface::try_tick() {}
  }
  ```
- [ ] `event_tx()` returns `std::sync::mpsc::Sender<E>` — still cloneable, still
  usable from `tau_iface::spawn`ed tasks
- [ ] stdin reading via `AsyncFd` wrapping `RawFd` 0 (stdin) + crossterm's
  `crossterm::event::read()` or raw byte parsing
- [ ] All existing tau-tui tests pass: `cargo test -p tau-tui`
  (tests link against `libtau_rt` at test time)
- [ ] US-015 demo and US-016 loadtest updated: replace `#[tokio::main]` with
  `tau_iface::block_on(async { ... })`, replace `tokio::spawn` with `tau_iface::spawn`
- [ ] `cargo check -p tau-tui` passes
- [ ] `ci/check-deps.sh` passes (no tokio in tree)

### US-REVIEW-PHASE2B: Review tau-rt foundation (US-RT-001 through US-RT-006) [ ]

**Description:** Review the shared runtime, interface crate, HTTP client, and TUI
migration as a cohesive layer before building the AI and agent layers on top.

**Acceptance Criteria:**
- [ ] Identify phase scope: US-RT-001 to US-RT-006
- [ ] Review all tau-rt C ABI exports — are they sufficient for the AI/agent layers?
  Will `tau-http` SSE streaming work for Anthropic? Is `AsyncFd` + stdin adequate for TUI?
- [ ] Run integration test from US-RT-003 — all DV checks pass
- [ ] Run `ci/check-deps.sh` — no forbidden dependencies anywhere
- [ ] Verify `cargo test --workspace` passes with `libtau_rt` on library path
- [ ] Check that `libtau_rt.dylib`/`.so` is the only dynamic Rust dependency
  (everything else is statically linked)
- [ ] Evaluate: is the C ABI surface minimal? Can any exports be removed or combined?
- [ ] If issues found:
  - Insert fix tasks (US-RT-XXXa, etc.)
  - Append findings to progress.txt
- [ ] If no issues:
  - Append "## Phase 2b review PASSED" to progress.txt
  - Mark this review task [x]

---

## Phase 3: Workspace + tau-ai

### US-017: Convert to Cargo workspace [ ]

**Description:** As a developer, I need to restructure the project as a Cargo workspace so each layer is a separate crate.

**Acceptance Criteria:**
- [ ] Root `~/tau/Cargo.toml` is a workspace:
  ```toml
  [workspace]
  members = ["crates/*"]
  resolver = "2"
  
  [workspace.dependencies]
  tau-iface = { path = "crates/tau-iface" }
  serde = { version = "1", features = ["derive"] }
  serde_json = "1"
  async-ffi = "0.5"
  ```
- [ ] Workspace contains: `tau-rt`, `tau-iface`, `tau-http`, `tau-tui`, `tau-ai`, `tau-agent`, `tau-cli`
- [ ] Create empty crate stubs: `crates/tau-ai/`, `crates/tau-agent/`, `crates/tau-cli/`
- [ ] All existing TUI tests still pass: `cargo test -p tau-tui`
- [ ] `cargo check --workspace` passes (except tau-rt which builds separately)
- [ ] No tokio, reqwest, or forbidden deps in `[workspace.dependencies]`

### US-018: tau-ai core message types [ ]

**Description:** As a developer, I need the core LLM message types so all crates share a common vocabulary.

**Acceptance Criteria:**
- [ ] `crates/tau-ai/src/types.rs` with:
  ```rust
  // Content blocks
  pub struct TextContent { pub text: String }
  pub struct ThinkingContent { pub thinking: String }
  pub struct ImageContent { pub data: String, pub mime_type: String } // base64
  pub struct ToolCall { pub id: String, pub name: String, pub arguments: serde_json::Value }
  
  // Messages
  pub struct UserMessage { pub content: Vec<UserContent>, pub timestamp: u64 }
  pub struct AssistantMessage {
      pub content: Vec<AssistantContent>,
      pub model: String,
      pub usage: Usage,
      pub stop_reason: StopReason,
      pub error_message: Option<String>,
  }
  pub struct ToolResultMessage {
      pub tool_call_id: String,
      pub tool_name: String,
      pub content: Vec<ResultContent>,
      pub is_error: bool,
  }
  
  pub enum Message { User(UserMessage), Assistant(AssistantMessage), ToolResult(ToolResultMessage) }
  pub enum StopReason { Stop, Length, ToolUse, Error, Aborted }
  pub struct Usage { pub input_tokens: u32, pub output_tokens: u32, pub cache_read: u32, pub cache_write: u32 }
  ```
- [ ] All types derive `Debug, Clone, Serialize, Deserialize`
- [ ] `UserContent` enum: `Text(TextContent) | Image(ImageContent)`
- [ ] `AssistantContent` enum: `Text(TextContent) | Thinking(ThinkingContent) | ToolCall(ToolCall)`
- [ ] `ResultContent` enum: `Text(TextContent) | Image(ImageContent)`
- [ ] Tests: round-trip serde for each message type
- [ ] `cargo test -p tau-ai` passes
- [ ] `cargo check --workspace` passes

### US-019: Tool type with JSON Schema [ ]

**Description:** As a developer, I need a `Tool` type with JSON Schema parameters so LLM providers can describe available tools.

**Acceptance Criteria:**
- [ ] Add `schemars = "0.8"` to tau-ai dependencies
- [ ] `crates/tau-ai/src/tool.rs`:
  ```rust
  pub struct Tool {
      pub name: String,
      pub description: String,
      pub parameters: schemars::schema::RootSchema,
  }
  ```
- [ ] `Context` struct:
  ```rust
  pub struct Context {
      pub system_prompt: Option<String>,
      pub messages: Vec<Message>,
      pub tools: Vec<Tool>,
  }
  ```
- [ ] Helper: `Tool::from_type::<T: JsonSchema>(name, description)` that derives schema from a `schemars::JsonSchema` struct
- [ ] Tests: derive schema from a test struct, verify JSON output matches expected shape
- [ ] `cargo test -p tau-ai` passes
- [ ] `cargo check --workspace` passes

### US-020: StreamEvent types and Provider trait [ ]

**Description:** As a developer, I need the streaming event types and a provider trait so we can plug in different LLM backends.

**Acceptance Criteria:**
- [ ] `crates/tau-ai/src/stream.rs` with `StreamEvent` enum:
  ```rust
  pub enum StreamEvent {
      MessageStart { message: AssistantMessage },
      TextDelta { content_index: usize, delta: String },
      ThinkingDelta { content_index: usize, delta: String },
      ToolCallDelta { content_index: usize, delta: String },
      ContentBlockStart { content_index: usize },
      ContentBlockEnd { content_index: usize },
      MessageDone { message: AssistantMessage },
      Error { error: String },
  }
  ```
- [ ] `StreamOptions` struct: `temperature`, `max_tokens`, `api_key`
- [ ] `Provider` trait:
  ```rust
  pub trait Provider: Send + Sync {
      fn stream(
          &self,
          model: &str,
          context: &Context,
          options: &StreamOptions,
      ) -> Pin<Box<dyn Stream<Item = Result<StreamEvent>> + Send>>;
  }
  ```
- [ ] Re-export everything from `crates/tau-ai/src/lib.rs`
- [ ] Tests: StreamEvent enum is constructible, Provider trait is object-safe
- [ ] `cargo test -p tau-ai` passes
- [ ] `cargo check --workspace` passes

### US-021: Anthropic Messages API streaming [ ]

**Description:** As a developer, I need the Anthropic provider that streams responses via SSE from the Messages API.

**Acceptance Criteria:**
- [ ] Add `tau-http = { path = "../tau-http" }` to tau-ai (NOT reqwest)
- [ ] `crates/tau-ai/src/providers/anthropic.rs` implementing `Provider`:
  - `tau_http::post()` to `https://api.anthropic.com/v1/messages` with `stream: true`
  - Headers: `x-api-key`, `anthropic-version: 2023-06-01`, `content-type: application/json`
  - Convert `Context` → Anthropic request body (messages, system, tools, max_tokens)
  - Anthropic tool parameters: convert `RootSchema` to Anthropic's `input_schema` format
- [ ] SSE parser via `tau_http::HttpResponse::sse_stream()`, parse events:
  - `message_start` → `StreamEvent::MessageStart`
  - `content_block_start` → `StreamEvent::ContentBlockStart`
  - `content_block_delta` with `text_delta` → `StreamEvent::TextDelta`
  - `content_block_delta` with `thinking_delta` → `StreamEvent::ThinkingDelta`
  - `content_block_delta` with `input_json_delta` → `StreamEvent::ToolCallDelta`
  - `content_block_stop` → `StreamEvent::ContentBlockEnd`
  - `message_delta` → update stop_reason + usage
  - `message_stop` → `StreamEvent::MessageDone`
  - `error` → `StreamEvent::Error`
- [ ] Handles Anthropic error responses (non-200 status) with clear error messages
- [ ] Tests:
  - Request body serialization: verify JSON structure matches Anthropic API spec
  - SSE parsing: feed mock SSE lines, verify correct `StreamEvent` sequence
  - Tool schema conversion: verify `RootSchema` maps to valid Anthropic `input_schema`
- [ ] `cargo test -p tau-ai` passes
- [ ] `cargo check --workspace` passes

### US-022: Anthropic provider integration test [ ]

**Description:** As a developer, I want a live integration test to verify the Anthropic provider works end-to-end.

**Acceptance Criteria:**
- [ ] `crates/tau-ai/tests/anthropic_live.rs` gated behind `#[ignore]` (run explicitly with `cargo test -- --ignored`)
- [ ] Reads `ANTHROPIC_API_KEY` from env
- [ ] Sends a simple prompt ("Say hello in exactly 3 words"), collects stream events
- [ ] Verifies: got `MessageStart`, at least one `TextDelta`, `MessageDone`
- [ ] Verifies: final message has `stop_reason: Stop` and `usage.output_tokens > 0`
- [ ] Tool call test: defines a `get_weather(city: String)` tool, sends "What's the weather in Paris?", verifies `ToolCallDelta` events and final `ToolCall` content block
- [ ] `cargo test -p tau-ai -- --ignored` passes with valid API key
- [ ] `cargo check --workspace` passes

### US-REVIEW-PHASE3: Review tau-ai (US-017 through US-022) [ ]

**Description:** Review the LLM abstraction layer as a cohesive system.

**Acceptance Criteria:**
- [ ] Identify phase scope: US-017 to US-022
- [ ] Review all tau-ai source files together
- [ ] Evaluate quality:
  - Types are ergonomic and match Anthropic's actual API well
  - Provider trait is minimal and extensible (easy to add OpenAI later)
  - SSE parser handles edge cases (partial reads, empty data lines, reconnection)
  - Error handling is consistent (Result types, clear error messages)
  - Serde derives work correctly for all types
- [ ] Cross-task analysis:
  - Verify `Context` → Anthropic JSON conversion is complete (system, messages, tools, images)
  - Verify streaming accumulation produces correct final `AssistantMessage`
  - Verify tool call JSON delta accumulation handles partial JSON correctly
  - Check that `Provider` trait doesn't leak Anthropic-specific details
- [ ] If issues found: insert fix tasks, append to progress.txt
- [ ] If no issues: append "## Phase 3 review PASSED" to progress.txt, mark [x]

---

## Phase 4: tau-agent

### US-023: tau-agent crate with AgentTool trait [ ]

**Description:** As a developer, I need the agent crate with a tool trait so tools can be defined and executed.

**Acceptance Criteria:**
- [ ] `crates/tau-agent/Cargo.toml` depending on `tau-ai`
- [ ] `crates/tau-agent/src/tool.rs`:
  ```rust
  pub struct AgentToolDef {
      pub tool: tau_ai::Tool,      // name, description, schema
      pub label: String,           // human-readable label for UI
  }
  
  /// Result of tool execution
  pub struct ToolResult {
      pub content: Vec<tau_ai::ResultContent>,
      pub is_error: bool,
  }
  
  /// Trait for executable tools
  pub trait AgentTool: Send + Sync {
      fn def(&self) -> &AgentToolDef;
      /// Execute the tool. Async because tools do IO (file, process, network).
      /// This is the ONE async boundary — runs in a spawned task.
      fn execute(
          &self,
          params: serde_json::Value,
      ) -> Pin<Box<dyn Future<Output = ToolResult> + Send>>;
  }
  ```
- [ ] Tests: mock tool implementing `AgentTool`, verify def() and execute()
- [ ] `cargo test -p tau-agent` passes
- [ ] `cargo check --workspace` passes

### US-024: Agent state machine (sync core) [ ]

**Description:** As a developer, I need the `Agent` struct — a synchronous state machine that processes events and returns actions to perform.

**Acceptance Criteria:**
- [ ] `crates/tau-agent/src/agent.rs`:
  ```rust
  pub enum AgentState { Idle, Streaming, ExecutingTools, }
  
  /// Actions the caller must perform (spawn async tasks)
  pub enum AgentAction {
      /// Start streaming from LLM with this context
      StreamLlm { context: tau_ai::Context, options: tau_ai::StreamOptions },
      /// Execute this tool call
      ExecuteTool { tool_call: tau_ai::ToolCall },
      /// Agent is done (no more work)
      Done,
  }
  
  pub struct Agent {
      state: AgentState,
      messages: Vec<tau_ai::Message>,
      tools: Vec<Box<dyn AgentTool>>,
      system_prompt: String,
      model: String,
      pending_tool_calls: Vec<tau_ai::ToolCall>,
      tool_results: Vec<tau_ai::ToolResultMessage>,
      current_message: Option<tau_ai::AssistantMessage>,
  }
  ```
- [ ] `Agent::prompt(&mut self, text: &str) -> Vec<AgentAction>`:
  - Appends `UserMessage` to messages
  - Sets state to `Streaming`
  - Returns `[AgentAction::StreamLlm { context, options }]`
- [ ] `Agent::handle_stream_event(&mut self, event: StreamEvent) -> Vec<AgentAction>`:
  - `TextDelta` / `ThinkingDelta` / `ToolCallDelta`: accumulate into `current_message`, return empty (no actions)
  - `MessageDone`: if message has tool calls → set state to `ExecutingTools`, return `AgentAction::ExecuteTool` for each. If no tool calls → set state to `Idle`, return `[AgentAction::Done]`
  - `Error`: set state to `Idle`, store error, return `[AgentAction::Done]`
- [ ] `Agent::handle_tool_result(&mut self, tool_call_id: &str, result: ToolResult) -> Vec<AgentAction>`:
  - Records result as `ToolResultMessage`, removes from pending
  - When all pending complete: appends all tool results to messages, sets state to `Streaming`, returns `[AgentAction::StreamLlm { ... }]` for next turn
- [ ] `Agent::state()`, `Agent::messages()`, `Agent::current_message()` — getters
- [ ] Tests:
  - prompt → returns StreamLlm action
  - handle text deltas → no actions, current_message accumulates
  - handle done with tool calls → returns ExecuteTool actions
  - handle all tool results → returns StreamLlm for next turn
  - handle done without tool calls → returns Done
- [ ] `cargo test -p tau-agent` passes
- [ ] `cargo check --workspace` passes

### US-025: Agent events for UI observation [ ]

**Description:** As a developer, I need the agent to emit events so the TUI can observe and render agent activity.

**Acceptance Criteria:**
- [ ] `crates/tau-agent/src/events.rs`:
  ```rust
  pub enum AgentEvent {
      /// New turn started (LLM call)
      TurnStart,
      /// Streaming text delta from LLM
      TextDelta { delta: String },
      /// Streaming thinking delta
      ThinkingDelta { delta: String },
      /// LLM response complete
      ResponseComplete { message: AssistantMessage },
      /// Tool execution started
      ToolStart { tool_call_id: String, tool_name: String, args: serde_json::Value },
      /// Tool execution finished
      ToolEnd { tool_call_id: String, tool_name: String, result: ToolResult },
      /// Agent finished all work
      AgentDone,
      /// Error occurred
      AgentError { error: String },
  }
  ```
- [ ] `Agent` methods now also return events alongside actions:
  ```rust
  pub struct AgentOutput {
      pub actions: Vec<AgentAction>,
      pub events: Vec<AgentEvent>,
  }
  ```
  - `prompt()` → emits `TurnStart`
  - `handle_stream_event(TextDelta)` → emits `AgentEvent::TextDelta`
  - `handle_stream_event(MessageDone)` → emits `ResponseComplete` + `ToolStart` per tool call
  - `handle_tool_result()` → emits `ToolEnd`, and when all done: `TurnStart` (next turn) or `AgentDone`
- [ ] Tests: verify event sequences for full prompt→stream→tools→stream→done cycle
- [ ] `cargo test -p tau-agent` passes
- [ ] `cargo check --workspace` passes

### US-026: Spawn helpers (async IO wiring) [ ]

**Description:** As a developer, I need helper functions that take `AgentAction`s and spawn the appropriate async tasks, posting results back via a channel.

**Acceptance Criteria:**
- [ ] `crates/tau-agent/src/spawn.rs`:
  ```rust
  /// Spawns async work for an AgentAction, posting results back via tx.
  pub fn spawn_action(
      action: AgentAction,
      provider: Arc<dyn Provider>,
      tools: Arc<Vec<Box<dyn AgentTool>>>,
      tx: UnboundedSender<AgentEvent>,
  )
  ```
  - `StreamLlm`: `tau_iface::spawn` that calls `provider.stream()`, forwards each `StreamEvent` as `AgentEvent` via `tx`
  - `ExecuteTool`: finds tool by name, `tau_iface::spawn` that calls `tool.execute()`, sends `ToolEnd` via `tx`
  - `Done`: sends `AgentDone` via `tx`
- [ ] Handles panics/errors in spawned tasks: catches and sends `AgentError` via `tx`
- [ ] Tests: mock provider + mock tool, verify events arrive on channel
- [ ] `cargo test -p tau-agent` passes
- [ ] `cargo check --workspace` passes

### US-REVIEW-PHASE4: Review tau-agent (US-023 through US-026) [ ]

**Description:** Review the agent framework as a cohesive system.

**Acceptance Criteria:**
- [ ] Identify phase scope: US-023 to US-026
- [ ] Review all tau-agent source files together
- [ ] Evaluate quality:
  - Agent state machine is simple and correct
  - No async in Agent struct — purely sync transitions
  - AgentAction/AgentEvent split is clean
  - Spawn helpers don't leak internal details
- [ ] Cross-task analysis:
  - Trace full cycle: prompt → stream → tool calls → execute → next turn → done
  - Verify tool call ID tracking is correct (pending set management)
  - Verify streaming accumulation builds correct AssistantMessage
  - Check error paths: provider error, tool error, tool panic
  - Verify events arrive in correct order for UI rendering
- [ ] If issues found: insert fix tasks, append to progress.txt
- [ ] If no issues: append "## Phase 4 review PASSED" to progress.txt, mark [x]

---

## Phase 5: tau-cli

### US-027: tau-cli crate with read tool [ ]

**Description:** As a developer, I need the CLI crate with a read tool that reads file contents.

**Acceptance Criteria:**
- [ ] `crates/tau-cli/Cargo.toml` depending on `tau-ai`, `tau-agent`, `tau-tui`
- [ ] `crates/tau-cli/src/tools/read.rs` implementing `AgentTool`:
  - Parameters (JsonSchema): `path: String`, `offset: Option<u32>` (line number), `limit: Option<u32>` (max lines)
  - Reads file, returns content as `TextContent`
  - Truncates output if exceeding 2000 lines or 50KB (whichever first)
  - Returns error result if file doesn't exist or isn't readable
  - Shows `[N more lines, use offset to continue]` when truncated
- [ ] Tests:
  - Read existing file → content matches
  - Read with offset/limit → correct subset
  - Read nonexistent → error result
  - Read large file → truncated with message
- [ ] `cargo test -p tau-cli` passes
- [ ] `cargo check --workspace` passes

### US-028: Bash tool [ ]

**Description:** As a developer, I need a bash tool that executes shell commands.

**Acceptance Criteria:**
- [ ] `crates/tau-cli/src/tools/bash.rs` implementing `AgentTool`:
  - Parameters: `command: String`, `timeout: Option<u64>` (seconds)
  - Spawns `std::process::Command` with `bash -c` (or `sh -c`), wraps child stdout/stderr
    fds in `tau_iface::AsyncFd` for non-blocking read
  - Captures stdout + stderr combined
  - Truncates output to 50KB / 2000 lines
  - Returns exit code in output: `"Exit code: N\n<output>"`
  - Respects timeout (default: no timeout), kills process on timeout
  - Sets CWD to agent's working directory
- [ ] Tests:
  - `echo hello` → "Exit code: 0\nhello\n"
  - `exit 1` → "Exit code: 1\n"
  - Timeout → error result with "timed out" message
  - Large output → truncated
- [ ] `cargo test -p tau-cli` passes
- [ ] `cargo check --workspace` passes

### US-029: Edit and write tools [ ]

**Description:** As a developer, I need edit (find-and-replace) and write (create/overwrite) tools for file manipulation.

**Acceptance Criteria:**
- [ ] `crates/tau-cli/src/tools/write.rs` implementing `AgentTool`:
  - Parameters: `path: String`, `content: String`
  - Creates parent directories if needed (`fs::create_dir_all`)
  - Writes content to file (creates or overwrites)
  - Returns success message with bytes written
- [ ] `crates/tau-cli/src/tools/edit.rs` implementing `AgentTool`:
  - Parameters: `path: String`, `old_text: String`, `new_text: String`
  - Reads file, finds exact match of `old_text`, replaces with `new_text`
  - Error if file doesn't exist, or `old_text` not found, or multiple matches
  - Returns success message showing the replacement
- [ ] Tests:
  - Write new file → file exists with content
  - Write creates parent dirs
  - Edit exact match → replaced
  - Edit no match → error
  - Edit multiple matches → error
- [ ] `cargo test -p tau-cli` passes
- [ ] `cargo check --workspace` passes

### US-030: System prompt and CLI entry point [ ]

**Description:** As a developer, I need the system prompt and a basic CLI that can run the agent in non-interactive (pipe) mode.

**Acceptance Criteria:**
- [ ] `crates/tau-cli/src/prompt.rs`: system prompt string for coding tasks:
  - Describes available tools (read, write, edit, bash)
  - Sets coding assistant persona
  - Instructions for tool use (read before edit, verify after changes)
- [ ] `crates/tau-cli/src/main.rs` with `fn main()` calling `tau_iface::block_on(...)`:
  - Reads `ANTHROPIC_API_KEY` from env (error if missing)
  - CLI args: `tau [prompt]` or `echo "prompt" | tau` (stdin)
  - If prompt provided: creates `Agent`, sets up tools, calls `prompt()`, runs event loop printing to stdout (no TUI), exits when done
  - Prints assistant text to stdout as it streams
  - Prints tool calls and results to stderr
- [ ] `cargo run -p tau-cli -- "What is 2+2?"` works (streams response to stdout)
- [ ] `cargo check --workspace` passes

### US-031: Interactive TUI mode [ ]

**Description:** As a developer, I need an interactive TUI mode where the user can chat with the agent, see streaming responses, and observe tool execution.

**Acceptance Criteria:**
- [ ] `crates/tau-cli/src/interactive.rs` using `tau-tui`:
  - Layout: message history (Text components in a Container) + Input at bottom
  - User types in Input, presses Enter → calls `agent.prompt()`
  - TUI `event_tx` channel receives `AgentEvent`s from spawn helpers
  - Handler matches events:
    - `TurnStart` → add loader/spinner component
    - `TextDelta` → append to current assistant Text component, re-render
    - `ThinkingDelta` → show in dimmed text (collapsible later)
    - `ToolStart` → show tool name + args in styled text
    - `ToolEnd` → show result (truncated if long)
    - `ResponseComplete` → finalize assistant message display
    - `AgentDone` → remove loader, refocus Input
    - `AgentError` → show error in red
  - Scrolling: content grows downward, latest messages visible
  - Ctrl+C while agent running → abort current request
  - Ctrl+C while idle → quit
- [ ] `cargo run -p tau-cli` (no args) → launches interactive mode
- [ ] `cargo check --workspace` passes

### US-REVIEW-PHASE5: Review tau-cli (US-027 through US-031) [ ]

**Description:** Review the coding agent as a complete system.

**Acceptance Criteria:**
- [ ] Identify phase scope: US-027 to US-031
- [ ] Review all tau-cli source files together
- [ ] Evaluate quality:
  - Tools are robust (error handling, edge cases)
  - System prompt is clear and effective
  - TUI integration is clean (no spaghetti event handling)
  - Non-interactive mode works for scripting/piping
- [ ] Cross-task analysis:
  - End-to-end: type prompt → LLM streams → tool calls → results → next turn → display
  - Verify tool output truncation is consistent
  - Verify abort (Ctrl+C) cleanly cancels HTTP stream and tool processes
  - Verify message history renders correctly after multiple turns
  - Check that edit tool handles edge cases (empty files, binary files, unicode)
- [ ] If issues found: insert fix tasks, append to progress.txt
- [ ] If no issues: append "## Phase 5 review PASSED" to progress.txt, mark [x]

---

---

## Non-Goals

- **No alternate-screen mode** — scrolling model only (like pi-mono)
- **No mouse support** — keyboard only
- **No image rendering** — no Kitty/iTerm2 image protocol
- **No markdown rendering** — just raw text and ANSI codes
- **No editor component** — multi-line editing is out of scope
- **No clipboard/paste integration** — no bracketed paste detection
- **No Kitty keyboard protocol** — standard crossterm key events only
- **No layout engine** — components stack vertically, width is passed down, that's it
- **No built-in widgets beyond the basics** — no settings lists, no cancellable loaders
- **No multi-provider support yet** — Anthropic only (Provider trait is extensible for later)
- **No session persistence** — no saving/loading conversation history
- **No model discovery/registry** — hardcoded model string for now
- **No extensions/plugins in v1** — the shared runtime (`tau-rt`) is designed to support cdylib extensions in the future, but the extension loading/API is not in v1 scope. Extensibility via Rust traits only for now. See `TAU-RT-DESIGN.md` and `EXTENSIONS.md`.

## Technical Considerations

- **Ownership model:** Components are stored as `Box<dyn Component>` in containers. Focus is tracked by raw pointer or index, not borrow (avoids lifetime hell). Consider `&mut` access patterns carefully — the `run` handler receives `&mut TUI<E>` to allow mutation.
- **Generic user events:** `TUI<E>` is generic over user event type. Components don't know about `E` — they only implement `Component`. The event type only matters for the run loop and the handler closure. For apps that don't need user events, use `TUI<()>`.
- **Async but components are sync:** The event loop is async (tau-rt reactor poll), but `Component::render()` and `Component::handle_input()` are synchronous. Components never await. Async work happens in `tau_iface::spawn`ed tasks that communicate results via `event_tx`.
- **String-heavy rendering:** Each frame produces `Vec<String>`. This is intentional — matches pi-mono's model and keeps components simple. Optimize later if needed.
- **Flicker-free rendering — three layers:**
  1. **Single buffered write:** Each `render()` builds one `String` with all cursor moves, line clears, and content. One `write()` + `flush()` call per frame. Never multiple writes.
  2. **Synchronized output:** Buffer wrapped in `\x1b[?2026h` / `\x1b[?2026l`. Terminals that support DEC mode 2026 hold the frame until the end marker, then paint atomically.
  3. **Differential rendering:** Only changed lines are rewritten. Cursor moves via relative ANSI escapes (`\x1b[{n}A/B`), individual lines cleared with `\x1b[2K` before rewrite. No full-screen clear unless width changed.
- **Testing without a terminal:** Use `MockTerminal` implementing `Terminal` that captures writes to a `Vec<String>` — all rendering logic is testable without a real terminal. For event loop tests, use `std::sync::mpsc` channels to simulate events.
- **Shared runtime architecture:** `libtau_rt.so`/`.dylib` is the single async runtime. Built as cdylib-only (no rlib). All crates access it through `tau-iface` extern declarations. Future extensions (cdylib plugins) also link against the same `.so`, sharing the reactor and executor. See `TAU-RT-DESIGN.md`.
- **ANSI reset at line end:** Each rendered line gets `\x1b[0m` appended to prevent style bleeding across lines (same as pi-mono's `applyLineResets`).
