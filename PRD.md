# PRD: rtui — Simple Rust TUI Library

## Introduction

A minimal terminal UI library in Rust inspired by [pi-mono's `@mariozechner/pi-tui`](https://github.com/badlogic/pi-mono/tree/main/packages/tui). The library uses a component-based architecture with differential rendering — components return lines of text, the engine diffs against the previous frame and only redraws what changed.

The design follows a scrolling model (not alternate screen): content grows downward, only the bottom viewport is visible, and the terminal scrollback is preserved.

The event loop is async (tokio). The TUI is generic over a user event type `E`, providing an `mpsc::UnboundedSender<E>` that can be cloned and sent to spawned tasks. The main loop `select!`s on crossterm terminal events and user events, rendering after each. This lets applications respond to network IO, timers, or any async work.

## Goals

- Provide a `Component` trait where `render(width) → Vec<String>` is the only required method
- Differential rendering: compare previous vs. new lines, emit only changed lines via ANSI cursor movement
- Correct visible-width calculation for Unicode (wide chars, emoji, grapheme clusters) and ANSI escape codes
- Focus management: one component receives input at a time
- Overlay system: modal components composited on top of base content
- Ship basic components: Text (word-wrap), Box (padding/bg), Spacer, Input (single-line), SelectList
- Async event loop with tokio — `select!` on terminal events + user event channel
- User event channel: `TUI<E>` provides `UnboundedSender<E>` for applications to push custom events from spawned tasks
- Minimal dependencies: `crossterm`, `unicode-width`, `unicode-segmentation`, `tokio`

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
- [x] Module structure in `src/`: `lib.rs`, `terminal.rs`, `utils.rs`, `component.rs`, `tui.rs`, `components/mod.rs`
- [x] Each module file exists with a placeholder comment
- [x] `cargo check` passes

### US-002: Terminal trait and CrosstermTerminal [ ]

**Description:** As a developer, I need a terminal abstraction so the rendering engine is decoupled from the actual terminal.

**Acceptance Criteria:**
- [ ] `Terminal` trait in `src/terminal.rs` with methods:
  - `fn start(&mut self)` — enable raw mode, hide cursor
  - `fn stop(&mut self)` — disable raw mode, show cursor, move cursor past content
  - `fn write(&mut self, data: &str)` — write to stdout
  - `fn flush(&mut self)` — flush stdout
  - `fn size(&self) -> (u16, u16)` — returns `(cols, rows)`
  - `fn hide_cursor(&mut self)` / `fn show_cursor(&mut self)`
- [ ] Note: event reading is NOT on the trait — crossterm's `EventStream` is used directly by the TUI run loop, and `MockTerminal` uses a channel. This keeps the trait sync and simple.
- [ ] `CrosstermTerminal` struct implementing `Terminal` using `crossterm`
  - `start()`: enables raw mode, hides cursor
  - `stop()`: shows cursor, disables raw mode
  - `write()`: writes to stdout buffer with `io::Write`
  - `flush()`: flushes stdout
  - `size()`: delegates to `crossterm::terminal::size()`
- [ ] `MockTerminal` struct implementing `Terminal` that captures writes to a `Vec<String>` for testing
- [ ] Tests: both terminals can be constructed, MockTerminal captures writes
- [ ] `cargo check` passes

### US-003: ANSI escape code utilities [ ]

**Description:** As a developer, I need utilities to parse and strip ANSI escape codes so I can correctly measure visible text width.

**Acceptance Criteria:**
- [ ] `src/utils.rs` with functions:
  - `strip_ansi(s: &str) -> String` — remove all ANSI escape sequences (CSI `\x1b[...m/G/K/H/J`, OSC `\x1b]...\x07`, APC `\x1b_...\x07`)
  - `extract_ansi_code(s: &str, pos: usize) -> Option<(String, usize)>` — extract escape code at position, return code and byte length consumed
- [ ] Tests:
  - `strip_ansi` on plain text returns text unchanged
  - `strip_ansi` on `"\x1b[31mhello\x1b[0m"` returns `"hello"`
  - `strip_ansi` strips OSC hyperlinks (`\x1b]8;;url\x07text\x1b]8;;\x07`)
  - `extract_ansi_code` returns `None` for non-escape positions
  - `extract_ansi_code` returns SGR code and length for `\x1b[31m`
- [ ] `cargo test` passes
- [ ] `cargo check` passes

### US-004: Visible width calculation [ ]

**Description:** As a developer, I need `visible_width()` to measure how many terminal columns a string occupies, ignoring ANSI codes and correctly handling wide/emoji characters.

**Acceptance Criteria:**
- [ ] `visible_width(s: &str) -> usize` in `src/utils.rs`
  - Strips ANSI codes first
  - Uses `unicode_width::UnicodeWidthStr` for base measurement
  - Handles tabs as 3 spaces (matching pi-mono)
- [ ] `truncate_to_width(s: &str, max_width: usize, ellipsis: &str) -> String`
  - Truncates to `max_width` visible columns, appending ellipsis if truncated
  - Preserves ANSI codes (they don't count toward width)
  - Grapheme-cluster-aware: doesn't split multi-byte characters
- [ ] Tests:
  - ASCII: `visible_width("hello")` == 5
  - ANSI: `visible_width("\x1b[31mhello\x1b[0m")` == 5
  - Wide chars: `visible_width("你好")` == 4
  - Tabs: `visible_width("\t")` == 3
  - Truncation: `truncate_to_width("hello world", 8, "...")` == `"hello..."`
  - Truncation preserves ANSI: input with colors → output still has colors up to truncation point
- [ ] `cargo test` passes
- [ ] `cargo check` passes

### US-005: Component trait and Container [ ]

**Description:** As a developer, I need the core `Component` trait and a `Container` that holds children, so I can compose UI elements.

**Acceptance Criteria:**
- [ ] `Component` trait in `src/component.rs`:
  ```rust
  pub trait Component {
      fn render(&self, width: u16) -> Vec<String>;
      fn handle_input(&mut self, _event: &crossterm::event::KeyEvent) {}
      fn invalidate(&mut self) {}
  }
  ```
- [ ] `Container` struct implementing `Component`:
  - Holds `Vec<Box<dyn Component>>`
  - `add_child()`, `remove_child(index)`, `clear()`
  - `render()` concatenates all children's rendered lines
  - `invalidate()` propagates to all children
- [ ] Tests:
  - Empty container renders to empty `Vec`
  - Container with mock components concatenates their output
- [ ] `cargo test` passes
- [ ] `cargo check` passes

### US-006: TUI engine with full rendering [ ]

**Description:** As a developer, I need the TUI engine that renders a component tree to the terminal, initially with full redraws (differential rendering comes next).

**Acceptance Criteria:**
- [ ] `TUI<E>` struct in `src/tui.rs`, generic over user event type `E: Send + 'static`
  - Wraps a `Box<dyn Terminal>` and a root `Container`
  - `new(terminal) -> Self`
  - `root(&mut self) -> &mut Container` — access to root container
  - `render(&mut self)` — renders root container, writes all lines to terminal
  - `start(&mut self)` — calls `terminal.start()`
  - `stop(&mut self)` — moves cursor past content, calls `terminal.stop()`
  - `event_tx(&self) -> UnboundedSender<E>` — returns cloneable sender for user events
- [ ] Rendering builds a **single `String` buffer** with all output (cursor moves, line clears, content), then calls `terminal.write(&buffer)` + `terminal.flush()` **once** — never multiple writes per frame
- [ ] Buffer is wrapped in synchronized output (`\x1b[?2026h` at start, `\x1b[?2026l` at end)
- [ ] Each line gets `\x1b[0m` appended (reset, prevents style bleeding)
- [ ] Stores `previous_lines: Vec<String>` and `previous_width: u16` for next story's diffing
- [ ] Tests: TUI with MockTerminal renders expected output
- [ ] `cargo test` passes
- [ ] `cargo check` passes

### US-007: Differential rendering [ ]

**Description:** As a developer, I need the TUI to only redraw changed lines so terminal output is efficient and flicker-free.

**Acceptance Criteria:**
- [ ] `TUI::render()` compares new lines vs `previous_lines` and builds a **single `String` buffer**:
  - If width changed: full re-render (clear screen `\x1b[3J\x1b[2J\x1b[H` + write all lines)
  - If first render (previous empty): write all lines without clearing
  - Otherwise: find `first_changed` and `last_changed` indices, append cursor movement to `first_changed` (`\x1b[{n}A` / `\x1b[{n}B`), append `\x1b[2K` + new content for each changed line — all into the same buffer
  - Wrap entire buffer in `\x1b[?2026h` ... `\x1b[?2026l`
  - **One** `terminal.write(&buffer)` + `terminal.flush()` call at the end
- [ ] Tracks `cursor_row` (logical end of content) and `hardware_cursor_row` (actual terminal cursor position) for correct cursor movement math
- [ ] If content shrunk: appends line-clear sequences (`\r\n\x1b[2K`) for extra lines, then cursor-up to return
- [ ] Tests with MockTerminal:
  - No changes → no output written
  - Single line changed → only that line rewritten (verify buffer contains exactly one `\x1b[2K` + content)
  - Width change → full redraw (buffer contains clear-screen)
  - Content grew → appends new lines
  - Content shrunk → clears old lines
- [ ] `cargo test` passes
- [ ] `cargo check` passes

### US-008: Async event loop with user events [ ]

**Description:** As a developer, I need an async event loop that handles terminal input, resize, AND user-defined events from spawned tasks.

**Acceptance Criteria:**
- [ ] `Event<E>` enum in `src/tui.rs`:
  ```rust
  pub enum Event<E> {
      Key(crossterm::event::KeyEvent),
      Resize(u16, u16),
      User(E),
  }
  ```
- [ ] `TUI<E>` has an `tokio::sync::mpsc::unbounded_channel` internally:
  - `event_tx()` returns `UnboundedSender<E>` — users clone this and send from `tokio::spawn`ed tasks
  - The receiver is consumed by the run loop
- [ ] `TUI::run<F>(&mut self, handler: F)` where `F: FnMut(Event<E>, &mut TUI<E>)`:
  - Calls `self.start()`
  - Uses `crossterm::event::EventStream` (from `event-stream` feature) for async terminal events
  - `tokio::select!` on:
    - `crossterm_stream.next()` → maps to `Event::Key` / `Event::Resize`
    - `user_rx.recv()` → maps to `Event::User(e)`
  - Calls `handler(event, self)` for each event
  - Calls `self.render()` after each handler invocation
  - Breaks when `self.should_quit` is true
  - Calls `self.stop()` on exit
- [ ] `TUI::quit(&mut self)` — sets `should_quit = true`
- [ ] Focus management: `set_focus()` and `handle_key()` forward to focused component (from previous US-008, now integrated here)
  - `set_focus(component)` — tracks focused component
  - When a `Key` event arrives and there's a focused component, forwards via `handle_input()`
- [ ] Tests:
  - User event arrives via channel → handler receives `Event::User(...)`
  - Key event → handler receives `Event::Key(...)`
  - `quit()` breaks the loop
- [ ] `cargo test` passes
- [ ] `cargo check` passes

### US-REVIEW-PHASE1: Review foundation (US-001 through US-008) [ ]

**Description:** Review the foundation layer as a cohesive system.

**Acceptance Criteria:**
- [ ] Identify phase scope: US-001 to US-008
- [ ] Review all phase code files together
- [ ] Evaluate quality:
  - Good taste: Simple and elegant across all tasks?
  - No special cases: Edge cases handled through design?
  - Data structures: Consistent and appropriate?
  - Complexity: Can anything be simplified?
  - Duplication: Any repeated logic between tasks?
  - Integration: Do components work together cleanly?
- [ ] Cross-task analysis:
  - Verify `Component` trait is ergonomic (not too many required methods)
  - Verify `Terminal` trait is minimal but sufficient (and that event reading being outside the trait works)
  - Verify differential rendering math is correct
  - Verify `visible_width` is used consistently wherever line widths matter
  - Verify async event loop handles edge cases (channel closed, stream ended)
  - Verify focus management integrates cleanly with the event loop
  - Verify `TUI<E>` generic is not overly constraining
- [ ] If issues found:
  - Insert fix tasks after the failing task (US-XXXa, US-XXXb, etc.)
  - Append review findings to progress.txt
  - Do NOT mark this review task [x]
- [ ] If no issues:
  - Append "## Phase 1 review PASSED" to progress.txt
  - Mark this review task [x]
  - Commit: "docs: phase 1 review complete"

### US-009: Spacer component [ ]

**Description:** As a developer, I want a Spacer component that renders N empty lines for vertical spacing.

**Acceptance Criteria:**
- [ ] `Spacer` struct in `src/components/spacer.rs` implementing `Component`
- [ ] Constructor takes `lines: usize` (default 1)
- [ ] `render()` returns `lines` empty strings
- [ ] `set_lines(n)` to update count
- [ ] Tests: `Spacer::new(3).render(80)` returns 3 empty strings
- [ ] `cargo test` passes
- [ ] `cargo check` passes

### US-010: Text component with word wrapping [ ]

**Description:** As a developer, I want a Text component that word-wraps content and preserves ANSI styles across line breaks.

**Acceptance Criteria:**
- [ ] `Text` struct in `src/components/text.rs` implementing `Component`
- [ ] Constructor: `new(text, padding_x, padding_y)`
- [ ] `set_text(text)` to update content (invalidates cache)
- [ ] `render(width)`:
  - Wraps text at `width - 2*padding_x` columns using word boundaries
  - Preserves ANSI codes across line breaks (tracks active SGR state, re-emits at start of continuation lines)
  - Adds `padding_y` empty lines above and below
  - Adds `padding_x` spaces on left, pads right to full width
- [ ] `wrap_text_with_ansi(text: &str, width: usize) -> Vec<String>` utility in `utils.rs`:
  - Splits on word boundaries
  - Breaks words longer than width character-by-character (grapheme-aware)
  - Tracks ANSI SGR state, re-applies at start of each wrapped line
- [ ] Caches rendered output — returns cached result if text and width unchanged
- [ ] Tests:
  - Short text (fits in one line): no wrapping
  - Long text: wraps at word boundary
  - ANSI styled text: style preserved across wrap
  - Empty text: returns empty vec
- [ ] `cargo test` passes
- [ ] `cargo check` passes

### US-011: Box component [ ]

**Description:** As a developer, I want a Box component that wraps children with padding and optional background color.

**Acceptance Criteria:**
- [ ] `BoxComponent` struct in `src/components/box_component.rs` implementing `Component`
- [ ] Holds `Vec<Box<dyn Component>>` children
- [ ] Constructor: `new(padding_x, padding_y)`
- [ ] `set_bg(ansi_code: &str)` — sets background color as raw ANSI code (e.g., `"\x1b[48;5;236m"`)
- [ ] `add_child()`, `remove_child(index)`, `clear()`
- [ ] `render(width)`:
  - Renders children at `width - 2*padding_x`
  - Prepends `padding_x` spaces to each child line
  - Pads each line to full `width`
  - Applies background color to entire padded line if set
  - Adds `padding_y` empty (background-filled) lines above and below
- [ ] Tests:
  - Box with one Text child renders with correct padding
  - Box with background applies bg to all lines including padding
  - Empty box renders nothing
- [ ] `cargo test` passes
- [ ] `cargo check` passes

### US-012: Input component [ ]

**Description:** As a user, I want a single-line text input with cursor, horizontal scrolling, and basic editing keybindings.

**Acceptance Criteria:**
- [ ] `Input` struct in `src/components/input.rs` implementing `Component`
- [ ] Displays `"> "` prompt followed by text with inverse-video cursor
- [ ] Cursor movement: Left, Right, Home, End, Ctrl+Left (word back), Ctrl+Right (word forward)
- [ ] Editing: printable char insertion, Backspace, Delete, Ctrl+Backspace (delete word), Ctrl+U (delete to start), Ctrl+K (delete to end)
- [ ] Horizontal scrolling when text exceeds available width
- [ ] Callbacks: `on_submit: Option<Box<dyn FnMut(&str)>>` (Enter), `on_escape: Option<Box<dyn FnMut()>>` (Escape)
- [ ] `value()` → `&str`, `set_value(s)` to get/set content
- [ ] `focused: bool` field — renders cursor only when focused
- [ ] Tests:
  - Initial render shows `"> "` with cursor
  - After typing "abc", value is "abc" and render shows it
  - Backspace removes last char
  - Left/Right moves cursor, render shows cursor at correct position
- [ ] `cargo test` passes
- [ ] `cargo check` passes

### US-013: SelectList component [ ]

**Description:** As a user, I want a selectable list with arrow-key navigation so I can pick from options.

**Acceptance Criteria:**
- [ ] `SelectList` struct in `src/components/select_list.rs` implementing `Component`
- [ ] `SelectItem { value: String, label: String, description: Option<String> }`
- [ ] Constructor: `new(items, max_visible)`
- [ ] Renders visible window of items, selected item has `→` prefix and distinct styling (bold/inverse)
- [ ] Arrow Up/Down changes selection (wraps around)
- [ ] Enter triggers `on_select` callback, Escape triggers `on_cancel`
- [ ] Scrolls when selection moves outside visible window
- [ ] Shows scroll indicator `(N/M)` when list is scrollable
- [ ] `set_filter(query)` — filters items by prefix match
- [ ] `selected_item() -> Option<&SelectItem>`
- [ ] Tests:
  - Renders correct number of visible items
  - Selection moves with Up/Down
  - Wraps from top to bottom and vice versa
  - Filter narrows visible items
- [ ] `cargo test` passes
- [ ] `cargo check` passes

### US-014: Overlay system [ ]

**Description:** As a developer, I need an overlay system to render modal components (like SelectList popups) on top of base content.

**Acceptance Criteria:**
- [ ] `TUI` gets overlay methods:
  - `show_overlay(component, options) -> OverlayHandle`
  - `hide_overlay()` — hides topmost overlay
  - `has_overlay() -> bool`
- [ ] `OverlayOptions` struct: `width`, `max_height`, `anchor` (Center, TopLeft, BottomLeft, etc.), `offset_x`, `offset_y`
- [ ] `OverlayHandle` with `hide()` and `set_hidden(bool)`
- [ ] Overlay compositing in `render()`:
  - Renders base content first
  - For each visible overlay: renders at its configured width, composites onto base lines at calculated row/col position
  - Compositing: splice overlay content into base line at column offset (slice before + overlay + slice after)
- [ ] Focus saves/restores: showing overlay saves current focus, hiding restores it
- [ ] Overlay stack: multiple overlays, topmost gets input
- [ ] Tests:
  - Single overlay composited at correct position
  - Overlay hide restores focus
  - Overlay stack: topmost gets input
- [ ] `cargo test` passes
- [ ] `cargo check` passes

### US-REVIEW-PHASE2: Review components and overlays (US-009 through US-014) [ ]

**Description:** Review all components and the overlay system as a cohesive layer.

**Acceptance Criteria:**
- [ ] Identify phase scope: US-009 to US-014
- [ ] Review all component files together
- [ ] Evaluate quality:
  - Consistent API patterns across components
  - Component trait is not fighting Rust's ownership model
  - Overlay compositing handles edge cases (wide chars at boundaries, ANSI codes)
  - visible_width used correctly everywhere
- [ ] Cross-task analysis:
  - Verify all components pad output to full width (no rendering artifacts)
  - Verify Input cursor math is correct with Unicode
  - Verify overlay focus save/restore works with nested overlays
  - Check SelectList + overlay integration works (common pattern: popup select list)
  - Verify `TUI<E>` generic doesn't leak into Component trait (components shouldn't care about E)
- [ ] If issues found:
  - Insert fix tasks
  - Append findings to progress.txt
- [ ] If no issues:
  - Append "## Phase 2 review PASSED" to progress.txt
  - Mark this review task [x]

### US-015: Example application with async user events [ ]

**Description:** As a developer, I want a working example app that demonstrates all components and async user events so I can verify everything works together.

**Acceptance Criteria:**
- [ ] `examples/demo.rs` with a runnable app using `#[tokio::main]`
- [ ] Shows: Text with styled content, Box with background, Input that echoes typed text
- [ ] Demonstrates user events: spawns a `tokio::spawn` task that sends a timer event every second via `event_tx`, updating a counter in the UI
- [ ] SelectList overlay triggered by a key (e.g., Ctrl+P)
- [ ] Quit with Ctrl+C or Escape (when no overlay)
- [ ] Demonstrates focus switching between Input and SelectList
- [ ] `cargo run --example demo` works
- [ ] `cargo check` passes

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

## Technical Considerations

- **Ownership model:** Components are stored as `Box<dyn Component>` in containers. Focus is tracked by raw pointer or index, not borrow (avoids lifetime hell). Consider `&mut` access patterns carefully — the `run` handler receives `&mut TUI<E>` to allow mutation.
- **Generic user events:** `TUI<E>` is generic over user event type. Components don't know about `E` — they only implement `Component`. The event type only matters for the run loop and the handler closure. For apps that don't need user events, use `TUI<()>`.
- **Async but components are sync:** The event loop is async (tokio select), but `Component::render()` and `Component::handle_input()` are synchronous. Components never await. Async work happens in spawned tasks that communicate results via `event_tx`.
- **String-heavy rendering:** Each frame produces `Vec<String>`. This is intentional — matches pi-mono's model and keeps components simple. Optimize later if needed.
- **Flicker-free rendering — three layers:**
  1. **Single buffered write:** Each `render()` builds one `String` with all cursor moves, line clears, and content. One `write()` + `flush()` call per frame. Never multiple writes.
  2. **Synchronized output:** Buffer wrapped in `\x1b[?2026h` / `\x1b[?2026l`. Terminals that support DEC mode 2026 hold the frame until the end marker, then paint atomically.
  3. **Differential rendering:** Only changed lines are rewritten. Cursor moves via relative ANSI escapes (`\x1b[{n}A/B`), individual lines cleared with `\x1b[2K` before rewrite. No full-screen clear unless width changed.
- **Testing without a terminal:** Use `MockTerminal` implementing `Terminal` that captures writes to a `Vec<String>` — all rendering logic is testable without a real terminal. For event loop tests, use `tokio::sync::mpsc` channels to simulate events.
- **ANSI reset at line end:** Each rendered line gets `\x1b[0m` appended to prevent style bleeding across lines (same as pi-mono's `applyLineResets`).
