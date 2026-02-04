// Single-line text input with cursor, horizontal scrolling, and editing keybindings.

use std::cell::Cell;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use unicode_width::UnicodeWidthChar;

use crate::component::Component;

/// A single-line text input component with cursor, editing, and horizontal scrolling.
///
/// Displays a `"> "` prompt followed by the current text. When focused, shows an
/// inverse-video cursor at the cursor position. Supports basic Emacs-style keybindings.
pub struct Input {
    /// The current text content.
    buffer: String,
    /// Cursor position as a character index (0 = before first char).
    cursor: usize,
    /// Whether this input currently has focus (renders cursor only when focused).
    pub focused: bool,
    /// Horizontal scroll offset (character index of the first visible char after prompt).
    /// Uses Cell so render(&self) can update it for smooth scrolling.
    scroll_offset: Cell<usize>,
    /// Callback invoked when Enter is pressed. Receives the current value.
    pub on_submit: Option<Box<dyn FnMut(&str)>>,
    /// Callback invoked when Escape is pressed.
    pub on_escape: Option<Box<dyn FnMut()>>,
}

const PROMPT: &str = "> ";
const PROMPT_WIDTH: usize = 2;

impl Input {
    /// Create a new empty Input.
    pub fn new() -> Self {
        Input {
            buffer: String::new(),
            cursor: 0,
            focused: true,
            scroll_offset: Cell::new(0),
            on_submit: None,
            on_escape: None,
        }
    }

    /// Get the current text content.
    pub fn value(&self) -> &str {
        &self.buffer
    }

    /// Set the text content and reset cursor to the end.
    pub fn set_value(&mut self, s: &str) {
        self.buffer = s.to_string();
        self.cursor = self.char_count();
        self.scroll_offset.set(0);
    }

    /// Number of characters in the buffer.
    fn char_count(&self) -> usize {
        self.buffer.chars().count()
    }

    /// Get the byte offset for a given character index.
    fn char_to_byte(&self, char_idx: usize) -> usize {
        self.buffer
            .char_indices()
            .nth(char_idx)
            .map(|(i, _)| i)
            .unwrap_or(self.buffer.len())
    }

    /// Insert a character at the current cursor position.
    fn insert_char(&mut self, c: char) {
        let byte_pos = self.char_to_byte(self.cursor);
        self.buffer.insert(byte_pos, c);
        self.cursor += 1;
    }

    /// Delete the character before the cursor (backspace).
    fn delete_backward(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
            let byte_pos = self.char_to_byte(self.cursor);
            let next_byte = self.char_to_byte(self.cursor + 1);
            self.buffer.drain(byte_pos..next_byte);
        }
    }

    /// Delete the character at the cursor (delete key).
    fn delete_forward(&mut self) {
        let count = self.char_count();
        if self.cursor < count {
            let byte_pos = self.char_to_byte(self.cursor);
            let next_byte = self.char_to_byte(self.cursor + 1);
            self.buffer.drain(byte_pos..next_byte);
        }
    }

    /// Delete the word before the cursor (Ctrl+Backspace).
    fn delete_word_backward(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let old_cursor = self.cursor;
        self.move_word_backward();
        let new_cursor = self.cursor;
        let start_byte = self.char_to_byte(new_cursor);
        let end_byte = self.char_to_byte(old_cursor);
        self.buffer.drain(start_byte..end_byte);
    }

    /// Delete from cursor to start of line (Ctrl+U).
    fn delete_to_start(&mut self) {
        let byte_pos = self.char_to_byte(self.cursor);
        self.buffer.drain(..byte_pos);
        self.cursor = 0;
    }

    /// Delete from cursor to end of line (Ctrl+K).
    fn delete_to_end(&mut self) {
        let byte_pos = self.char_to_byte(self.cursor);
        self.buffer.truncate(byte_pos);
    }

    /// Move cursor one word backward (Ctrl+Left).
    fn move_word_backward(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let chars: Vec<char> = self.buffer.chars().collect();
        let mut pos = self.cursor;

        // Skip any spaces before the cursor
        while pos > 0 && chars[pos - 1] == ' ' {
            pos -= 1;
        }
        // Skip non-space characters (the word)
        while pos > 0 && chars[pos - 1] != ' ' {
            pos -= 1;
        }
        self.cursor = pos;
    }

    /// Move cursor one word forward (Ctrl+Right).
    fn move_word_forward(&mut self) {
        let chars: Vec<char> = self.buffer.chars().collect();
        let count = chars.len();
        let mut pos = self.cursor;

        // Skip non-space characters (the word)
        while pos < count && chars[pos] != ' ' {
            pos += 1;
        }
        // Skip spaces after the word
        while pos < count && chars[pos] == ' ' {
            pos += 1;
        }
        self.cursor = pos;
    }

}

impl Default for Input {
    fn default() -> Self {
        Self::new()
    }
}

impl Component for Input {
    fn render(&self, width: u16) -> Vec<String> {
        let total_width = width as usize;
        if total_width <= PROMPT_WIDTH {
            return vec![" ".repeat(total_width)];
        }
        let available = total_width - PROMPT_WIDTH;
        let chars: Vec<char> = self.buffer.chars().collect();

        // Compute scroll offset using column widths
        let scroll = compute_scroll(self.cursor, self.scroll_offset.get(), available, &chars);
        self.scroll_offset.set(scroll);

        // Compute visible_end by walking from scroll, summing column widths
        let mut visible_end = scroll;
        let mut vis_cols: usize = 0;
        while visible_end < chars.len() {
            let w = char_col_width(chars[visible_end]);
            if vis_cols + w > available {
                break;
            }
            vis_cols += w;
            visible_end += 1;
        }

        // Build output line
        let mut line = String::with_capacity(total_width + 20);
        line.push_str(PROMPT);

        if self.focused {
            // Chars before cursor
            let before: String = chars[scroll..self.cursor].iter().collect();
            line.push_str(&before);

            // Cursor character (inverse video)
            let cursor_char = if self.cursor < chars.len() {
                chars[self.cursor].to_string()
            } else {
                " ".to_string() // cursor past end of text
            };
            line.push_str("\x1b[7m"); // inverse
            line.push_str(&cursor_char);
            line.push_str("\x1b[27m"); // reset inverse

            // Chars after cursor
            let after_start = (self.cursor + 1).min(visible_end);
            if after_start < visible_end {
                let after: String = chars[after_start..visible_end].iter().collect();
                line.push_str(&after);
            }

            // Pad to full width using actual column widths
            let cursor_extra = if self.cursor >= chars.len() { 1 } else { 0 };
            let content_cols = PROMPT_WIDTH + vis_cols + cursor_extra;
            let pad = total_width.saturating_sub(content_cols);
            for _ in 0..pad {
                line.push(' ');
            }
        } else {
            // Not focused: no cursor shown
            let visible_chars: String = chars[scroll..visible_end].iter().collect();
            line.push_str(&visible_chars);
            let pad = total_width.saturating_sub(PROMPT_WIDTH + vis_cols);
            for _ in 0..pad {
                line.push(' ');
            }
        }

        vec![line]
    }

    fn handle_input(&mut self, event: &KeyEvent) {
        let modifiers = event.modifiers;
        let ctrl = modifiers.contains(KeyModifiers::CONTROL);

        match event.code {
            // Cursor movement
            KeyCode::Left if ctrl => self.move_word_backward(),
            KeyCode::Right if ctrl => self.move_word_forward(),
            KeyCode::Left => {
                if self.cursor > 0 {
                    self.cursor -= 1;
                }
            }
            KeyCode::Right => {
                if self.cursor < self.char_count() {
                    self.cursor += 1;
                }
            }
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = self.char_count(),

            // Editing
            KeyCode::Backspace if ctrl => self.delete_word_backward(),
            KeyCode::Backspace => self.delete_backward(),
            KeyCode::Delete => self.delete_forward(),
            KeyCode::Char('u') if ctrl => self.delete_to_start(),
            KeyCode::Char('k') if ctrl => self.delete_to_end(),

            // Character insertion
            KeyCode::Char(c) if !ctrl => self.insert_char(c),

            // Callbacks
            KeyCode::Enter => {
                if let Some(ref mut cb) = self.on_submit {
                    let val = self.buffer.clone();
                    cb(&val);
                }
            }
            KeyCode::Esc => {
                if let Some(ref mut cb) = self.on_escape {
                    cb();
                }
            }

            _ => {}
        }

        // After any input, update scroll offset.
        // We need to know the available width, but handle_input doesn't get width.
        // We'll adjust scroll lazily in render() instead. Just ensure cursor is valid.
        let count = self.char_count();
        if self.cursor > count {
            self.cursor = count;
        }
    }
}

/// Column width of a single character.
fn char_col_width(c: char) -> usize {
    UnicodeWidthChar::width(c).unwrap_or(0)
}

/// Total column width of a character slice.
fn chars_col_width(chars: &[char]) -> usize {
    chars.iter().map(|c| char_col_width(*c)).sum()
}

/// Pure function to compute scroll offset (char index) so the cursor is visible
/// within `available` terminal columns. All width comparisons use actual column widths.
fn compute_scroll(cursor: usize, current_offset: usize, available: usize, chars: &[char]) -> usize {
    if available == 0 {
        return 0;
    }
    let mut offset = current_offset.min(chars.len());

    // Scroll left if cursor moved before visible window
    if cursor < offset {
        offset = cursor;
    }

    // Scroll right if cursor doesn't fit in available columns.
    // We need: cols(chars[offset..cursor]) + cursor_char_width <= available
    loop {
        let cols_before = chars_col_width(&chars[offset..cursor]);
        let cursor_w = if cursor < chars.len() {
            char_col_width(chars[cursor]).max(1)
        } else {
            1 // space cursor at end of text
        };

        if cols_before + cursor_w <= available {
            break;
        }
        if offset < cursor {
            offset += 1;
        } else {
            break;
        }
    }

    offset
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::visible_width;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl_key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::CONTROL)
    }

    fn char_key(c: char) -> KeyEvent {
        key(KeyCode::Char(c))
    }

    // === Rendering tests ===

    #[test]
    fn initial_render_shows_prompt_with_cursor() {
        let input = Input::new();
        let lines = input.render(20);
        assert_eq!(lines.len(), 1);
        let line = &lines[0];
        // Should start with "> "
        assert!(line.starts_with("> "));
        // Should contain inverse video for cursor
        assert!(line.contains("\x1b[7m"));
        assert!(line.contains("\x1b[27m"));
        // Cursor should be a space (empty buffer)
        assert!(line.contains("\x1b[7m \x1b[27m"));
    }

    #[test]
    fn render_after_typing_abc() {
        let mut input = Input::new();
        input.handle_input(&char_key('a'));
        input.handle_input(&char_key('b'));
        input.handle_input(&char_key('c'));
        assert_eq!(input.value(), "abc");

        let lines = input.render(20);
        let line = &lines[0];
        assert!(line.starts_with("> abc"));
        // Cursor should be past 'c' — inverse space
        assert!(line.contains("\x1b[7m \x1b[27m"));
    }

    #[test]
    fn render_unfocused_no_cursor() {
        let mut input = Input::new();
        input.set_value("hello");
        input.focused = false;

        let lines = input.render(20);
        let line = &lines[0];
        assert!(line.starts_with("> hello"));
        // Should NOT contain inverse video
        assert!(!line.contains("\x1b[7m"));
    }

    #[test]
    fn render_pads_to_full_width() {
        let input = Input::new();
        let lines = input.render(20);
        // visible_width should be 20 (ANSI codes don't count)
        assert_eq!(visible_width(&lines[0]), 20);
    }

    #[test]
    fn render_narrow_width() {
        let input = Input::new();
        let lines = input.render(2);
        // Width <= PROMPT_WIDTH: just spaces
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0], "  ");
    }

    // === Editing tests ===

    #[test]
    fn backspace_removes_last_char() {
        let mut input = Input::new();
        input.handle_input(&char_key('a'));
        input.handle_input(&char_key('b'));
        input.handle_input(&char_key('c'));
        input.handle_input(&key(KeyCode::Backspace));
        assert_eq!(input.value(), "ab");
        assert_eq!(input.cursor, 2);
    }

    #[test]
    fn backspace_at_start_does_nothing() {
        let mut input = Input::new();
        input.handle_input(&key(KeyCode::Backspace));
        assert_eq!(input.value(), "");
        assert_eq!(input.cursor, 0);
    }

    #[test]
    fn delete_removes_char_at_cursor() {
        let mut input = Input::new();
        input.set_value("abc");
        input.cursor = 1; // before 'b'
        input.handle_input(&key(KeyCode::Delete));
        assert_eq!(input.value(), "ac");
        assert_eq!(input.cursor, 1);
    }

    #[test]
    fn delete_at_end_does_nothing() {
        let mut input = Input::new();
        input.set_value("abc");
        // cursor is at end (3)
        input.handle_input(&key(KeyCode::Delete));
        assert_eq!(input.value(), "abc");
    }

    // === Cursor movement tests ===

    #[test]
    fn left_right_moves_cursor() {
        let mut input = Input::new();
        input.set_value("abc");
        // cursor starts at end (3)
        assert_eq!(input.cursor, 3);

        input.handle_input(&key(KeyCode::Left));
        assert_eq!(input.cursor, 2);

        input.handle_input(&key(KeyCode::Left));
        assert_eq!(input.cursor, 1);

        input.handle_input(&key(KeyCode::Right));
        assert_eq!(input.cursor, 2);
    }

    #[test]
    fn left_at_start_stays() {
        let mut input = Input::new();
        input.set_value("abc");
        input.cursor = 0;
        input.handle_input(&key(KeyCode::Left));
        assert_eq!(input.cursor, 0);
    }

    #[test]
    fn right_at_end_stays() {
        let mut input = Input::new();
        input.set_value("abc");
        // cursor is at 3 (end)
        input.handle_input(&key(KeyCode::Right));
        assert_eq!(input.cursor, 3);
    }

    #[test]
    fn home_end_movement() {
        let mut input = Input::new();
        input.set_value("hello world");
        input.cursor = 5;

        input.handle_input(&key(KeyCode::Home));
        assert_eq!(input.cursor, 0);

        input.handle_input(&key(KeyCode::End));
        assert_eq!(input.cursor, 11);
    }

    #[test]
    fn cursor_render_shows_at_correct_position() {
        let mut input = Input::new();
        input.set_value("abc");
        input.cursor = 1; // cursor on 'b'

        let lines = input.render(20);
        let line = &lines[0];
        // Should render: "> a" + inverse("b") + "c" + padding
        assert!(line.starts_with("> a\x1b[7mb\x1b[27mc"));
    }

    // === Word movement tests ===

    #[test]
    fn ctrl_left_moves_word_backward() {
        let mut input = Input::new();
        input.set_value("hello world foo");
        // cursor at end (15)

        input.handle_input(&ctrl_key(KeyCode::Left));
        assert_eq!(input.cursor, 12); // before "foo"

        input.handle_input(&ctrl_key(KeyCode::Left));
        assert_eq!(input.cursor, 6); // before "world"

        input.handle_input(&ctrl_key(KeyCode::Left));
        assert_eq!(input.cursor, 0); // before "hello"
    }

    #[test]
    fn ctrl_right_moves_word_forward() {
        let mut input = Input::new();
        input.set_value("hello world foo");
        input.cursor = 0;

        input.handle_input(&ctrl_key(KeyCode::Right));
        assert_eq!(input.cursor, 6); // after "hello "

        input.handle_input(&ctrl_key(KeyCode::Right));
        assert_eq!(input.cursor, 12); // after "world "

        input.handle_input(&ctrl_key(KeyCode::Right));
        assert_eq!(input.cursor, 15); // end
    }

    // === Advanced editing tests ===

    #[test]
    fn ctrl_backspace_deletes_word() {
        let mut input = Input::new();
        input.set_value("hello world");
        // cursor at end (11)

        input.handle_input(&ctrl_key(KeyCode::Backspace));
        assert_eq!(input.value(), "hello ");
        assert_eq!(input.cursor, 6);
    }

    #[test]
    fn ctrl_u_deletes_to_start() {
        let mut input = Input::new();
        input.set_value("hello world");
        input.cursor = 6; // after "hello "

        input.handle_input(&ctrl_key(KeyCode::Char('u')));
        assert_eq!(input.value(), "world");
        assert_eq!(input.cursor, 0);
    }

    #[test]
    fn ctrl_k_deletes_to_end() {
        let mut input = Input::new();
        input.set_value("hello world");
        input.cursor = 5; // after "hello"

        input.handle_input(&ctrl_key(KeyCode::Char('k')));
        assert_eq!(input.value(), "hello");
        assert_eq!(input.cursor, 5);
    }

    // === Horizontal scrolling tests ===

    #[test]
    fn horizontal_scroll_when_text_exceeds_width() {
        let mut input = Input::new();
        // Width 10 - PROMPT_WIDTH(2) = 8 available chars
        input.set_value("abcdefghijklmnop"); // 16 chars
        // cursor at end (16), scroll should move right

        let lines = input.render(10);
        let line = &lines[0];
        // Should show prompt + last 8 chars (scroll adjusted)
        // Cursor is at position 16, available=8, so scroll=16-8+1=9
        // Visible: chars[9..17] but only 16 chars, so chars[9..16] = "jklmnop"
        // Plus cursor space at end
        assert!(line.starts_with("> "));
        assert_eq!(visible_width(line), 10);
    }

    #[test]
    fn scroll_follows_cursor_left() {
        let mut input = Input::new();
        input.set_value("abcdefghijklmnop");
        // Render once at narrow width to establish scroll
        input.render(10); // available=8, cursor=16, scroll=9

        // Move cursor to start
        input.cursor = 0;
        input.scroll_offset.set(9); // simulate previous scroll state

        let lines = input.render(10);
        let line = &lines[0];
        // Cursor at 0, scroll should reset to 0
        // Should show "abcdefgh" with cursor on 'a'
        assert!(line.contains("\x1b[7ma\x1b[27m"));
    }

    // === Callback tests ===

    #[test]
    fn on_submit_called_with_value() {
        use std::cell::RefCell;
        use std::rc::Rc;

        let submitted = Rc::new(RefCell::new(String::new()));
        let submitted_clone = submitted.clone();

        let mut input = Input::new();
        input.set_value("hello");
        input.on_submit = Some(Box::new(move |val: &str| {
            *submitted_clone.borrow_mut() = val.to_string();
        }));

        input.handle_input(&key(KeyCode::Enter));
        assert_eq!(*submitted.borrow(), "hello");
    }

    #[test]
    fn on_escape_called() {
        use std::cell::RefCell;
        use std::rc::Rc;

        let escaped = Rc::new(RefCell::new(false));
        let escaped_clone = escaped.clone();

        let mut input = Input::new();
        input.on_escape = Some(Box::new(move || {
            *escaped_clone.borrow_mut() = true;
        }));

        input.handle_input(&key(KeyCode::Esc));
        assert!(*escaped.borrow());
    }

    // === value/set_value tests ===

    #[test]
    fn value_returns_current_text() {
        let mut input = Input::new();
        assert_eq!(input.value(), "");
        input.handle_input(&char_key('x'));
        assert_eq!(input.value(), "x");
    }

    #[test]
    fn set_value_updates_text_and_cursor() {
        let mut input = Input::new();
        input.set_value("hello");
        assert_eq!(input.value(), "hello");
        assert_eq!(input.cursor, 5); // cursor at end
    }

    // === Object safety ===

    #[test]
    fn input_is_valid_component() {
        let _boxed: Box<dyn Component> = Box::new(Input::new());
    }

    // === Wide character tests ===

    #[test]
    fn wide_chars_render_exact_width() {
        // "你好世界" = 4 chars, 8 columns
        // width 12: prompt(2) + available(10)
        let mut input = Input::new();
        input.set_value("你好世界");
        let lines = input.render(12);
        assert_eq!(visible_width(&lines[0]), 12);
    }

    #[test]
    fn wide_chars_long_string_scrolling_no_overflow() {
        // "你好世界测试编程代码" = 10 chars, 20 columns
        // width 12: prompt(2) + available(10), cursor at end
        let mut input = Input::new();
        input.set_value("你好世界测试编程代码");
        let lines = input.render(12);
        assert_eq!(visible_width(&lines[0]), 12);
    }

    #[test]
    fn wide_chars_cursor_at_end_padding_correct() {
        // "你好" = 2 chars, 4 columns
        // width 20: prompt(2) + available(18)
        // Visible: "你好" (4 cols) + cursor space (1 col) = 5 cols after prompt
        let mut input = Input::new();
        input.set_value("你好");
        let lines = input.render(20);
        assert_eq!(visible_width(&lines[0]), 20);
    }

    #[test]
    fn wide_chars_cursor_in_middle_correct_position() {
        // "你好世界" = 4 chars, 8 columns
        // cursor=2 (on '世')
        let mut input = Input::new();
        input.set_value("你好世界");
        input.cursor = 2;
        let lines = input.render(20);
        let line = &lines[0];
        // Should show: "> 你好" + inverse("世") + "界" + padding
        assert!(line.contains("你好\x1b[7m世\x1b[27m界"));
        assert_eq!(visible_width(line), 20);
    }

    #[test]
    fn wide_chars_scroll_shows_correct_chars() {
        // "你好世界测试编程代码" = 10 chars, 20 cols
        // width 10: prompt(2) + available(8), cursor at end (10)
        // compute_scroll needs: cols(chars[scroll..10]) + 1 <= 8
        // cols must be <= 7 → at most 3 wide chars = 6 cols → scroll = 7
        let mut input = Input::new();
        input.set_value("你好世界测试编程代码");
        let lines = input.render(10);
        let line = &lines[0];
        assert_eq!(visible_width(line), 10);
        // Visible content should be last few chars, not overflowing
    }

    #[test]
    fn wide_chars_unfocused_exact_width() {
        let mut input = Input::new();
        input.set_value("你好世界");
        input.focused = false;
        let lines = input.render(12);
        assert_eq!(visible_width(&lines[0]), 12);
    }

    // === Insert in middle ===

    #[test]
    fn insert_at_cursor_position() {
        let mut input = Input::new();
        input.set_value("ac");
        input.cursor = 1; // between 'a' and 'c'
        input.handle_input(&char_key('b'));
        assert_eq!(input.value(), "abc");
        assert_eq!(input.cursor, 2);
    }

    // === Backspace in middle ===

    #[test]
    fn backspace_in_middle() {
        let mut input = Input::new();
        input.set_value("abc");
        input.cursor = 2; // after 'b'
        input.handle_input(&key(KeyCode::Backspace));
        assert_eq!(input.value(), "ac");
        assert_eq!(input.cursor, 1);
    }
}
