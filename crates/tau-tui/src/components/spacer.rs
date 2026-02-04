// Spacer component — renders N empty lines for vertical spacing.

use crate::component::Component;

/// A component that renders empty lines for vertical spacing.
pub struct Spacer {
    lines: usize,
}

impl Spacer {
    /// Create a new Spacer that renders `lines` empty lines.
    pub fn new(lines: usize) -> Self {
        Spacer { lines }
    }

    /// Update the number of empty lines.
    pub fn set_lines(&mut self, lines: usize) {
        self.lines = lines;
    }
}

impl Default for Spacer {
    fn default() -> Self {
        Spacer { lines: 1 }
    }
}

impl Component for Spacer {
    fn render(&self, _width: u16) -> Vec<String> {
        vec![String::new(); self.lines]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spacer_new_renders_n_empty_lines() {
        let spacer = Spacer::new(3);
        let lines = spacer.render(80);
        assert_eq!(lines, vec!["", "", ""]);
    }

    #[test]
    fn spacer_default_renders_one_empty_line() {
        let spacer = Spacer::default();
        let lines = spacer.render(80);
        assert_eq!(lines, vec![""]);
    }

    #[test]
    fn spacer_zero_lines_renders_empty() {
        let spacer = Spacer::new(0);
        let lines = spacer.render(80);
        assert!(lines.is_empty());
    }

    #[test]
    fn spacer_set_lines_updates_count() {
        let mut spacer = Spacer::new(1);
        spacer.set_lines(5);
        let lines = spacer.render(80);
        assert_eq!(lines.len(), 5);
        assert!(lines.iter().all(|l| l.is_empty()));
    }

    #[test]
    fn spacer_width_is_ignored() {
        let spacer = Spacer::new(2);
        let narrow = spacer.render(10);
        let wide = spacer.render(200);
        assert_eq!(narrow, wide);
    }

    #[test]
    fn spacer_is_valid_component() {
        // Verify Spacer can be boxed as dyn Component
        let _boxed: Box<dyn Component> = Box::new(Spacer::new(1));
    }
}
