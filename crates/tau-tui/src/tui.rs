// TUI engine with rendering and async event loop.

use std::fmt::Write;
use tokio::sync::mpsc::{self, UnboundedSender, UnboundedReceiver};

use crate::component::{Component, Container};
use crate::terminal::Terminal;

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
    #[allow(dead_code)] // consumed by the run loop in US-008
    event_rx: Option<UnboundedReceiver<E>>,
}

impl<E: Send + 'static> TUI<E> {
    /// Create a new TUI engine wrapping the given terminal.
    pub fn new(terminal: Box<dyn Terminal>) -> Self {
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        TUI {
            terminal,
            root: Container::new(),
            previous_lines: Vec::new(),
            previous_width: 0,
            cursor_row: 0,
            hardware_cursor_row: 0,
            event_tx,
            event_rx: Some(event_rx),
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

    /// Start the terminal (enable raw mode, hide cursor).
    pub fn start(&mut self) {
        self.terminal.start();
    }

    /// Stop the terminal (show cursor, disable raw mode).
    /// Cursor is already past content after render (each line ends with \r\n).
    pub fn stop(&mut self) {
        self.terminal.stop();
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
        let lines = self.root.render(width);

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
}
