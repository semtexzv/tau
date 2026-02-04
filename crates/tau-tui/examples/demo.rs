//! Demo application demonstrating all tau-tui components and async user events.
//!
//! Run with: `cargo run -p tau-tui --example demo`
//!
//! Features demonstrated:
//! - Text with styled ANSI content
//! - Box with background color
//! - Input that echoes typed text on Enter
//! - Async user events: a timer thread sends tick events every second
//! - SelectList overlay triggered by Ctrl+P
//! - Focus switching between Input and SelectList
//! - Quit with Ctrl+C or Escape (when no overlay visible)

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use crossterm::event::{KeyCode, KeyModifiers};

use tau_tui::component::Component;
use tau_tui::components::{BoxComponent, Input, SelectItem, SelectList, Spacer, Text};
use tau_tui::terminal::CrosstermTerminal;
use tau_tui::tui::{Anchor, Event, OverlayOptions, TUI};
use tau_tui::utils::visible_width;

/// Custom user events for the demo application.
#[derive(Debug)]
enum DemoEvent {
    /// Timer tick with the elapsed seconds count.
    Tick(u64),
}

// ── Custom components with shared state ─────────────────────────────

/// Displays a live counter that reads from shared state on each render.
struct CounterDisplay {
    counter: Rc<Cell<u64>>,
}

impl CounterDisplay {
    fn new(counter: Rc<Cell<u64>>) -> Self {
        CounterDisplay { counter }
    }
}

impl Component for CounterDisplay {
    fn render(&self, width: u16) -> Vec<String> {
        let count = self.counter.get();
        let text = format!("⏱  Timer: {} second{} elapsed", count, if count == 1 { "" } else { "s" });
        let vis = visible_width(&text);
        let pad = (width as usize).saturating_sub(vis);
        vec![format!("{}{}", text, " ".repeat(pad))]
    }
}

/// Displays the last echoed text, reading from shared state.
struct EchoDisplay {
    text: Rc<RefCell<String>>,
}

impl EchoDisplay {
    fn new(text: Rc<RefCell<String>>) -> Self {
        EchoDisplay { text }
    }
}

impl Component for EchoDisplay {
    fn render(&self, width: u16) -> Vec<String> {
        let text = self.text.borrow();
        if text.is_empty() {
            return vec![];
        }
        let line = format!("\x1b[32m> {}\x1b[0m", &*text);
        let vis = visible_width(&line);
        let pad = (width as usize).saturating_sub(vis);
        vec![format!("{}{}", line, " ".repeat(pad))]
    }
}

// ── Main ────────────────────────────────────────────────────────────

#[tokio::main(flavor = "current_thread")]
async fn main() {
    // Shared state for dynamic components
    let counter = Rc::new(Cell::new(0u64));
    let echo = Rc::new(RefCell::new(String::new()));

    // Create TUI with real terminal
    let mut tui: TUI<DemoEvent> = TUI::new(Box::new(CrosstermTerminal::new()));

    // ── Build component tree ────────────────────────────────────────

    // 0: Header with styled text
    tui.root().add_child(Box::new(Text::new(
        "\x1b[1m\x1b[36m═══ tau-tui Demo ═══\x1b[0m",
        1,
        1,
    )));

    // 1: Counter in a dark box
    let mut counter_box = BoxComponent::new(1, 0);
    counter_box.set_bg("\x1b[48;5;236m");
    counter_box.add_child(Box::new(CounterDisplay::new(counter.clone())));
    tui.root().add_child(Box::new(counter_box));

    // 2: Spacer
    tui.root().add_child(Box::new(Spacer::new(1)));

    // 3: Instructions
    tui.root().add_child(Box::new(Text::new(
        "Type text and press \x1b[1mEnter\x1b[0m to echo. \x1b[33mCtrl+P\x1b[0m opens command palette. \x1b[31mEsc\x1b[0m or \x1b[31mCtrl+C\x1b[0m to quit.",
        1,
        0,
    )));

    // 4: Echo display (reads from shared state)
    tui.root()
        .add_child(Box::new(EchoDisplay::new(echo.clone())));

    // 5: Spacer before input
    tui.root().add_child(Box::new(Spacer::new(1)));

    // 6: Input component
    let echo_for_input = echo.clone();
    let mut input = Input::new();
    input.on_submit = Some(Box::new(move |text: &str| {
        *echo_for_input.borrow_mut() = text.to_string();
    }));
    tui.root().add_child(Box::new(input));

    // Focus the Input (index 6)
    tui.set_focus(Some(6));

    // ── Spawn timer task ────────────────────────────────────────────
    // Uses a plain thread + std::thread::sleep (no tokio::time needed).
    // Sends DemoEvent::Tick via event_tx every second.
    let tx = tui.event_tx();
    std::thread::spawn(move || {
        let mut count = 0u64;
        loop {
            std::thread::sleep(std::time::Duration::from_secs(1));
            count += 1;
            if tx.send(DemoEvent::Tick(count)).is_err() {
                break; // TUI was dropped, exit
            }
        }
    });

    // ── Event loop ──────────────────────────────────────────────────
    let counter_in_handler = counter.clone();
    let echo_in_handler = echo.clone();

    tui.run(|event, tui| {
        match event {
            Event::User(DemoEvent::Tick(n)) => {
                // Update shared counter; CounterDisplay reads it on next render
                counter_in_handler.set(n);
            }
            Event::Key(key) => {
                if tui.has_overlay() {
                    // Overlay is visible: Enter selects, Esc cancels — both close overlay
                    match key.code {
                        KeyCode::Enter | KeyCode::Esc => {
                            tui.hide_overlay();
                        }
                        _ => {} // Up/Down handled by SelectList
                    }
                } else {
                    match (key.code, key.modifiers) {
                        (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                            tui.quit();
                        }
                        (KeyCode::Esc, _) => {
                            tui.quit();
                        }
                        (KeyCode::Char('p'), KeyModifiers::CONTROL) => {
                            // Show command palette (SelectList overlay)
                            let echo_for_select = echo_in_handler.clone();
                            let mut list = SelectList::new(
                                vec![
                                    SelectItem::new("rust", "Rust"),
                                    SelectItem::with_description("go", "Go", "Google's language"),
                                    SelectItem::new("python", "Python"),
                                    SelectItem::with_description(
                                        "ts",
                                        "TypeScript",
                                        "JavaScript but typed",
                                    ),
                                    SelectItem::new("zig", "Zig"),
                                    SelectItem::with_description(
                                        "c",
                                        "C",
                                        "Close to the metal",
                                    ),
                                    SelectItem::new("haskell", "Haskell"),
                                ],
                                5,
                            );
                            list.on_select = Some(Box::new(move |item: &SelectItem| {
                                *echo_for_select.borrow_mut() =
                                    format!("Selected: {}", item.label);
                            }));
                            tui.show_overlay(
                                Box::new(list),
                                OverlayOptions {
                                    width: 35,
                                    max_height: Some(7),
                                    anchor: Anchor::Center,
                                    offset_x: 0,
                                    offset_y: 0,
                                },
                            );
                        }
                        _ => {} // Other keys handled by focused Input
                    }
                }
            }
            Event::Resize(_, _) => {
                // Resize handled automatically by TUI re-render
            }
        }
    })
    .await;
}
