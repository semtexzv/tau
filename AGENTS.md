# AGENTS.md — Reusable Patterns for rtui

## Trait Object Downcasting
- Terminal trait has `as_any(&self) -> &dyn Any` and `as_any_mut(&mut self) -> &mut dyn Any`
- In tests, downcast via: `tui.terminal.as_any().downcast_ref::<MockTerminal>().unwrap()`
- Standard Rust pattern — use this whenever trait objects need test inspection

## Testing with MockTerminal
- `MockTerminal::new(cols, rows)` — captures all writes to `pub writes: Vec<String>`
- `mock.output()` — concatenates all writes for full output inspection
- `mock.writes.len()` — verify number of write calls (should be 1 per render frame)
- Access via TUI: use `as_any()` downcasting from private `terminal` field (tests in same module can access)

## TUI Rendering Model
- Scrolling model (not alternate screen) — content grows downward
- Each line gets `\x1b[0m\r\n` suffix — reset prevents style bleeding, \r\n positions cursor on next line
- Buffer wrapped in synchronized output: `\x1b[?2026h` ... `\x1b[?2026l`
- Single `write()` + `flush()` per render frame — never multiple writes
- `previous_lines` and `previous_width` stored for differential rendering

## Component Pattern
- `Component` trait: `render(width) -> Vec<String>` is the only required method
- Components are `Box<dyn Component>` in containers — object-safe
- Components don't know about user event type `E` — only TUI is generic over it

## Tokio Channels
- `unbounded_channel()` can be created without runtime; only `recv()` needs async
- `event_rx` is `Option<>` so the run loop can `.take()` it
