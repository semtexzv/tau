// Text component — word-wraps content and preserves ANSI styles across breaks.

use std::cell::RefCell;

use crate::component::Component;
use crate::utils::{visible_width, wrap_text_with_ansi};

/// Cached render result for a given width.
struct CachedRender {
    width: u16,
    lines: Vec<String>,
}

/// A text component that word-wraps content and preserves ANSI styles.
///
/// Renders text with configurable horizontal and vertical padding.
/// Caches rendered output — returns cached result if text and width unchanged.
pub struct Text {
    text: String,
    padding_x: u16,
    padding_y: u16,
    cache: RefCell<Option<CachedRender>>,
}

impl Text {
    /// Create a new Text component.
    ///
    /// - `text`: the content to display (may contain ANSI codes and newlines)
    /// - `padding_x`: horizontal padding (spaces on each side)
    /// - `padding_y`: vertical padding (empty lines above and below)
    pub fn new(text: &str, padding_x: u16, padding_y: u16) -> Self {
        Text {
            text: text.to_string(),
            padding_x,
            padding_y,
            cache: RefCell::new(None),
        }
    }

    /// Update the text content. Invalidates the render cache.
    pub fn set_text(&mut self, text: &str) {
        if self.text != text {
            self.text = text.to_string();
            self.cache.borrow_mut().take();
        }
    }
}

impl Component for Text {
    fn render(&self, width: u16) -> Vec<String> {
        // Check cache
        if let Some(ref cached) = *self.cache.borrow() {
            if cached.width == width {
                return cached.lines.clone();
            }
        }

        let lines = self.render_inner(width);

        // Store in cache
        *self.cache.borrow_mut() = Some(CachedRender {
            width,
            lines: lines.clone(),
        });

        lines
    }

    fn invalidate(&mut self) {
        self.cache.borrow_mut().take();
    }
}

impl Text {
    /// Core rendering logic, separated from caching.
    fn render_inner(&self, width: u16) -> Vec<String> {
        if self.text.is_empty() {
            return vec![];
        }

        let full_width = width as usize;
        let inner_width = full_width.saturating_sub(2 * self.padding_x as usize);

        if inner_width == 0 {
            return vec![];
        }

        let wrapped = wrap_text_with_ansi(&self.text, inner_width);
        let pad_left = " ".repeat(self.padding_x as usize);

        let mut lines = Vec::new();

        // Top padding
        for _ in 0..self.padding_y {
            lines.push(String::new());
        }

        // Content with horizontal padding
        for line in &wrapped {
            let vis_width = visible_width(line);
            let right_pad = full_width.saturating_sub(self.padding_x as usize + vis_width);
            lines.push(format!("{}{}{}", pad_left, line, " ".repeat(right_pad)));
        }

        // Bottom padding
        for _ in 0..self.padding_y {
            lines.push(String::new());
        }

        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_short_no_wrap() {
        let text = Text::new("hello", 0, 0);
        let lines = text.render(80);
        assert_eq!(lines.len(), 1);
        assert_eq!(visible_width(&lines[0]), 80); // padded to full width
        assert!(lines[0].starts_with("hello"));
    }

    #[test]
    fn text_long_wraps_at_word_boundary() {
        let text = Text::new("hello world", 0, 0);
        let lines = text.render(7);
        assert_eq!(lines.len(), 2);
        assert!(lines[0].starts_with("hello"));
        assert!(lines[1].starts_with("world"));
    }

    #[test]
    fn text_ansi_style_preserved() {
        let text = Text::new("\x1b[31mhello world\x1b[0m", 0, 0);
        let lines = text.render(7);
        assert_eq!(lines.len(), 2);
        // First line starts with red code
        assert!(lines[0].starts_with("\x1b[31mhello"));
        // Second line re-applies red, then has "world" + reset
        assert!(lines[1].contains("\x1b[31m"));
        assert!(lines[1].contains("world"));
    }

    #[test]
    fn text_empty_returns_empty() {
        let text = Text::new("", 0, 0);
        let lines = text.render(80);
        assert!(lines.is_empty());
    }

    #[test]
    fn text_padding_x() {
        let text = Text::new("hello", 2, 0);
        let lines = text.render(20);
        assert_eq!(lines.len(), 1);
        // 2 spaces padding left + "hello" + right padding to fill 20
        assert!(lines[0].starts_with("  hello"));
        assert_eq!(visible_width(&lines[0]), 20);
    }

    #[test]
    fn text_padding_y() {
        let text = Text::new("hello", 0, 2);
        let lines = text.render(80);
        // 2 empty lines + 1 content + 2 empty lines = 5
        assert_eq!(lines.len(), 5);
        assert!(lines[0].is_empty());
        assert!(lines[1].is_empty());
        assert!(lines[2].starts_with("hello"));
        assert!(lines[3].is_empty());
        assert!(lines[4].is_empty());
    }

    #[test]
    fn text_padding_both() {
        let text = Text::new("hi", 3, 1);
        let lines = text.render(20);
        // 1 padding_y + 1 content + 1 padding_y = 3
        assert_eq!(lines.len(), 3);
        assert!(lines[0].is_empty());
        assert!(lines[1].starts_with("   hi")); // 3 spaces + "hi"
        assert_eq!(visible_width(&lines[1]), 20);
        assert!(lines[2].is_empty());
    }

    #[test]
    fn text_wraps_within_padding() {
        // Width 20, padding_x 5 → inner width = 20 - 10 = 10
        let text = Text::new("hello beautiful world", 5, 0);
        let lines = text.render(20);
        // "hello" (5) + " " + "beautiful" (9) = 15 > 10 → wraps
        // Line 1: "hello"
        // Line 2: "beautiful"
        // Line 3: "world"
        assert_eq!(lines.len(), 3);
        assert!(lines[0].starts_with("     hello")); // 5 spaces + hello
        assert!(lines[1].starts_with("     beautiful"));
        assert!(lines[2].starts_with("     world"));
        for line in &lines {
            assert_eq!(visible_width(line), 20);
        }
    }

    #[test]
    fn text_set_text_invalidates_cache() {
        let mut text = Text::new("hello", 0, 0);
        let lines1 = text.render(80);
        assert!(lines1[0].starts_with("hello"));

        text.set_text("world");
        let lines2 = text.render(80);
        assert!(lines2[0].starts_with("world"));
    }

    #[test]
    fn text_cache_returns_same_result() {
        let text = Text::new("hello", 0, 0);
        let lines1 = text.render(80);
        let lines2 = text.render(80);
        assert_eq!(lines1, lines2);
    }

    #[test]
    fn text_cache_invalidated_on_width_change() {
        let text = Text::new("hello world", 0, 0);
        let lines_wide = text.render(80);
        assert_eq!(lines_wide.len(), 1); // fits on one line

        let lines_narrow = text.render(7);
        assert_eq!(lines_narrow.len(), 2); // wraps
    }

    #[test]
    fn text_invalidate_clears_cache() {
        let mut text = Text::new("hello", 0, 0);
        let _lines = text.render(80);
        text.invalidate();
        // After invalidate, should re-render (no way to verify directly,
        // but we can verify it doesn't crash or return stale data)
        let lines = text.render(80);
        assert!(lines[0].starts_with("hello"));
    }

    #[test]
    fn text_is_valid_component() {
        let _boxed: Box<dyn Component> = Box::new(Text::new("hello", 0, 0));
    }

    #[test]
    fn text_pads_each_line_to_full_width() {
        let text = Text::new("a\nb\nc", 0, 0);
        let lines = text.render(40);
        assert_eq!(lines.len(), 3);
        for line in &lines {
            assert_eq!(visible_width(line), 40);
        }
    }

    #[test]
    fn text_narrow_width_with_padding_returns_empty() {
        // Width 4, padding_x 3 → inner width = 4 - 6 < 0, saturates to 0
        let text = Text::new("hello", 3, 0);
        let lines = text.render(4);
        assert!(lines.is_empty());
    }
}
