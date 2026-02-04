// TUI engine with rendering and async event loop.

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
    /// Lines from the most recent render — used for differential rendering in US-007.
    previous_lines: Vec<String>,
    /// Terminal width from the most recent render — width change triggers full redraw.
    previous_width: u16,
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

    /// Render the component tree to the terminal.
    ///
    /// Builds a single `String` buffer with synchronized output markers,
    /// all lines with ANSI reset appended, then writes + flushes once.
    pub fn render(&mut self) {
        let (width, _height) = self.terminal.size();
        let lines = self.root.render(width);

        let mut buffer = String::new();

        // Start synchronized output (DEC mode 2026)
        buffer.push_str("\x1b[?2026h");

        // Write all lines, each with reset + newline
        for line in &lines {
            buffer.push_str(line);
            buffer.push_str("\x1b[0m\r\n");
        }

        // End synchronized output
        buffer.push_str("\x1b[?2026l");

        // Single write + flush per frame
        self.terminal.write(&buffer);
        self.terminal.flush();

        // Store for future differential rendering
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
    fn render_empty_produces_sync_markers_only() {
        let mut tui: TUI<()> = TUI::new(Box::new(MockTerminal::new(80, 24)));
        tui.render();
        let mock = mock_terminal(&tui);
        assert_eq!(mock.writes.len(), 1, "exactly one write call");
        let output = mock.output();
        assert_eq!(output, "\x1b[?2026h\x1b[?2026l");
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
    fn multiple_renders_accumulate_writes() {
        let mut tui: TUI<()> = TUI::new(Box::new(MockTerminal::new(80, 24)));
        tui.root()
            .add_child(Box::new(StubComponent::new(&["hello"])));
        tui.render();
        tui.render();
        // Each render produces exactly one write, so two renders = two writes
        assert_eq!(mock_terminal(&tui).writes.len(), 2);
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
}
