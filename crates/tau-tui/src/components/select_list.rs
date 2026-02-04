// Selectable list component with arrow-key navigation, filtering, and scrolling.

use crossterm::event::{KeyCode, KeyEvent};

use crate::component::Component;
use crate::utils::visible_width;

/// A single item in a SelectList.
#[derive(Debug, Clone)]
pub struct SelectItem {
    /// Value used for programmatic identification.
    pub value: String,
    /// Display label shown in the list.
    pub label: String,
    /// Optional description shown after the label.
    pub description: Option<String>,
}

impl SelectItem {
    pub fn new(value: impl Into<String>, label: impl Into<String>) -> Self {
        SelectItem {
            value: value.into(),
            label: label.into(),
            description: None,
        }
    }

    pub fn with_description(
        value: impl Into<String>,
        label: impl Into<String>,
        description: impl Into<String>,
    ) -> Self {
        SelectItem {
            value: value.into(),
            label: label.into(),
            description: Some(description.into()),
        }
    }
}

/// A selectable list with arrow-key navigation, filtering, and scrolling.
///
/// Renders a visible window of items. The selected item has a `→` prefix and
/// bold/inverse styling. Arrow Up/Down changes selection with wrapping.
/// Enter triggers `on_select`, Escape triggers `on_cancel`.
pub struct SelectList {
    /// All items (unfiltered).
    items: Vec<SelectItem>,
    /// Maximum number of visible items at once.
    max_visible: usize,
    /// Index into filtered_indices of the selected item.
    selected: usize,
    /// Scroll offset (index into filtered_indices of the first visible item).
    scroll_offset: usize,
    /// Current filter query (empty = show all).
    filter: String,
    /// Indices into `items` that match the current filter.
    filtered_indices: Vec<usize>,
    /// Callback invoked on Enter with the selected item.
    pub on_select: Option<Box<dyn FnMut(&SelectItem)>>,
    /// Callback invoked on Escape.
    pub on_cancel: Option<Box<dyn FnMut()>>,
}

impl SelectList {
    /// Create a new SelectList with the given items and max visible count.
    pub fn new(items: Vec<SelectItem>, max_visible: usize) -> Self {
        let filtered_indices: Vec<usize> = (0..items.len()).collect();
        SelectList {
            items,
            max_visible: max_visible.max(1),
            selected: 0,
            scroll_offset: 0,
            filter: String::new(),
            filtered_indices,
            on_select: None,
            on_cancel: None,
        }
    }

    /// Get the currently selected item, if any.
    pub fn selected_item(&self) -> Option<&SelectItem> {
        self.filtered_indices
            .get(self.selected)
            .map(|&idx| &self.items[idx])
    }

    /// Filter items by prefix match on label (case-insensitive).
    /// Resets selection to 0 and scroll to 0.
    pub fn set_filter(&mut self, query: &str) {
        self.filter = query.to_string();
        let query_lower = query.to_lowercase();
        self.filtered_indices = self
            .items
            .iter()
            .enumerate()
            .filter(|(_, item)| item.label.to_lowercase().starts_with(&query_lower))
            .map(|(i, _)| i)
            .collect();
        self.selected = 0;
        self.scroll_offset = 0;
    }

    /// Move selection up by one, wrapping to bottom.
    fn move_up(&mut self) {
        let count = self.filtered_indices.len();
        if count == 0 {
            return;
        }
        if self.selected == 0 {
            self.selected = count - 1;
        } else {
            self.selected -= 1;
        }
        self.ensure_visible();
    }

    /// Move selection down by one, wrapping to top.
    fn move_down(&mut self) {
        let count = self.filtered_indices.len();
        if count == 0 {
            return;
        }
        self.selected = (self.selected + 1) % count;
        self.ensure_visible();
    }

    /// Ensure the selected item is within the visible window.
    fn ensure_visible(&mut self) {
        if self.selected < self.scroll_offset {
            self.scroll_offset = self.selected;
        }
        if self.selected >= self.scroll_offset + self.max_visible {
            self.scroll_offset = self.selected - self.max_visible + 1;
        }
    }

    /// Number of filtered items.
    fn filtered_count(&self) -> usize {
        self.filtered_indices.len()
    }
}

impl Component for SelectList {
    fn render(&self, width: u16) -> Vec<String> {
        let total_width = width as usize;
        let count = self.filtered_count();

        if count == 0 {
            // Show "(no items)" placeholder
            let msg = "(no items)";
            let pad = total_width.saturating_sub(visible_width(msg));
            let mut line = msg.to_string();
            line.extend(std::iter::repeat(' ').take(pad));
            return vec![line];
        }

        let visible_count = count.min(self.max_visible);
        let visible_end = (self.scroll_offset + visible_count).min(count);

        let mut lines = Vec::with_capacity(visible_count + 1);

        for i in self.scroll_offset..visible_end {
            let item_idx = self.filtered_indices[i];
            let item = &self.items[item_idx];
            let is_selected = i == self.selected;

            let mut line = String::new();

            if is_selected {
                // Selected: "→ " prefix with bold/inverse styling
                line.push_str("\x1b[1;7m→ ");
                line.push_str(&item.label);
                if let Some(ref desc) = item.description {
                    line.push_str(" - ");
                    line.push_str(desc);
                }
                let content_width = visible_width(&line);
                let pad = total_width.saturating_sub(content_width);
                line.extend(std::iter::repeat(' ').take(pad));
                line.push_str("\x1b[0m");
            } else {
                // Unselected: "  " prefix (same width as "→ ")
                line.push_str("  ");
                line.push_str(&item.label);
                if let Some(ref desc) = item.description {
                    line.push_str(" - ");
                    line.push_str(desc);
                }
                let content_width = visible_width(&line);
                let pad = total_width.saturating_sub(content_width);
                line.extend(std::iter::repeat(' ').take(pad));
            }

            lines.push(line);
        }

        // Show scroll indicator if list is scrollable
        if count > self.max_visible {
            let indicator = format!("({}/{})", self.selected + 1, count);
            let pad = total_width.saturating_sub(visible_width(&indicator));
            let mut indicator_line = String::new();
            indicator_line.extend(std::iter::repeat(' ').take(pad));
            indicator_line.push_str(&indicator);
            lines.push(indicator_line);
        }

        lines
    }

    fn handle_input(&mut self, event: &KeyEvent) {
        match event.code {
            KeyCode::Up => self.move_up(),
            KeyCode::Down => self.move_down(),
            KeyCode::Enter => {
                if let Some(item) = self.selected_item().cloned() {
                    if let Some(ref mut cb) = self.on_select {
                        cb(&item);
                    }
                }
            }
            KeyCode::Esc => {
                if let Some(ref mut cb) = self.on_cancel {
                    cb();
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_items(labels: &[&str]) -> Vec<SelectItem> {
        labels
            .iter()
            .map(|&l| SelectItem::new(l, l))
            .collect()
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, crossterm::event::KeyModifiers::NONE)
    }

    // === Rendering tests ===

    #[test]
    fn renders_correct_number_of_visible_items() {
        let items = make_items(&["alpha", "beta", "gamma", "delta", "epsilon"]);
        let sl = SelectList::new(items, 3);
        let lines = sl.render(30);
        // 3 visible items + 1 scroll indicator = 4 lines
        assert_eq!(lines.len(), 4);
    }

    #[test]
    fn renders_all_items_when_fewer_than_max() {
        let items = make_items(&["alpha", "beta"]);
        let sl = SelectList::new(items, 5);
        let lines = sl.render(30);
        // 2 items, no scroll indicator
        assert_eq!(lines.len(), 2);
    }

    #[test]
    fn selected_item_has_arrow_prefix() {
        let items = make_items(&["alpha", "beta", "gamma"]);
        let sl = SelectList::new(items, 5);
        let lines = sl.render(30);
        // First item is selected — has "→ " prefix
        assert!(lines[0].contains("→ alpha"));
        // Others have "  " prefix
        assert!(lines[1].contains("  beta"));
    }

    #[test]
    fn selected_item_has_bold_inverse_styling() {
        let items = make_items(&["alpha"]);
        let sl = SelectList::new(items, 5);
        let lines = sl.render(30);
        // Should contain bold+inverse start and reset end
        assert!(lines[0].contains("\x1b[1;7m"));
        assert!(lines[0].contains("\x1b[0m"));
    }

    #[test]
    fn unselected_items_no_ansi() {
        let items = make_items(&["alpha", "beta"]);
        let sl = SelectList::new(items, 5);
        let lines = sl.render(30);
        // Second item (unselected) should have no ANSI codes
        assert!(!lines[1].contains("\x1b["));
    }

    #[test]
    fn lines_padded_to_full_width() {
        let items = make_items(&["alpha", "beta"]);
        let sl = SelectList::new(items, 5);
        let lines = sl.render(20);
        for line in &lines {
            assert_eq!(visible_width(line), 20, "line not padded: {:?}", line);
        }
    }

    #[test]
    fn description_rendered_after_label() {
        let items = vec![SelectItem::with_description("v1", "Alpha", "First letter")];
        let sl = SelectList::new(items, 5);
        let lines = sl.render(40);
        assert!(lines[0].contains("Alpha - First letter"));
    }

    // === Selection movement tests ===

    #[test]
    fn selection_moves_down() {
        let items = make_items(&["alpha", "beta", "gamma"]);
        let mut sl = SelectList::new(items, 5);
        assert_eq!(sl.selected, 0);

        sl.handle_input(&key(KeyCode::Down));
        assert_eq!(sl.selected, 1);

        sl.handle_input(&key(KeyCode::Down));
        assert_eq!(sl.selected, 2);
    }

    #[test]
    fn selection_moves_up() {
        let items = make_items(&["alpha", "beta", "gamma"]);
        let mut sl = SelectList::new(items, 5);
        sl.selected = 2;

        sl.handle_input(&key(KeyCode::Up));
        assert_eq!(sl.selected, 1);

        sl.handle_input(&key(KeyCode::Up));
        assert_eq!(sl.selected, 0);
    }

    #[test]
    fn wraps_down_to_top() {
        let items = make_items(&["alpha", "beta", "gamma"]);
        let mut sl = SelectList::new(items, 5);
        sl.selected = 2;

        sl.handle_input(&key(KeyCode::Down));
        assert_eq!(sl.selected, 0);
    }

    #[test]
    fn wraps_up_to_bottom() {
        let items = make_items(&["alpha", "beta", "gamma"]);
        let mut sl = SelectList::new(items, 5);
        assert_eq!(sl.selected, 0);

        sl.handle_input(&key(KeyCode::Up));
        assert_eq!(sl.selected, 2);
    }

    // === Scrolling tests ===

    #[test]
    fn scrolls_when_moving_below_visible() {
        let items = make_items(&["a", "b", "c", "d", "e"]);
        let mut sl = SelectList::new(items, 3);
        assert_eq!(sl.scroll_offset, 0);

        // Move to item 3 (index 2) — still visible
        sl.handle_input(&key(KeyCode::Down));
        sl.handle_input(&key(KeyCode::Down));
        assert_eq!(sl.scroll_offset, 0);

        // Move to item 4 (index 3) — should scroll
        sl.handle_input(&key(KeyCode::Down));
        assert_eq!(sl.scroll_offset, 1);
    }

    #[test]
    fn scrolls_when_moving_above_visible() {
        let items = make_items(&["a", "b", "c", "d", "e"]);
        let mut sl = SelectList::new(items, 3);
        sl.selected = 4;
        sl.scroll_offset = 2;

        // Move up to item 2 (visible)
        sl.handle_input(&key(KeyCode::Up));
        assert_eq!(sl.scroll_offset, 2);

        // Move up to item 2 (index 2 — visible at offset 2)
        sl.handle_input(&key(KeyCode::Up));
        assert_eq!(sl.scroll_offset, 2);

        // Move up to item 1 (index 1 — below offset 2, should scroll)
        sl.handle_input(&key(KeyCode::Up));
        assert_eq!(sl.scroll_offset, 1);
    }

    #[test]
    fn scroll_indicator_shows_position() {
        let items = make_items(&["a", "b", "c", "d", "e"]);
        let sl = SelectList::new(items, 3);
        let lines = sl.render(30);
        // Last line should be scroll indicator
        let last = lines.last().unwrap();
        assert!(last.contains("(1/5)"), "expected (1/5), got: {}", last);
    }

    #[test]
    fn no_scroll_indicator_when_all_visible() {
        let items = make_items(&["a", "b", "c"]);
        let sl = SelectList::new(items, 5);
        let lines = sl.render(30);
        assert_eq!(lines.len(), 3); // no indicator line
    }

    #[test]
    fn scroll_indicator_updates_with_selection() {
        let items = make_items(&["a", "b", "c", "d", "e"]);
        let mut sl = SelectList::new(items, 3);
        sl.handle_input(&key(KeyCode::Down)); // select index 1

        let lines = sl.render(30);
        let last = lines.last().unwrap();
        assert!(last.contains("(2/5)"), "expected (2/5), got: {}", last);
    }

    // === Filter tests ===

    #[test]
    fn filter_narrows_visible_items() {
        let items = make_items(&["apple", "banana", "apricot", "blueberry"]);
        let mut sl = SelectList::new(items, 5);

        sl.set_filter("ap");
        assert_eq!(sl.filtered_count(), 2);

        let lines = sl.render(30);
        assert!(lines[0].contains("apple"));
        assert!(lines[1].contains("apricot"));
    }

    #[test]
    fn filter_case_insensitive() {
        let items = make_items(&["Apple", "Banana", "APRICOT"]);
        let mut sl = SelectList::new(items, 5);

        sl.set_filter("ap");
        assert_eq!(sl.filtered_count(), 2);
    }

    #[test]
    fn filter_resets_selection() {
        let items = make_items(&["apple", "banana", "apricot"]);
        let mut sl = SelectList::new(items, 5);
        sl.selected = 2;

        sl.set_filter("a");
        assert_eq!(sl.selected, 0);
    }

    #[test]
    fn filter_empty_shows_all() {
        let items = make_items(&["apple", "banana"]);
        let mut sl = SelectList::new(items, 5);
        sl.set_filter("ap");
        assert_eq!(sl.filtered_count(), 1);

        sl.set_filter("");
        assert_eq!(sl.filtered_count(), 2);
    }

    #[test]
    fn filter_no_matches_shows_placeholder() {
        let items = make_items(&["apple", "banana"]);
        let mut sl = SelectList::new(items, 5);
        sl.set_filter("xyz");
        assert_eq!(sl.filtered_count(), 0);

        let lines = sl.render(30);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("(no items)"));
    }

    // === Callback tests ===

    #[test]
    fn enter_triggers_on_select() {
        use std::cell::RefCell;
        use std::rc::Rc;

        let selected_value = Rc::new(RefCell::new(String::new()));
        let sv = selected_value.clone();

        let items = make_items(&["alpha", "beta"]);
        let mut sl = SelectList::new(items, 5);
        sl.handle_input(&key(KeyCode::Down)); // select "beta"
        sl.on_select = Some(Box::new(move |item: &SelectItem| {
            *sv.borrow_mut() = item.value.clone();
        }));

        sl.handle_input(&key(KeyCode::Enter));
        assert_eq!(*selected_value.borrow(), "beta");
    }

    #[test]
    fn escape_triggers_on_cancel() {
        use std::cell::RefCell;
        use std::rc::Rc;

        let cancelled = Rc::new(RefCell::new(false));
        let c = cancelled.clone();

        let items = make_items(&["alpha"]);
        let mut sl = SelectList::new(items, 5);
        sl.on_cancel = Some(Box::new(move || {
            *c.borrow_mut() = true;
        }));

        sl.handle_input(&key(KeyCode::Esc));
        assert!(*cancelled.borrow());
    }

    // === selected_item() tests ===

    #[test]
    fn selected_item_returns_correct_item() {
        let items = make_items(&["alpha", "beta", "gamma"]);
        let mut sl = SelectList::new(items, 5);
        assert_eq!(sl.selected_item().unwrap().value, "alpha");

        sl.handle_input(&key(KeyCode::Down));
        assert_eq!(sl.selected_item().unwrap().value, "beta");
    }

    #[test]
    fn selected_item_after_filter() {
        let items = make_items(&["apple", "banana", "apricot"]);
        let mut sl = SelectList::new(items, 5);
        sl.set_filter("b");
        assert_eq!(sl.selected_item().unwrap().value, "banana");
    }

    #[test]
    fn selected_item_empty_filter_result() {
        let items = make_items(&["apple"]);
        let mut sl = SelectList::new(items, 5);
        sl.set_filter("xyz");
        assert!(sl.selected_item().is_none());
    }

    // === Edge cases ===

    #[test]
    fn empty_items() {
        let sl = SelectList::new(vec![], 5);
        let lines = sl.render(30);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("(no items)"));
        assert!(sl.selected_item().is_none());
    }

    #[test]
    fn single_item() {
        let items = make_items(&["only"]);
        let mut sl = SelectList::new(items, 5);
        // Down wraps to itself
        sl.handle_input(&key(KeyCode::Down));
        assert_eq!(sl.selected, 0);
        // Up wraps to itself
        sl.handle_input(&key(KeyCode::Up));
        assert_eq!(sl.selected, 0);
    }

    #[test]
    fn max_visible_zero_treated_as_one() {
        let items = make_items(&["alpha", "beta"]);
        let sl = SelectList::new(items, 0);
        assert_eq!(sl.max_visible, 1);
    }

    #[test]
    fn wrap_scroll_down_then_up() {
        let items = make_items(&["a", "b", "c", "d", "e"]);
        let mut sl = SelectList::new(items, 3);
        // Go to last item
        sl.selected = 4;
        sl.scroll_offset = 2;

        // Wrap to top
        sl.handle_input(&key(KeyCode::Down));
        assert_eq!(sl.selected, 0);
        assert_eq!(sl.scroll_offset, 0);
    }

    // === Object safety ===

    #[test]
    fn select_list_is_valid_component() {
        let items = make_items(&["a"]);
        let _boxed: Box<dyn Component> = Box::new(SelectList::new(items, 5));
    }
}
