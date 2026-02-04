// BoxComponent — wraps children with padding and optional background color.

use crate::component::Component;
use crate::utils::visible_width;

/// A box component that wraps children with padding and optional background color.
///
/// Renders children vertically, adding horizontal and vertical padding.
/// Optionally applies a background color (raw ANSI code) to every line including padding.
pub struct BoxComponent {
    children: Vec<Box<dyn Component>>,
    padding_x: u16,
    padding_y: u16,
    bg: Option<String>,
}

impl BoxComponent {
    /// Create a new BoxComponent with the given padding.
    ///
    /// - `padding_x`: horizontal padding (spaces on each side)
    /// - `padding_y`: vertical padding (empty lines above and below)
    pub fn new(padding_x: u16, padding_y: u16) -> Self {
        BoxComponent {
            children: Vec::new(),
            padding_x,
            padding_y,
            bg: None,
        }
    }

    /// Set the background color as a raw ANSI code (e.g., `"\x1b[48;5;236m"`).
    pub fn set_bg(&mut self, ansi_code: &str) {
        self.bg = Some(ansi_code.to_string());
    }

    /// Add a child component.
    pub fn add_child(&mut self, child: Box<dyn Component>) {
        self.children.push(child);
    }

    /// Remove the child at the given index. Panics if out of bounds.
    pub fn remove_child(&mut self, index: usize) -> Box<dyn Component> {
        self.children.remove(index)
    }

    /// Remove all children.
    pub fn clear(&mut self) {
        self.children.clear();
    }
}

impl Component for BoxComponent {
    fn render(&self, width: u16) -> Vec<String> {
        if self.children.is_empty() {
            return vec![];
        }

        let full_width = width as usize;
        let inner_width = full_width.saturating_sub(2 * self.padding_x as usize);

        // Collect all child lines rendered at inner width
        let mut child_lines = Vec::new();
        for child in &self.children {
            child_lines.extend(child.render(inner_width as u16));
        }

        // If children produce nothing, return empty
        if child_lines.is_empty() {
            return vec![];
        }

        let pad_left = " ".repeat(self.padding_x as usize);
        let bg_start = self.bg.as_deref().unwrap_or("");
        let bg_end = if self.bg.is_some() { "\x1b[0m" } else { "" };

        let mut lines = Vec::new();

        // Build a padded line (for padding_y rows: empty content)
        let empty_padded = format!(
            "{}{}{}",
            bg_start,
            " ".repeat(full_width),
            bg_end
        );

        // Top padding
        for _ in 0..self.padding_y {
            lines.push(empty_padded.clone());
        }

        // Content lines with horizontal padding
        for line in &child_lines {
            let vis = visible_width(line);
            let right_pad = full_width.saturating_sub(self.padding_x as usize + vis);
            lines.push(format!(
                "{}{}{}{}{}",
                bg_start,
                pad_left,
                line,
                " ".repeat(right_pad),
                bg_end,
            ));
        }

        // Bottom padding
        for _ in 0..self.padding_y {
            lines.push(empty_padded.clone());
        }

        lines
    }

    fn invalidate(&mut self) {
        for child in &mut self.children {
            child.invalidate();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::text::Text;

    /// Mock component returning fixed lines.
    struct MockChild {
        lines: Vec<String>,
    }

    impl MockChild {
        fn new(lines: Vec<&str>) -> Self {
            MockChild {
                lines: lines.into_iter().map(String::from).collect(),
            }
        }
    }

    impl Component for MockChild {
        fn render(&self, _width: u16) -> Vec<String> {
            self.lines.clone()
        }
    }

    #[test]
    fn empty_box_renders_nothing() {
        let b = BoxComponent::new(2, 1);
        let lines = b.render(40);
        assert!(lines.is_empty());
    }

    #[test]
    fn box_with_one_text_child_correct_padding() {
        let mut b = BoxComponent::new(2, 1);
        b.add_child(Box::new(Text::new("hello", 0, 0)));
        let lines = b.render(20);

        // padding_y=1 top + content + padding_y=1 bottom = 3 lines
        assert_eq!(lines.len(), 3);

        // Top padding: 20 spaces
        assert_eq!(visible_width(&lines[0]), 20);
        assert!(lines[0].trim().is_empty());

        // Content: 2 spaces + "hello" + 13 spaces = 20 chars visible
        assert!(lines[1].starts_with("  hello"));
        assert_eq!(visible_width(&lines[1]), 20);

        // Bottom padding: 20 spaces
        assert_eq!(visible_width(&lines[2]), 20);
        assert!(lines[2].trim().is_empty());
    }

    #[test]
    fn box_with_background_applies_to_all_lines() {
        let mut b = BoxComponent::new(1, 1);
        b.set_bg("\x1b[48;5;236m");
        b.add_child(Box::new(MockChild::new(vec!["hi"])));
        let lines = b.render(10);

        // 1 top padding + 1 content + 1 bottom padding = 3
        assert_eq!(lines.len(), 3);

        // All lines start with bg code and end with reset
        for line in &lines {
            assert!(line.starts_with("\x1b[48;5;236m"), "line should start with bg: {:?}", line);
            assert!(line.ends_with("\x1b[0m"), "line should end with reset: {:?}", line);
        }

        // Content line has padding + "hi"
        assert!(lines[1].contains("hi"));
    }

    #[test]
    fn box_no_background_no_ansi() {
        let mut b = BoxComponent::new(0, 0);
        b.add_child(Box::new(MockChild::new(vec!["hello"])));
        let lines = b.render(10);
        assert_eq!(lines.len(), 1);
        // No ANSI codes in output
        assert!(!lines[0].contains('\x1b'));
        assert!(lines[0].starts_with("hello"));
        assert_eq!(visible_width(&lines[0]), 10);
    }

    #[test]
    fn box_children_rendered_at_inner_width() {
        // Width 20, padding_x 3 → inner width = 20 - 6 = 14
        let mut b = BoxComponent::new(3, 0);
        // Text component wraps at inner_width; "hello world test" (16 chars) wraps at 14
        b.add_child(Box::new(Text::new("hello world test", 0, 0)));
        let lines = b.render(20);

        // "hello world" (11) fits in 14, "test" (4) wraps → 2 lines
        assert_eq!(lines.len(), 2);

        // Each line padded to full 20 width with 3-space left pad
        for line in &lines {
            assert!(line.starts_with("   ")); // 3 spaces padding
            assert_eq!(visible_width(line), 20);
        }
    }

    #[test]
    fn box_multiple_children() {
        let mut b = BoxComponent::new(1, 0);
        b.add_child(Box::new(MockChild::new(vec!["line1"])));
        b.add_child(Box::new(MockChild::new(vec!["line2", "line3"])));
        let lines = b.render(20);

        assert_eq!(lines.len(), 3);
        assert!(lines[0].contains("line1"));
        assert!(lines[1].contains("line2"));
        assert!(lines[2].contains("line3"));
    }

    #[test]
    fn box_remove_child() {
        let mut b = BoxComponent::new(0, 0);
        b.add_child(Box::new(MockChild::new(vec!["a"])));
        b.add_child(Box::new(MockChild::new(vec!["b"])));
        b.remove_child(0);
        let lines = b.render(10);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].starts_with("b"));
    }

    #[test]
    fn box_clear_children() {
        let mut b = BoxComponent::new(0, 0);
        b.add_child(Box::new(MockChild::new(vec!["a"])));
        b.clear();
        assert!(b.render(10).is_empty());
    }

    #[test]
    fn box_invalidate_propagates() {
        let mut b = BoxComponent::new(0, 0);
        b.add_child(Box::new(MockChild::new(vec!["a"])));
        b.invalidate(); // Should not panic, propagates to children
    }

    #[test]
    fn box_is_valid_component() {
        let _boxed: Box<dyn Component> = Box::new(BoxComponent::new(1, 1));
    }

    #[test]
    fn box_children_that_render_empty() {
        let mut b = BoxComponent::new(1, 1);
        b.add_child(Box::new(MockChild::new(vec![])));
        // Children produce no lines → empty
        assert!(b.render(20).is_empty());
    }

    #[test]
    fn box_padding_y_only() {
        let mut b = BoxComponent::new(0, 2);
        b.add_child(Box::new(MockChild::new(vec!["content"])));
        let lines = b.render(20);
        // 2 top + 1 content + 2 bottom = 5
        assert_eq!(lines.len(), 5);
        assert!(lines[0].trim().is_empty());
        assert!(lines[1].trim().is_empty());
        assert!(lines[2].starts_with("content"));
        assert!(lines[3].trim().is_empty());
        assert!(lines[4].trim().is_empty());
    }
}
