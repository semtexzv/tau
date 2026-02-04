// TUI engine with rendering and async event loop.

use std::cell::Cell;
use std::fmt::Write;
use std::rc::Rc;
use futures::StreamExt;
use tokio::sync::mpsc::{self, UnboundedSender, UnboundedReceiver};

use crate::component::{Component, Container};
use crate::terminal::Terminal;
use crate::utils::{visible_width, truncate_to_width, slice_from_column};

/// Events delivered to the TUI handler.
#[derive(Debug)]
pub enum Event<E> {
    /// A keyboard input event.
    Key(crossterm::event::KeyEvent),
    /// Terminal was resized to (cols, rows).
    Resize(u16, u16),
    /// A user-defined event from a spawned task.
    User(E),
}

/// Anchor point for overlay positioning relative to the base content area.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Anchor {
    Center,
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

/// Options for overlay positioning and sizing.
#[derive(Debug, Clone)]
pub struct OverlayOptions {
    pub width: u16,
    pub max_height: Option<u16>,
    pub anchor: Anchor,
    pub offset_x: i16,
    pub offset_y: i16,
}

/// Handle to a displayed overlay, allowing visibility control.
///
/// Cloning creates another reference to the same overlay's visibility state.
/// Use `hide()` to make the overlay invisible without removing it from the stack.
/// To fully remove an overlay and restore focus, use `TUI::hide_overlay()`.
#[derive(Clone)]
pub struct OverlayHandle {
    hidden: Rc<Cell<bool>>,
}

impl OverlayHandle {
    /// Hide this overlay (sets hidden = true).
    pub fn hide(&self) {
        self.hidden.set(true);
    }

    /// Set the hidden state explicitly.
    pub fn set_hidden(&self, hidden: bool) {
        self.hidden.set(hidden);
    }
}

/// Internal overlay entry in the stack.
struct OverlayEntry {
    component: Box<dyn Component>,
    options: OverlayOptions,
    hidden: Rc<Cell<bool>>,
    saved_focus: Option<usize>,
}

/// The main TUI engine. Renders a component tree to a terminal.
///
/// Generic over user event type `E`, providing an `mpsc::UnboundedSender<E>`
/// that can be cloned and sent to spawned tasks. Components don't know about `E`.
/// For apps that don't need user events, use `TUI<()>`.
pub struct TUI<E: Send + 'static> {
    terminal: Box<dyn Terminal>,
    root: Container,
    /// Lines from the most recent render — used for differential rendering.
    previous_lines: Vec<String>,
    /// Terminal width from the most recent render — width change triggers full redraw.
    previous_width: u16,
    /// Logical cursor position: number of content lines from the last render.
    cursor_row: usize,
    /// Actual terminal cursor row position (may differ from cursor_row after differential render).
    hardware_cursor_row: usize,
    event_tx: UnboundedSender<E>,
    event_rx: Option<UnboundedReceiver<E>>,
    /// Whether the run loop should exit.
    should_quit: bool,
    /// Index of the focused child component in root (receives key input).
    focused: Option<usize>,
    /// Sender for injecting terminal (crossterm) events into the run loop.
    /// In production, a spawned task bridges crossterm's EventStream here.
    /// In tests, events are injected directly.
    crossterm_tx: UnboundedSender<crossterm::event::Event>,
    crossterm_rx: Option<UnboundedReceiver<crossterm::event::Event>>,
    /// Stack of overlay entries (topmost is last).
    overlays: Vec<OverlayEntry>,
}

impl<E: Send + 'static> TUI<E> {
    /// Create a new TUI engine wrapping the given terminal.
    pub fn new(terminal: Box<dyn Terminal>) -> Self {
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let (crossterm_tx, crossterm_rx) = mpsc::unbounded_channel();
        TUI {
            terminal,
            root: Container::new(),
            previous_lines: Vec::new(),
            previous_width: 0,
            cursor_row: 0,
            hardware_cursor_row: 0,
            event_tx,
            event_rx: Some(event_rx),
            should_quit: false,
            focused: None,
            crossterm_tx,
            crossterm_rx: Some(crossterm_rx),
            overlays: Vec::new(),
        }
    }

    /// Access the root container for adding/removing child components.
    pub fn root(&mut self) -> &mut Container {
        &mut self.root
    }

    /// Returns a cloneable sender for pushing user events from spawned tasks.
    pub fn event_tx(&self) -> UnboundedSender<E> {
        self.event_tx.clone()
    }

    /// Returns a sender for injecting terminal events (key, resize).
    /// In production, the run loop spawns a task that bridges crossterm's
    /// EventStream to this channel. For testing, inject events directly.
    pub fn crossterm_event_tx(&self) -> UnboundedSender<crossterm::event::Event> {
        self.crossterm_tx.clone()
    }

    /// Signal the run loop to exit after the current handler returns.
    pub fn quit(&mut self) {
        self.should_quit = true;
    }

    /// Set which child component in root has focus (receives key input).
    /// Pass `None` to clear focus.
    pub fn set_focus(&mut self, index: Option<usize>) {
        self.focused = index;
    }

    /// Returns the index of the currently focused child, if any.
    pub fn focused(&self) -> Option<usize> {
        self.focused
    }

    /// Show an overlay component on top of the base content.
    ///
    /// Saves the current focus state. Returns an `OverlayHandle` for
    /// controlling the overlay's visibility. Multiple overlays form a stack;
    /// the topmost visible overlay receives key input.
    pub fn show_overlay(
        &mut self,
        component: Box<dyn Component>,
        options: OverlayOptions,
    ) -> OverlayHandle {
        let hidden = Rc::new(Cell::new(false));
        let handle = OverlayHandle { hidden: hidden.clone() };
        let saved_focus = self.focused;
        self.overlays.push(OverlayEntry {
            component,
            options,
            hidden,
            saved_focus,
        });
        handle
    }

    /// Remove the topmost overlay and restore its saved focus state.
    pub fn hide_overlay(&mut self) {
        if let Some(entry) = self.overlays.pop() {
            self.focused = entry.saved_focus;
        }
    }

    /// Returns whether any overlay is currently visible (not hidden).
    pub fn has_overlay(&self) -> bool {
        self.overlays.iter().any(|e| !e.hidden.get())
    }

    /// Start the terminal (enable raw mode, hide cursor).
    pub fn start(&mut self) {
        self.terminal.start();
    }

    /// Stop the terminal (show cursor, disable raw mode).
    /// Moves cursor from `hardware_cursor_row` to `cursor_row` (end of content)
    /// so the shell prompt appears below all TUI output, not mid-content.
    pub fn stop(&mut self) {
        if self.hardware_cursor_row < self.cursor_row {
            let n = self.cursor_row - self.hardware_cursor_row;
            self.terminal.write(&format!("\x1b[{}B", n));
            self.terminal.flush();
            self.hardware_cursor_row = self.cursor_row;
        }
        self.terminal.stop();
    }

    /// Run the async event loop.
    ///
    /// Calls `start()`, then enters a `tokio::select!` loop reading from
    /// both the crossterm event channel and the user event channel. Each
    /// event is forwarded to `handler`, followed by `render()`. The loop
    /// exits when `quit()` is called or both channels close. Calls `stop()`
    /// on exit.
    ///
    /// Key events are automatically forwarded to the focused component
    /// (if any) before the handler is called.
    pub async fn run<F>(&mut self, mut handler: F)
    where
        F: FnMut(Event<E>, &mut TUI<E>),
    {
        self.start();
        self.render();

        let mut user_rx = self.event_rx.take().expect("run() can only be called once");
        let mut crossterm_rx = self
            .crossterm_rx
            .take()
            .expect("run() can only be called once");

        // Spawn a task that bridges crossterm's EventStream to our channel.
        let ct_tx = self.crossterm_tx.clone();
        let reader_handle = tokio::spawn(async move {
            let mut stream = crossterm::event::EventStream::new();
            while let Some(result) = stream.next().await {
                match result {
                    Ok(event) => {
                        if ct_tx.send(event).is_err() {
                            break; // receiver dropped
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        loop {
            let event = tokio::select! {
                Some(ct_event) = crossterm_rx.recv() => {
                    match ct_event {
                        crossterm::event::Event::Key(key) => Some(Event::Key(key)),
                        crossterm::event::Event::Resize(w, h) => Some(Event::Resize(w, h)),
                        _ => None,
                    }
                }
                Some(user_event) = user_rx.recv() => {
                    Some(Event::User(user_event))
                }
                else => break,
            };

            if let Some(event) = event {
                // Forward key events: overlays first, then focused component
                if let Event::Key(ref key) = event {
                    let mut forwarded = false;
                    for entry in self.overlays.iter_mut().rev() {
                        if !entry.hidden.get() {
                            entry.component.handle_input(key);
                            forwarded = true;
                            break;
                        }
                    }
                    if !forwarded {
                        if let Some(idx) = self.focused {
                            if let Some(child) = self.root.child_mut(idx) {
                                child.handle_input(key);
                            }
                        }
                    }
                }

                handler(event, self);
                self.render();
            }

            if self.should_quit {
                break;
            }
        }

        reader_handle.abort();
        self.stop();
    }

    /// Render the component tree to the terminal with differential rendering.
    ///
    /// Compares new lines vs `previous_lines` to minimize terminal output:
    /// - First render: writes all lines without clearing
    /// - Width changed: full re-render with screen clear
    /// - Otherwise: only rewrites changed lines using cursor movement
    ///
    /// Builds a single `String` buffer, wraps in synchronized output markers,
    /// then calls `terminal.write()` + `terminal.flush()` once.
    /// If nothing changed, no output is written at all.
    pub fn render(&mut self) {
        let (width, _height) = self.terminal.size();
        let mut lines = self.root.render(width);

        // Composite visible overlays onto base content
        for overlay in &self.overlays {
            if overlay.hidden.get() {
                continue;
            }
            let ov_width = (overlay.options.width as usize).min(width as usize);
            if ov_width == 0 {
                continue;
            }
            let mut ov_lines = overlay.component.render(ov_width as u16);
            if let Some(max_h) = overlay.options.max_height {
                ov_lines.truncate(max_h as usize);
            }
            let ov_height = ov_lines.len();
            if ov_height == 0 {
                continue;
            }

            let (row, col) = calculate_overlay_position(
                &overlay.options,
                width as usize,
                lines.len(),
                ov_width,
                ov_height,
            );

            // Extend base lines if overlay goes beyond content
            while lines.len() < row + ov_height {
                lines.push(String::new());
            }

            // Splice each overlay line into the corresponding base line
            for (i, ov_line) in ov_lines.iter().enumerate() {
                lines[row + i] =
                    splice_overlay_into_line(&lines[row + i], col, ov_line, ov_width);
            }
        }

        let mut buffer = String::new();
        let is_first_render = self.previous_width == 0;

        if is_first_render {
            // First render: write all lines without clearing
            for line in &lines {
                buffer.push_str(line);
                buffer.push_str("\x1b[0m\r\n");
            }
            self.hardware_cursor_row = lines.len();
        } else if width != self.previous_width {
            // Width changed: full re-render with screen clear
            buffer.push_str("\x1b[3J\x1b[2J\x1b[H");
            for line in &lines {
                buffer.push_str(line);
                buffer.push_str("\x1b[0m\r\n");
            }
            self.hardware_cursor_row = lines.len();
        } else {
            // Differential render: compare previous vs new
            let old = &self.previous_lines;
            let max_len = old.len().max(lines.len());

            // Find first and last changed indices
            let mut first_changed: Option<usize> = None;
            let mut last_changed: Option<usize> = None;

            for i in 0..max_len {
                let old_line = old.get(i);
                let new_line = lines.get(i);
                if old_line != new_line {
                    if first_changed.is_none() {
                        first_changed = Some(i);
                    }
                    last_changed = Some(i);
                }
            }

            if let (Some(first), Some(last)) = (first_changed, last_changed) {
                // Move cursor from hardware_cursor_row to first_changed
                if self.hardware_cursor_row > first {
                    write!(buffer, "\x1b[{}A", self.hardware_cursor_row - first).unwrap();
                } else if self.hardware_cursor_row < first {
                    write!(buffer, "\x1b[{}B", first - self.hardware_cursor_row).unwrap();
                }
                buffer.push('\r'); // Ensure column 0

                // Write all lines from first to last
                for i in first..=last {
                    buffer.push_str("\x1b[2K");
                    if let Some(line) = lines.get(i) {
                        buffer.push_str(line);
                        buffer.push_str("\x1b[0m\r\n");
                    } else {
                        // Content shrunk: clear this old line and advance
                        buffer.push_str("\r\n");
                    }
                }

                // Cursor is now at last + 1
                let cursor_pos = last + 1;

                // If we went past the new content end, move cursor back
                if cursor_pos > lines.len() {
                    write!(buffer, "\x1b[{}A", cursor_pos - lines.len()).unwrap();
                    self.hardware_cursor_row = lines.len();
                } else {
                    self.hardware_cursor_row = cursor_pos;
                }
            }
            // else: no changes, buffer stays empty → no write
        }

        // Only write if there's something to output
        if !buffer.is_empty() {
            let mut output = String::with_capacity(buffer.len() + 20);
            output.push_str("\x1b[?2026h");
            output.push_str(&buffer);
            output.push_str("\x1b[?2026l");
            self.terminal.write(&output);
            self.terminal.flush();
        }

        // Update state
        self.cursor_row = lines.len();
        self.previous_lines = lines;
        self.previous_width = width;
    }

    /// Access stored lines from the previous render.
    pub fn previous_lines(&self) -> &[String] {
        &self.previous_lines
    }

    /// Access stored width from the previous render.
    pub fn previous_width(&self) -> u16 {
        self.previous_width
    }
}

/// Calculate the (row, col) position for an overlay within the content area.
fn calculate_overlay_position(
    options: &OverlayOptions,
    content_width: usize,
    content_height: usize,
    overlay_width: usize,
    overlay_height: usize,
) -> (usize, usize) {
    let (base_row, base_col) = match options.anchor {
        Anchor::Center => (
            content_height.saturating_sub(overlay_height) / 2,
            content_width.saturating_sub(overlay_width) / 2,
        ),
        Anchor::TopLeft => (0, 0),
        Anchor::TopRight => (0, content_width.saturating_sub(overlay_width)),
        Anchor::BottomLeft => (content_height.saturating_sub(overlay_height), 0),
        Anchor::BottomRight => (
            content_height.saturating_sub(overlay_height),
            content_width.saturating_sub(overlay_width),
        ),
    };

    let row = (base_row as i32 + options.offset_y as i32).max(0) as usize;
    let col = (base_col as i32 + options.offset_x as i32).max(0) as usize;
    (row, col)
}

/// Splice overlay content into a base line at the given column position.
///
/// Cuts the base line at `col`, inserts the overlay content, then resumes
/// the base line after `col + overlay_width`. Resets ANSI state between
/// sections and re-applies the base's SGR state for the "after" portion.
fn splice_overlay_into_line(
    base: &str,
    col: usize,
    overlay: &str,
    overlay_width: usize,
) -> String {
    let before = truncate_to_width(base, col, "");
    let before_width = visible_width(&before);
    let pad = " ".repeat(col.saturating_sub(before_width));

    let base_width = visible_width(base);
    let after_col = col + overlay_width;
    let after = if after_col < base_width {
        let (sgr, remaining) = slice_from_column(base, after_col);
        format!("{}{}", sgr, remaining)
    } else {
        String::new()
    };

    format!("{}{}\x1b[0m{}\x1b[0m{}", before, pad, overlay, after)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::component::Component;
    use crate::terminal::MockTerminal;

    /// Helper: get a reference to the MockTerminal inside a TUI.
    fn mock_terminal(tui: &TUI<()>) -> &MockTerminal {
        tui.terminal
            .as_any()
            .downcast_ref::<MockTerminal>()
            .expect("terminal should be MockTerminal")
    }

    /// A simple test component that returns fixed lines.
    struct StubComponent {
        lines: Vec<String>,
    }

    impl StubComponent {
        fn new(lines: &[&str]) -> Self {
            StubComponent {
                lines: lines.iter().map(|s| s.to_string()).collect(),
            }
        }
    }

    impl Component for StubComponent {
        fn render(&self, _width: u16) -> Vec<String> {
            self.lines.clone()
        }
    }

    // ── Construction ────────────────────────────────────────────────

    #[test]
    fn new_creates_empty_tui() {
        let tui: TUI<()> = TUI::new(Box::new(MockTerminal::new(80, 24)));
        assert!(tui.previous_lines.is_empty());
        assert_eq!(tui.previous_width, 0);
    }

    #[test]
    fn root_returns_empty_container() {
        let mut tui: TUI<()> = TUI::new(Box::new(MockTerminal::new(80, 24)));
        assert!(tui.root().is_empty());
    }

    #[test]
    fn root_allows_adding_children() {
        let mut tui: TUI<()> = TUI::new(Box::new(MockTerminal::new(80, 24)));
        tui.root().add_child(Box::new(StubComponent::new(&["hello"])));
        assert_eq!(tui.root().len(), 1);
    }

    // ── Start / Stop ────────────────────────────────────────────────

    #[test]
    fn start_calls_terminal_start() {
        let mut tui: TUI<()> = TUI::new(Box::new(MockTerminal::new(80, 24)));
        tui.start();
        let mock = mock_terminal(&tui);
        assert!(mock.started);
    }

    #[test]
    fn stop_calls_terminal_stop() {
        let mut tui: TUI<()> = TUI::new(Box::new(MockTerminal::new(80, 24)));
        tui.stop();
        let mock = mock_terminal(&tui);
        assert!(mock.stopped);
    }

    // ── Rendering ───────────────────────────────────────────────────

    #[test]
    fn render_empty_first_render_no_output() {
        let mut tui: TUI<()> = TUI::new(Box::new(MockTerminal::new(80, 24)));
        tui.render();
        let mock = mock_terminal(&tui);
        assert_eq!(mock.writes.len(), 0, "no writes for empty first render");
        // State is still updated
        assert_eq!(tui.previous_width(), 80);
        assert!(tui.previous_lines().is_empty());
    }

    #[test]
    fn render_writes_single_line_with_reset() {
        let mut tui: TUI<()> = TUI::new(Box::new(MockTerminal::new(80, 24)));
        tui.root().add_child(Box::new(StubComponent::new(&["hello"])));
        tui.render();
        let mock = mock_terminal(&tui);
        assert_eq!(mock.writes.len(), 1, "exactly one write call");
        let output = mock.output();
        assert_eq!(output, "\x1b[?2026hhello\x1b[0m\r\n\x1b[?2026l");
    }

    #[test]
    fn render_writes_multiple_lines_with_resets() {
        let mut tui: TUI<()> = TUI::new(Box::new(MockTerminal::new(80, 24)));
        tui.root()
            .add_child(Box::new(StubComponent::new(&["alpha", "beta"])));
        tui.render();
        let output = mock_terminal(&tui).output();
        assert_eq!(
            output,
            "\x1b[?2026halpha\x1b[0m\r\nbeta\x1b[0m\r\n\x1b[?2026l"
        );
    }

    #[test]
    fn render_preserves_ansi_in_content() {
        let mut tui: TUI<()> = TUI::new(Box::new(MockTerminal::new(80, 24)));
        tui.root()
            .add_child(Box::new(StubComponent::new(&["\x1b[31mred\x1b[0m"])));
        tui.render();
        let output = mock_terminal(&tui).output();
        // The content's own ANSI codes are preserved; reset is appended
        assert!(output.contains("\x1b[31mred\x1b[0m\x1b[0m\r\n"));
    }

    #[test]
    fn render_stores_previous_lines() {
        let mut tui: TUI<()> = TUI::new(Box::new(MockTerminal::new(80, 24)));
        tui.root()
            .add_child(Box::new(StubComponent::new(&["line1", "line2"])));
        tui.render();
        assert_eq!(tui.previous_lines(), &["line1", "line2"]);
    }

    #[test]
    fn render_stores_previous_width() {
        let mut tui: TUI<()> = TUI::new(Box::new(MockTerminal::new(120, 40)));
        tui.render();
        assert_eq!(tui.previous_width(), 120);
    }

    #[test]
    fn render_uses_terminal_width() {
        // A component that includes the width in its output for verification
        struct WidthReporter;
        impl Component for WidthReporter {
            fn render(&self, width: u16) -> Vec<String> {
                vec![format!("w={}", width)]
            }
        }

        let mut tui: TUI<()> = TUI::new(Box::new(MockTerminal::new(42, 10)));
        tui.root().add_child(Box::new(WidthReporter));
        tui.render();
        let output = mock_terminal(&tui).output();
        assert!(output.contains("w=42"));
    }

    #[test]
    fn render_exactly_one_write_per_frame() {
        let mut tui: TUI<()> = TUI::new(Box::new(MockTerminal::new(80, 24)));
        tui.root()
            .add_child(Box::new(StubComponent::new(&["a", "b", "c"])));
        tui.render();
        assert_eq!(mock_terminal(&tui).writes.len(), 1);
    }

    #[test]
    fn render_starts_with_sync_start() {
        let mut tui: TUI<()> = TUI::new(Box::new(MockTerminal::new(80, 24)));
        tui.root()
            .add_child(Box::new(StubComponent::new(&["test"])));
        tui.render();
        let output = mock_terminal(&tui).output();
        assert!(output.starts_with("\x1b[?2026h"));
    }

    #[test]
    fn render_ends_with_sync_end() {
        let mut tui: TUI<()> = TUI::new(Box::new(MockTerminal::new(80, 24)));
        tui.root()
            .add_child(Box::new(StubComponent::new(&["test"])));
        tui.render();
        let output = mock_terminal(&tui).output();
        assert!(output.ends_with("\x1b[?2026l"));
    }

    #[test]
    fn same_content_second_render_no_output() {
        let mut tui: TUI<()> = TUI::new(Box::new(MockTerminal::new(80, 24)));
        tui.root()
            .add_child(Box::new(StubComponent::new(&["hello"])));
        tui.render();
        assert_eq!(mock_terminal(&tui).writes.len(), 1, "first render writes");
        tui.render();
        // Second render with same content produces no new write
        assert_eq!(mock_terminal(&tui).writes.len(), 1, "no new write for same content");
    }

    #[test]
    fn previous_lines_updates_on_each_render() {
        let mut tui: TUI<()> = TUI::new(Box::new(MockTerminal::new(80, 24)));
        tui.root()
            .add_child(Box::new(StubComponent::new(&["first"])));
        tui.render();
        assert_eq!(tui.previous_lines(), &["first"]);

        tui.root().clear();
        tui.root()
            .add_child(Box::new(StubComponent::new(&["second"])));
        tui.render();
        assert_eq!(tui.previous_lines(), &["second"]);
    }

    // ── Event channel ───────────────────────────────────────────────

    #[test]
    fn event_tx_returns_working_sender() {
        let tui: TUI<String> = TUI::new(Box::new(MockTerminal::new(80, 24)));
        let tx = tui.event_tx();
        // Should succeed (receiver exists inside TUI)
        assert!(tx.send("hello".to_string()).is_ok());
    }

    #[test]
    fn event_tx_is_cloneable() {
        let tui: TUI<i32> = TUI::new(Box::new(MockTerminal::new(80, 24)));
        let tx1 = tui.event_tx();
        let tx2 = tx1.clone();
        assert!(tx1.send(1).is_ok());
        assert!(tx2.send(2).is_ok());
    }

    #[test]
    fn tui_with_unit_event_type() {
        // Common case: no user events needed
        let mut tui: TUI<()> = TUI::new(Box::new(MockTerminal::new(80, 24)));
        tui.start();
        tui.render();
        tui.stop();
        let mock = mock_terminal(&tui);
        assert!(mock.started);
        assert!(mock.stopped);
    }

    // ── Differential Rendering ──────────────────────────────────────

    #[test]
    fn diff_no_changes_no_output() {
        let mut tui: TUI<()> = TUI::new(Box::new(MockTerminal::new(80, 24)));
        tui.root()
            .add_child(Box::new(StubComponent::new(&["alpha", "beta", "gamma"])));
        tui.render(); // first render
        let writes_after_first = mock_terminal(&tui).writes.len();

        tui.render(); // same content
        assert_eq!(
            mock_terminal(&tui).writes.len(),
            writes_after_first,
            "no output written when nothing changed"
        );
    }

    #[test]
    fn diff_single_line_changed() {
        let mut tui: TUI<()> = TUI::new(Box::new(MockTerminal::new(80, 24)));
        tui.root()
            .add_child(Box::new(StubComponent::new(&["A", "B", "C"])));
        tui.render(); // first render

        // Change middle line
        tui.root().clear();
        tui.root()
            .add_child(Box::new(StubComponent::new(&["A", "X", "C"])));
        tui.render(); // differential render

        let last_write = mock_terminal(&tui).writes.last().unwrap();
        // Should contain exactly one \x1b[2K (for the single changed line)
        assert_eq!(
            last_write.matches("\x1b[2K").count(),
            1,
            "exactly one line clear for single changed line"
        );
        // Should contain the new content
        assert!(last_write.contains("X"), "contains new line content");
        // Should NOT contain unchanged lines
        assert!(
            !last_write.contains("\x1b[2KA"),
            "does not rewrite unchanged first line"
        );
        assert!(
            !last_write.contains("\x1b[2KC"),
            "does not rewrite unchanged last line"
        );
    }

    #[test]
    fn diff_width_change_full_redraw() {
        let mut tui: TUI<()> = TUI::new(Box::new(MockTerminal::new(80, 24)));
        tui.root()
            .add_child(Box::new(StubComponent::new(&["hello"])));
        tui.render(); // first render at width 80

        // Change terminal width
        tui.terminal
            .as_any_mut()
            .downcast_mut::<MockTerminal>()
            .unwrap()
            .set_size(120, 24);
        tui.render(); // should trigger full redraw

        let last_write = mock_terminal(&tui).writes.last().unwrap();
        // Full redraw: contains clear-screen sequence
        assert!(
            last_write.contains("\x1b[3J\x1b[2J\x1b[H"),
            "width change triggers clear-screen"
        );
        // Contains all content
        assert!(last_write.contains("hello"), "full redraw contains content");
    }

    #[test]
    fn diff_content_grew() {
        let mut tui: TUI<()> = TUI::new(Box::new(MockTerminal::new(80, 24)));
        tui.root()
            .add_child(Box::new(StubComponent::new(&["A"])));
        tui.render(); // first render: 1 line

        tui.root().clear();
        tui.root()
            .add_child(Box::new(StubComponent::new(&["A", "B", "C"])));
        tui.render(); // differential: grew from 1 to 3

        let last_write = mock_terminal(&tui).writes.last().unwrap();
        // Should contain the new lines
        assert!(last_write.contains("B"), "contains new line B");
        assert!(last_write.contains("C"), "contains new line C");
        // Should NOT rewrite unchanged line A
        assert!(
            !last_write.contains("\x1b[2KA"),
            "does not rewrite unchanged line A"
        );
    }

    #[test]
    fn diff_content_shrunk() {
        let mut tui: TUI<()> = TUI::new(Box::new(MockTerminal::new(80, 24)));
        tui.root()
            .add_child(Box::new(StubComponent::new(&["A", "B", "C", "D"])));
        tui.render(); // first render: 4 lines

        tui.root().clear();
        tui.root()
            .add_child(Box::new(StubComponent::new(&["A", "B"])));
        tui.render(); // differential: shrunk from 4 to 2

        let last_write = mock_terminal(&tui).writes.last().unwrap();
        // Should contain clear sequences for the removed lines (C and D)
        // Lines at indices 2 and 3 are cleared with \x1b[2K
        assert_eq!(
            last_write.matches("\x1b[2K").count(),
            2,
            "two line-clears for two removed lines"
        );
        // Should contain cursor-up to return to logical end of content
        assert!(
            last_write.contains("\x1b[2A"),
            "cursor-up 2 to return from row 4 to row 2"
        );
    }

    #[test]
    fn diff_content_shrunk_to_empty() {
        let mut tui: TUI<()> = TUI::new(Box::new(MockTerminal::new(80, 24)));
        tui.root()
            .add_child(Box::new(StubComponent::new(&["A", "B"])));
        tui.render(); // first render: 2 lines

        tui.root().clear();
        tui.render(); // differential: shrunk to empty

        let last_write = mock_terminal(&tui).writes.last().unwrap();
        // Two lines cleared
        assert_eq!(last_write.matches("\x1b[2K").count(), 2);
        // Cursor returns to row 0
        assert!(last_write.contains("\x1b[2A"), "cursor-up 2 to return to row 0");
    }

    #[test]
    fn diff_content_grew_from_empty() {
        let mut tui: TUI<()> = TUI::new(Box::new(MockTerminal::new(80, 24)));
        tui.render(); // first render: empty

        tui.root()
            .add_child(Box::new(StubComponent::new(&["A", "B"])));
        tui.render(); // differential: grew from empty to 2 lines

        let last_write = mock_terminal(&tui).writes.last().unwrap();
        assert!(last_write.contains("A"));
        assert!(last_write.contains("B"));
    }

    #[test]
    fn diff_multiple_sequential_changes() {
        let mut tui: TUI<()> = TUI::new(Box::new(MockTerminal::new(80, 24)));
        tui.root()
            .add_child(Box::new(StubComponent::new(&["A", "B", "C"])));
        tui.render();

        // First change: update line 1
        tui.root().clear();
        tui.root()
            .add_child(Box::new(StubComponent::new(&["A", "X", "C"])));
        tui.render();

        // Second change: update line 2
        tui.root().clear();
        tui.root()
            .add_child(Box::new(StubComponent::new(&["A", "X", "Y"])));
        tui.render();

        // Both differential renders should have written
        assert_eq!(
            mock_terminal(&tui).writes.len(),
            3,
            "first render + two differential updates"
        );
    }

    #[test]
    fn diff_cursor_movement_uses_hardware_position() {
        let mut tui: TUI<()> = TUI::new(Box::new(MockTerminal::new(80, 24)));
        tui.root()
            .add_child(Box::new(StubComponent::new(&["A", "B", "C", "D", "E"])));
        tui.render(); // hardware_cursor_row = 5

        // Change line 1 (index 1) → hardware_cursor_row moves to 2
        tui.root().clear();
        tui.root()
            .add_child(Box::new(StubComponent::new(&["A", "X", "C", "D", "E"])));
        tui.render();

        // Now change line 4 (index 4) → should move DOWN from row 2 to row 4
        tui.root().clear();
        tui.root()
            .add_child(Box::new(StubComponent::new(&["A", "X", "C", "D", "Z"])));
        tui.render();

        let last_write = mock_terminal(&tui).writes.last().unwrap();
        // Move down from row 2 to row 4 = 2 down
        assert!(
            last_write.contains("\x1b[2B"),
            "cursor moves down from hardware position"
        );
    }

    #[test]
    fn diff_first_render_no_clear_screen() {
        let mut tui: TUI<()> = TUI::new(Box::new(MockTerminal::new(80, 24)));
        tui.root()
            .add_child(Box::new(StubComponent::new(&["hello"])));
        tui.render();

        let output = mock_terminal(&tui).output();
        // First render should NOT contain clear-screen
        assert!(
            !output.contains("\x1b[3J"),
            "first render does not clear screen"
        );
        assert!(
            !output.contains("\x1b[2J"),
            "first render does not clear screen"
        );
    }

    #[test]
    fn diff_wrapped_in_sync_markers() {
        let mut tui: TUI<()> = TUI::new(Box::new(MockTerminal::new(80, 24)));
        tui.root()
            .add_child(Box::new(StubComponent::new(&["A", "B"])));
        tui.render();

        tui.root().clear();
        tui.root()
            .add_child(Box::new(StubComponent::new(&["A", "X"])));
        tui.render();

        let last_write = mock_terminal(&tui).writes.last().unwrap();
        assert!(
            last_write.starts_with("\x1b[?2026h"),
            "differential render starts with sync start"
        );
        assert!(
            last_write.ends_with("\x1b[?2026l"),
            "differential render ends with sync end"
        );
    }

    #[test]
    fn diff_exactly_one_write_per_changed_frame() {
        let mut tui: TUI<()> = TUI::new(Box::new(MockTerminal::new(80, 24)));
        tui.root()
            .add_child(Box::new(StubComponent::new(&["A", "B", "C"])));
        tui.render(); // 1 write

        tui.root().clear();
        tui.root()
            .add_child(Box::new(StubComponent::new(&["X", "Y", "Z"])));
        tui.render(); // 1 write (differential)

        assert_eq!(
            mock_terminal(&tui).writes.len(),
            2,
            "exactly one write per changed frame"
        );
    }

    #[test]
    fn diff_cursor_row_tracks_logical_end() {
        let mut tui: TUI<()> = TUI::new(Box::new(MockTerminal::new(80, 24)));
        tui.root()
            .add_child(Box::new(StubComponent::new(&["A", "B", "C"])));
        tui.render();
        assert_eq!(tui.cursor_row, 3);

        tui.root().clear();
        tui.root()
            .add_child(Box::new(StubComponent::new(&["A"])));
        tui.render();
        assert_eq!(tui.cursor_row, 1);
    }

    // ── Quit ────────────────────────────────────────────────────────

    #[test]
    fn quit_sets_should_quit() {
        let mut tui: TUI<()> = TUI::new(Box::new(MockTerminal::new(80, 24)));
        assert!(!tui.should_quit);
        tui.quit();
        assert!(tui.should_quit);
    }

    // ── Focus management ────────────────────────────────────────────

    #[test]
    fn focus_defaults_to_none() {
        let tui: TUI<()> = TUI::new(Box::new(MockTerminal::new(80, 24)));
        assert_eq!(tui.focused(), None);
    }

    #[test]
    fn set_focus_and_read_back() {
        let mut tui: TUI<()> = TUI::new(Box::new(MockTerminal::new(80, 24)));
        tui.set_focus(Some(0));
        assert_eq!(tui.focused(), Some(0));
        tui.set_focus(None);
        assert_eq!(tui.focused(), None);
    }

    // ── Async event loop (run) ──────────────────────────────────────

    #[tokio::test]
    async fn run_user_event_arrives() {
        let mut tui: TUI<String> = TUI::new(Box::new(MockTerminal::new(80, 24)));
        let tx = tui.event_tx();

        tokio::spawn(async move {
            tx.send("hello".to_string()).unwrap();
        });

        let mut received_user = false;
        tui.run(|event, tui| {
            if let Event::User(ref msg) = event {
                assert_eq!(msg, "hello");
                received_user = true;
            }
            tui.quit();
        })
        .await;

        assert!(received_user, "handler should receive user event");
    }

    #[tokio::test]
    async fn run_key_event_arrives() {
        let mut tui: TUI<()> = TUI::new(Box::new(MockTerminal::new(80, 24)));
        let ct_tx = tui.crossterm_event_tx();

        tokio::spawn(async move {
            let key = crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Char('a'),
                crossterm::event::KeyModifiers::NONE,
            );
            ct_tx.send(crossterm::event::Event::Key(key)).unwrap();
        });

        let mut received_key = false;
        tui.run(|event, tui| {
            if let Event::Key(key) = event {
                assert_eq!(key.code, crossterm::event::KeyCode::Char('a'));
                received_key = true;
            }
            tui.quit();
        })
        .await;

        assert!(received_key, "handler should receive key event");
    }

    #[tokio::test]
    async fn run_quit_breaks_loop() {
        let mut tui: TUI<String> = TUI::new(Box::new(MockTerminal::new(80, 24)));
        let tx = tui.event_tx();

        // Send multiple events; handler should quit on the first
        tokio::spawn(async move {
            tx.send("one".to_string()).unwrap();
            tx.send("two".to_string()).unwrap();
            tx.send("three".to_string()).unwrap();
        });

        let mut count = 0;
        tui.run(|event, tui| {
            if let Event::User(_) = event {
                count += 1;
            }
            tui.quit();
        })
        .await;

        assert_eq!(count, 1, "loop should break after quit on first event");
    }

    #[tokio::test]
    async fn run_calls_start_and_stop() {
        let mut tui: TUI<()> = TUI::new(Box::new(MockTerminal::new(80, 24)));
        let ct_tx = tui.crossterm_event_tx();

        tokio::spawn(async move {
            let key = crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Esc,
                crossterm::event::KeyModifiers::NONE,
            );
            ct_tx.send(crossterm::event::Event::Key(key)).unwrap();
        });

        tui.run(|_event, tui| {
            tui.quit();
        })
        .await;

        let mock = tui
            .terminal
            .as_any()
            .downcast_ref::<MockTerminal>()
            .unwrap();
        assert!(mock.started, "run() should call start()");
        assert!(mock.stopped, "run() should call stop()");
    }

    #[tokio::test]
    async fn run_renders_after_each_handler() {
        let mut tui: TUI<String> = TUI::new(Box::new(MockTerminal::new(80, 24)));
        tui.root()
            .add_child(Box::new(StubComponent::new(&["content"])));
        let tx = tui.event_tx();

        tokio::spawn(async move {
            tx.send("ev".to_string()).unwrap();
        });

        tui.run(|_event, tui| {
            tui.quit();
        })
        .await;

        let mock = tui
            .terminal
            .as_any()
            .downcast_ref::<MockTerminal>()
            .unwrap();
        // At least the initial render + one render after handler
        assert!(mock.writes.len() >= 1, "should render at least once");
    }

    #[tokio::test]
    async fn run_focus_forwards_key_to_component() {
        use std::sync::{Arc, Mutex};

        /// Component that records received key events.
        struct KeyTracker {
            keys: Arc<Mutex<Vec<crossterm::event::KeyEvent>>>,
        }

        impl Component for KeyTracker {
            fn render(&self, _width: u16) -> Vec<String> {
                vec![]
            }
            fn handle_input(&mut self, event: &crossterm::event::KeyEvent) {
                self.keys.lock().unwrap().push(*event);
            }
        }

        let keys = Arc::new(Mutex::new(Vec::new()));
        let mut tui: TUI<()> = TUI::new(Box::new(MockTerminal::new(80, 24)));
        tui.root().add_child(Box::new(KeyTracker {
            keys: keys.clone(),
        }));
        tui.set_focus(Some(0));

        let ct_tx = tui.crossterm_event_tx();
        tokio::spawn(async move {
            let key = crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Char('x'),
                crossterm::event::KeyModifiers::NONE,
            );
            ct_tx.send(crossterm::event::Event::Key(key)).unwrap();
        });

        tui.run(|_event, tui| {
            tui.quit();
        })
        .await;

        let received = keys.lock().unwrap();
        assert_eq!(received.len(), 1, "focused component should receive key");
        assert_eq!(
            received[0].code,
            crossterm::event::KeyCode::Char('x'),
            "correct key forwarded"
        );
    }

    #[tokio::test]
    async fn run_no_focus_no_forwarding() {
        use std::sync::{Arc, Mutex};

        struct KeyTracker {
            keys: Arc<Mutex<Vec<crossterm::event::KeyEvent>>>,
        }

        impl Component for KeyTracker {
            fn render(&self, _width: u16) -> Vec<String> {
                vec![]
            }
            fn handle_input(&mut self, event: &crossterm::event::KeyEvent) {
                self.keys.lock().unwrap().push(*event);
            }
        }

        let keys = Arc::new(Mutex::new(Vec::new()));
        let mut tui: TUI<()> = TUI::new(Box::new(MockTerminal::new(80, 24)));
        tui.root().add_child(Box::new(KeyTracker {
            keys: keys.clone(),
        }));
        // No focus set

        let ct_tx = tui.crossterm_event_tx();
        tokio::spawn(async move {
            let key = crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Char('x'),
                crossterm::event::KeyModifiers::NONE,
            );
            ct_tx.send(crossterm::event::Event::Key(key)).unwrap();
        });

        tui.run(|_event, tui| {
            tui.quit();
        })
        .await;

        let received = keys.lock().unwrap();
        assert_eq!(received.len(), 0, "without focus, no key forwarding");
    }

    #[tokio::test]
    async fn run_resize_event_arrives() {
        let mut tui: TUI<()> = TUI::new(Box::new(MockTerminal::new(80, 24)));
        let ct_tx = tui.crossterm_event_tx();

        tokio::spawn(async move {
            ct_tx
                .send(crossterm::event::Event::Resize(120, 40))
                .unwrap();
        });

        let mut received_resize = false;
        tui.run(|event, tui| {
            if let Event::Resize(w, h) = event {
                assert_eq!(w, 120);
                assert_eq!(h, 40);
                received_resize = true;
            }
            tui.quit();
        })
        .await;

        assert!(received_resize, "handler should receive resize event");
    }

    // ── Stop cursor repositioning (US-007a) ─────────────────────────

    #[test]
    fn stop_moves_cursor_to_end_after_differential_render() {
        let mut tui: TUI<()> = TUI::new(Box::new(MockTerminal::new(80, 24)));
        // Render 5 lines
        tui.root()
            .add_child(Box::new(StubComponent::new(&["A", "B", "C", "D", "E"])));
        tui.render(); // first render: hardware_cursor_row = 5, cursor_row = 5

        // Differential update: change only line 1 → hardware_cursor_row = 2
        tui.root().clear();
        tui.root()
            .add_child(Box::new(StubComponent::new(&["A", "X", "C", "D", "E"])));
        tui.render();
        // After this: hardware_cursor_row = 2, cursor_row = 5

        let writes_before_stop = mock_terminal(&tui).writes.len();
        tui.stop();

        let mock = mock_terminal(&tui);
        assert!(mock.stopped, "terminal.stop() was called");
        // stop() should have written cursor-down to move from row 2 to row 5
        assert!(
            mock.writes.len() > writes_before_stop,
            "stop() should write cursor movement"
        );
        let stop_write = &mock.writes[writes_before_stop];
        assert!(
            stop_write.contains("\x1b[3B"),
            "stop() should emit cursor-down 3 (from row 2 to row 5), got: {:?}",
            stop_write
        );
    }

    #[test]
    fn stop_no_cursor_movement_when_already_at_end() {
        let mut tui: TUI<()> = TUI::new(Box::new(MockTerminal::new(80, 24)));
        // First render: hardware_cursor_row == cursor_row (both at end)
        tui.root()
            .add_child(Box::new(StubComponent::new(&["A", "B", "C"])));
        tui.render();
        // After first render: hardware_cursor_row = 3, cursor_row = 3

        let writes_before_stop = mock_terminal(&tui).writes.len();
        tui.stop();

        let mock = mock_terminal(&tui);
        assert!(mock.stopped, "terminal.stop() was called");
        // No additional writes for cursor movement
        assert_eq!(
            mock.writes.len(),
            writes_before_stop,
            "stop() should not write cursor movement when already at end"
        );
    }

    // ── Overlay: splice_overlay_into_line ───────────────────────────

    #[test]
    fn splice_overlay_at_start() {
        let result = super::splice_overlay_into_line("hello world", 0, "XX", 2);
        // "XX" at col 0 replaces "he", rest is "llo world"
        assert_eq!(
            result,
            "\x1b[0mXX\x1b[0mllo world"
        );
    }

    #[test]
    fn splice_overlay_in_middle() {
        let result = super::splice_overlay_into_line("hello world", 3, "XX", 2);
        // before="hel", overlay="XX", after="world" (from col 5)
        assert_eq!(
            result,
            "hel\x1b[0mXX\x1b[0m world"
        );
    }

    #[test]
    fn splice_overlay_at_end() {
        let result = super::splice_overlay_into_line("hello", 3, "XX", 2);
        // before="hel", overlay="XX", after="" (col 5 = end of "hello")
        assert_eq!(
            result,
            "hel\x1b[0mXX\x1b[0m"
        );
    }

    #[test]
    fn splice_overlay_beyond_base() {
        let result = super::splice_overlay_into_line("hi", 5, "XX", 2);
        // before="hi", pad="   " (3 spaces to reach col 5), overlay="XX", after=""
        assert_eq!(
            result,
            "hi   \x1b[0mXX\x1b[0m"
        );
    }

    #[test]
    fn splice_overlay_on_empty_base() {
        let result = super::splice_overlay_into_line("", 3, "overlay", 7);
        // before="", pad="   ", overlay, after=""
        assert_eq!(
            result,
            "   \x1b[0moverlay\x1b[0m"
        );
    }

    // ── Overlay: calculate_overlay_position ─────────────────────────

    #[test]
    fn overlay_position_center() {
        let (row, col) = super::calculate_overlay_position(
            &OverlayOptions {
                width: 10,
                max_height: None,
                anchor: Anchor::Center,
                offset_x: 0,
                offset_y: 0,
            },
            80,  // content_width
            20,  // content_height
            10,  // overlay_width
            4,   // overlay_height
        );
        assert_eq!(row, 8);  // (20 - 4) / 2 = 8
        assert_eq!(col, 35); // (80 - 10) / 2 = 35
    }

    #[test]
    fn overlay_position_top_left() {
        let (row, col) = super::calculate_overlay_position(
            &OverlayOptions {
                width: 10,
                max_height: None,
                anchor: Anchor::TopLeft,
                offset_x: 2,
                offset_y: 1,
            },
            80, 20, 10, 4,
        );
        assert_eq!(row, 1); // 0 + offset_y
        assert_eq!(col, 2); // 0 + offset_x
    }

    #[test]
    fn overlay_position_bottom_right() {
        let (row, col) = super::calculate_overlay_position(
            &OverlayOptions {
                width: 10,
                max_height: None,
                anchor: Anchor::BottomRight,
                offset_x: 0,
                offset_y: 0,
            },
            80, 20, 10, 4,
        );
        assert_eq!(row, 16); // 20 - 4 = 16
        assert_eq!(col, 70); // 80 - 10 = 70
    }

    #[test]
    fn overlay_position_negative_offset_clamped() {
        let (row, col) = super::calculate_overlay_position(
            &OverlayOptions {
                width: 10,
                max_height: None,
                anchor: Anchor::TopLeft,
                offset_x: -5,
                offset_y: -5,
            },
            80, 20, 10, 4,
        );
        assert_eq!(row, 0); // clamped to 0
        assert_eq!(col, 0); // clamped to 0
    }

    // ── Overlay: TUI methods ────────────────────────────────────────

    #[test]
    fn show_overlay_returns_handle() {
        let mut tui: TUI<()> = TUI::new(Box::new(MockTerminal::new(80, 24)));
        let handle = tui.show_overlay(
            Box::new(StubComponent::new(&["popup"])),
            OverlayOptions {
                width: 20,
                max_height: None,
                anchor: Anchor::Center,
                offset_x: 0,
                offset_y: 0,
            },
        );
        assert!(tui.has_overlay());
        handle.hide();
        assert!(!tui.has_overlay());
    }

    #[test]
    fn has_overlay_false_initially() {
        let tui: TUI<()> = TUI::new(Box::new(MockTerminal::new(80, 24)));
        assert!(!tui.has_overlay());
    }

    #[test]
    fn hide_overlay_pops_topmost() {
        let mut tui: TUI<()> = TUI::new(Box::new(MockTerminal::new(80, 24)));
        tui.show_overlay(
            Box::new(StubComponent::new(&["popup"])),
            OverlayOptions {
                width: 20,
                max_height: None,
                anchor: Anchor::Center,
                offset_x: 0,
                offset_y: 0,
            },
        );
        assert!(tui.has_overlay());
        tui.hide_overlay();
        assert!(!tui.has_overlay());
    }

    #[test]
    fn hide_overlay_noop_when_empty() {
        let mut tui: TUI<()> = TUI::new(Box::new(MockTerminal::new(80, 24)));
        tui.hide_overlay(); // should not panic
        assert!(!tui.has_overlay());
    }

    #[test]
    fn overlay_handle_set_hidden_toggle() {
        let mut tui: TUI<()> = TUI::new(Box::new(MockTerminal::new(80, 24)));
        let handle = tui.show_overlay(
            Box::new(StubComponent::new(&["popup"])),
            OverlayOptions {
                width: 20,
                max_height: None,
                anchor: Anchor::Center,
                offset_x: 0,
                offset_y: 0,
            },
        );
        assert!(tui.has_overlay());
        handle.set_hidden(true);
        assert!(!tui.has_overlay());
        handle.set_hidden(false);
        assert!(tui.has_overlay());
    }

    // ── Overlay: compositing in render ──────────────────────────────

    #[test]
    fn overlay_composited_at_correct_position() {
        let mut tui: TUI<()> = TUI::new(Box::new(MockTerminal::new(20, 24)));
        // Base: 3 lines of 20 chars
        tui.root().add_child(Box::new(StubComponent::new(&[
            "aaaaaaaaaaaaaaaaaaaa",
            "bbbbbbbbbbbbbbbbbbbb",
            "cccccccccccccccccccc",
        ])));
        // Overlay: 1 line, 4 wide, at TopLeft (0,0) + offset (5,1)
        tui.show_overlay(
            Box::new(StubComponent::new(&["XXXX"])),
            OverlayOptions {
                width: 4,
                max_height: None,
                anchor: Anchor::TopLeft,
                offset_x: 5,
                offset_y: 1,
            },
        );
        tui.render();

        // Line 1 (row index 1) should have overlay at col 5
        let lines = tui.previous_lines();
        assert_eq!(lines.len(), 3);
        // Line 0 unchanged
        assert_eq!(lines[0], "aaaaaaaaaaaaaaaaaaaa");
        // Line 1: "bbbbb" + reset + "XXXX" + reset + "bbbbbbbbbbb" (from col 9)
        assert_eq!(
            lines[1],
            "bbbbb\x1b[0mXXXX\x1b[0mbbbbbbbbbbb"
        );
        // Line 2 unchanged
        assert_eq!(lines[2], "cccccccccccccccccccc");
    }

    #[test]
    fn overlay_center_position() {
        let mut tui: TUI<()> = TUI::new(Box::new(MockTerminal::new(20, 24)));
        // Base: 5 lines
        tui.root().add_child(Box::new(StubComponent::new(&[
            "12345678901234567890",
            "12345678901234567890",
            "12345678901234567890",
            "12345678901234567890",
            "12345678901234567890",
        ])));
        // Overlay: 1 line, 4 wide, centered
        tui.show_overlay(
            Box::new(StubComponent::new(&["XXXX"])),
            OverlayOptions {
                width: 4,
                max_height: None,
                anchor: Anchor::Center,
                offset_x: 0,
                offset_y: 0,
            },
        );
        tui.render();

        let lines = tui.previous_lines();
        // Center row: (5-1)/2 = 2, center col: (20-4)/2 = 8
        // Lines 0,1,3,4 unchanged
        assert_eq!(lines[0], "12345678901234567890");
        assert_eq!(lines[1], "12345678901234567890");
        // Line 2: "12345678" + reset + "XXXX" + reset + "34567890"
        assert_eq!(
            lines[2],
            "12345678\x1b[0mXXXX\x1b[0m34567890"
        );
        assert_eq!(lines[3], "12345678901234567890");
        assert_eq!(lines[4], "12345678901234567890");
    }

    #[test]
    fn overlay_extends_base_lines_if_needed() {
        let mut tui: TUI<()> = TUI::new(Box::new(MockTerminal::new(20, 24)));
        // Base: 1 line
        tui.root().add_child(Box::new(StubComponent::new(&["base"])));
        // Overlay at row 2 (beyond base)
        tui.show_overlay(
            Box::new(StubComponent::new(&["overlay"])),
            OverlayOptions {
                width: 7,
                max_height: None,
                anchor: Anchor::TopLeft,
                offset_x: 0,
                offset_y: 2,
            },
        );
        tui.render();

        let lines = tui.previous_lines();
        assert_eq!(lines.len(), 3); // extended from 1 to 3
        assert_eq!(lines[0], "base");
        assert_eq!(lines[1], ""); // empty padding line
        assert_eq!(lines[2], "\x1b[0moverlay\x1b[0m");
    }

    #[test]
    fn overlay_max_height_truncates() {
        let mut tui: TUI<()> = TUI::new(Box::new(MockTerminal::new(20, 24)));
        tui.root().add_child(Box::new(StubComponent::new(&[
            "aaaaaaaaaa",
            "bbbbbbbbbb",
            "cccccccccc",
            "dddddddddd",
        ])));
        // Overlay: 3 lines, but max_height=1
        tui.show_overlay(
            Box::new(StubComponent::new(&["XX", "YY", "ZZ"])),
            OverlayOptions {
                width: 2,
                max_height: Some(1),
                anchor: Anchor::TopLeft,
                offset_x: 0,
                offset_y: 0,
            },
        );
        tui.render();

        let lines = tui.previous_lines();
        // Only first overlay line should be composited
        assert_eq!(lines[0], "\x1b[0mXX\x1b[0maaaaaaaa");
        assert_eq!(lines[1], "bbbbbbbbbb"); // unchanged
    }

    #[test]
    fn hidden_overlay_not_composited() {
        let mut tui: TUI<()> = TUI::new(Box::new(MockTerminal::new(20, 24)));
        tui.root().add_child(Box::new(StubComponent::new(&["base content"])));
        let handle = tui.show_overlay(
            Box::new(StubComponent::new(&["popup"])),
            OverlayOptions {
                width: 5,
                max_height: None,
                anchor: Anchor::TopLeft,
                offset_x: 0,
                offset_y: 0,
            },
        );
        handle.hide();
        tui.render();

        let lines = tui.previous_lines();
        assert_eq!(lines[0], "base content"); // unchanged, overlay hidden
    }

    // ── Overlay: focus save/restore ─────────────────────────────────

    #[test]
    fn overlay_saves_and_restores_focus() {
        let mut tui: TUI<()> = TUI::new(Box::new(MockTerminal::new(80, 24)));
        tui.root().add_child(Box::new(StubComponent::new(&["child0"])));
        tui.root().add_child(Box::new(StubComponent::new(&["child1"])));
        tui.set_focus(Some(1));
        assert_eq!(tui.focused(), Some(1));

        // Show overlay — saves focus
        tui.show_overlay(
            Box::new(StubComponent::new(&["popup"])),
            OverlayOptions {
                width: 20,
                max_height: None,
                anchor: Anchor::Center,
                offset_x: 0,
                offset_y: 0,
            },
        );

        // Change focus (as overlay handler might)
        tui.set_focus(Some(0));
        assert_eq!(tui.focused(), Some(0));

        // Hide overlay — restores saved focus
        tui.hide_overlay();
        assert_eq!(tui.focused(), Some(1));
    }

    #[test]
    fn nested_overlays_restore_focus_correctly() {
        let mut tui: TUI<()> = TUI::new(Box::new(MockTerminal::new(80, 24)));
        tui.set_focus(Some(0));

        // Show first overlay — saves focus=Some(0)
        tui.show_overlay(
            Box::new(StubComponent::new(&["popup1"])),
            OverlayOptions {
                width: 20,
                max_height: None,
                anchor: Anchor::Center,
                offset_x: 0,
                offset_y: 0,
            },
        );
        tui.set_focus(Some(1)); // change focus while overlay is up

        // Show second overlay — saves focus=Some(1)
        tui.show_overlay(
            Box::new(StubComponent::new(&["popup2"])),
            OverlayOptions {
                width: 20,
                max_height: None,
                anchor: Anchor::Center,
                offset_x: 0,
                offset_y: 0,
            },
        );

        // Hide second overlay — restores to Some(1)
        tui.hide_overlay();
        assert_eq!(tui.focused(), Some(1));

        // Hide first overlay — restores to Some(0)
        tui.hide_overlay();
        assert_eq!(tui.focused(), Some(0));
    }

    // ── Overlay: input forwarding (overlay stack) ───────────────────

    #[tokio::test]
    async fn overlay_topmost_gets_input() {
        use std::sync::{Arc, Mutex};

        /// Component that records received key events.
        struct KeyRecorder {
            keys: Arc<Mutex<Vec<crossterm::event::KeyCode>>>,
        }

        impl Component for KeyRecorder {
            fn render(&self, _width: u16) -> Vec<String> {
                vec!["recorder".to_string()]
            }
            fn handle_input(&mut self, event: &crossterm::event::KeyEvent) {
                self.keys.lock().unwrap().push(event.code);
            }
        }

        let root_keys = Arc::new(Mutex::new(Vec::new()));
        let overlay_keys = Arc::new(Mutex::new(Vec::new()));

        let mut tui: TUI<()> = TUI::new(Box::new(MockTerminal::new(80, 24)));
        tui.root().add_child(Box::new(KeyRecorder {
            keys: root_keys.clone(),
        }));
        tui.set_focus(Some(0));

        // Show overlay with its own key recorder
        tui.show_overlay(
            Box::new(KeyRecorder {
                keys: overlay_keys.clone(),
            }),
            OverlayOptions {
                width: 20,
                max_height: None,
                anchor: Anchor::Center,
                offset_x: 0,
                offset_y: 0,
            },
        );

        let ct_tx = tui.crossterm_event_tx();
        tokio::spawn(async move {
            let key = crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Char('x'),
                crossterm::event::KeyModifiers::NONE,
            );
            ct_tx.send(crossterm::event::Event::Key(key)).unwrap();
        });

        tui.run(|_event, tui| {
            tui.quit();
        })
        .await;

        // Overlay got the key, root did not
        assert_eq!(overlay_keys.lock().unwrap().len(), 1);
        assert_eq!(root_keys.lock().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn overlay_hidden_falls_through_to_focused() {
        use std::sync::{Arc, Mutex};

        struct KeyRecorder {
            keys: Arc<Mutex<Vec<crossterm::event::KeyCode>>>,
        }

        impl Component for KeyRecorder {
            fn render(&self, _width: u16) -> Vec<String> {
                vec!["recorder".to_string()]
            }
            fn handle_input(&mut self, event: &crossterm::event::KeyEvent) {
                self.keys.lock().unwrap().push(event.code);
            }
        }

        let root_keys = Arc::new(Mutex::new(Vec::new()));

        let mut tui: TUI<()> = TUI::new(Box::new(MockTerminal::new(80, 24)));
        tui.root().add_child(Box::new(KeyRecorder {
            keys: root_keys.clone(),
        }));
        tui.set_focus(Some(0));

        // Show overlay, then hide it
        let handle = tui.show_overlay(
            Box::new(StubComponent::new(&["popup"])),
            OverlayOptions {
                width: 20,
                max_height: None,
                anchor: Anchor::Center,
                offset_x: 0,
                offset_y: 0,
            },
        );
        handle.hide();

        let ct_tx = tui.crossterm_event_tx();
        tokio::spawn(async move {
            let key = crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Char('y'),
                crossterm::event::KeyModifiers::NONE,
            );
            ct_tx.send(crossterm::event::Event::Key(key)).unwrap();
        });

        tui.run(|_event, tui| {
            tui.quit();
        })
        .await;

        // Root component got the key (overlay was hidden)
        assert_eq!(root_keys.lock().unwrap().len(), 1);
        assert_eq!(
            root_keys.lock().unwrap()[0],
            crossterm::event::KeyCode::Char('y')
        );
    }

    #[tokio::test]
    async fn overlay_stack_topmost_visible_gets_input() {
        use std::sync::{Arc, Mutex};

        struct KeyRecorder {
            keys: Arc<Mutex<Vec<crossterm::event::KeyCode>>>,
        }

        impl Component for KeyRecorder {
            fn render(&self, _width: u16) -> Vec<String> {
                vec!["recorder".to_string()]
            }
            fn handle_input(&mut self, event: &crossterm::event::KeyEvent) {
                self.keys.lock().unwrap().push(event.code);
            }
        }

        let overlay1_keys = Arc::new(Mutex::new(Vec::new()));
        let overlay2_keys = Arc::new(Mutex::new(Vec::new()));

        let mut tui: TUI<()> = TUI::new(Box::new(MockTerminal::new(80, 24)));

        // Show two overlays
        tui.show_overlay(
            Box::new(KeyRecorder {
                keys: overlay1_keys.clone(),
            }),
            OverlayOptions {
                width: 20,
                max_height: None,
                anchor: Anchor::Center,
                offset_x: 0,
                offset_y: 0,
            },
        );
        tui.show_overlay(
            Box::new(KeyRecorder {
                keys: overlay2_keys.clone(),
            }),
            OverlayOptions {
                width: 20,
                max_height: None,
                anchor: Anchor::Center,
                offset_x: 0,
                offset_y: 0,
            },
        );

        let ct_tx = tui.crossterm_event_tx();
        tokio::spawn(async move {
            let key = crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Char('z'),
                crossterm::event::KeyModifiers::NONE,
            );
            ct_tx.send(crossterm::event::Event::Key(key)).unwrap();
        });

        tui.run(|_event, tui| {
            tui.quit();
        })
        .await;

        // Only topmost (overlay2) got the key
        assert_eq!(overlay2_keys.lock().unwrap().len(), 1);
        assert_eq!(overlay1_keys.lock().unwrap().len(), 0);
    }
}
