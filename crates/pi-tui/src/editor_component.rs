//! Port of packages/tui/src/editor-component.ts.

use crate::autocomplete::{AutocompleteItem, AutocompleteProvider, AutocompleteSuggestions};
use crate::tui::Component;
use std::rc::Rc;
use std::cell::RefCell;

/// Port of the `EditorPasteSnapshot` interface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditorPasteSnapshot {
    pub pastes: Vec<(i64, String)>,
    pub paste_counter: i64,
}

/// Port of the `EditorComponent` interface. Extends [`Component`] with the
/// optional surface extensions rely on (history, paste snapshots, autocomplete).
pub trait EditorComponent: Component {
    /// Get the current text content
    fn get_text(&self) -> String;

    /// Set the text content
    fn set_text(&mut self, text: &str);

    /// Handle raw terminal input (key presses, paste sequences, etc.)
    fn handle_input(&mut self, data: &str);

    /// Called when user submits (e.g., Enter key)
    fn on_submit(&mut self, text: &str) {
        let _ = text;
    }

    /// Called when text changes
    fn on_change(&mut self, text: &str) {
        let _ = text;
    }

    /// Add text to history for up/down navigation
    fn add_to_history(&mut self, text: &str) {
        let _ = text;
    }

    /// Prompt history entries available for up/down navigation (most recent first).
    fn get_history(&self) -> Vec<String> {
        Vec::new()
    }

    /// Clear prompt history (e.g. when switching to a different session).
    fn clear_history(&mut self) {}

    /// Insert text at current cursor position
    fn insert_text_at_cursor(&mut self, text: &str) {
        let _ = text;
    }

    /// Get text with any markers expanded (e.g., paste markers).
    /// Falls back to `get_text()` when not implemented.
    fn get_expanded_text(&self) -> String {
        self.get_text()
    }

    fn get_paste_snapshot(&self) -> Option<EditorPasteSnapshot> {
        None
    }

    fn restore_paste_snapshot(&mut self, snapshot: &EditorPasteSnapshot) {
        let _ = snapshot;
    }

    /// Set the autocomplete provider
    fn set_autocomplete_provider(&mut self, provider: Rc<RefCell<dyn AutocompleteProvider>>) {
        let _ = provider;
    }

    /// Border color function
    fn border_color(&self, str_value: &str) -> String {
        str_value.to_string()
    }

    /// Background color function
    fn background_color(&self, str_value: &str) -> String {
        str_value.to_string()
    }

    /// Set horizontal padding
    fn set_padding_x(&mut self, padding: f64) {
        let _ = padding;
    }

    /// Set max visible items in autocomplete dropdown
    fn set_autocomplete_max_visible(&mut self, max_visible: f64) {
        let _ = max_visible;
    }
}

/// Port of `AutocompleteSuggestions` re-exported for editor implementations.
pub use crate::autocomplete::AutocompleteSuggestions as EditorAutocompleteSuggestions;

/// Port of the `AutocompleteItem` re-export used by `EditorComponent`.
pub use crate::autocomplete::AutocompleteItem as EditorAutocompleteItem;

#[cfg(test)]
mod tests {
    use super::*;

    struct StubEditor {
        text: String,
    }

    impl Component for StubEditor {
        fn render(&mut self, _width: f64) -> Vec<String> {
            vec![self.text.clone()]
        }

        fn invalidate(&mut self) {}
    }

    impl EditorComponent for StubEditor {
        fn get_text(&self) -> String {
            self.text.clone()
        }

        fn set_text(&mut self, text: &str) {
            self.text = text.to_string();
        }

        fn handle_input(&mut self, data: &str) {
            self.text.push_str(data);
        }
    }

    #[test]
    fn default_methods_fall_back_to_text() {
        let mut editor = StubEditor {
            text: "hi".to_string(),
        };
        assert_eq!(editor.get_expanded_text(), "hi");
        assert_eq!(editor.get_history(), Vec::<String>::new());
        assert!(editor.get_paste_snapshot().is_none());
        assert_eq!(editor.border_color("x"), "x");
        editor.set_text("yo");
        assert_eq!(editor.get_text(), "yo");
        let _: &dyn AutocompleteProvider = &Unused;
        let _ = std::mem::size_of::<AutocompleteSuggestions>();
        let _ = std::mem::size_of::<AutocompleteItem>();
    }

    struct Unused;

    #[async_trait::async_trait]
    impl AutocompleteProvider for Unused {
        async fn get_suggestions(
            &self,
            _lines: &[String],
            _cursor_line: usize,
            _cursor_col: usize,
            _signal: &crate::autocomplete::AbortSignal,
            _force: bool,
        ) -> Option<AutocompleteSuggestions> {
            None
        }

        fn apply_completion(
            &self,
            _lines: &[String],
            _cursor_line: usize,
            _cursor_col: usize,
            _item: &AutocompleteItem,
            _prefix: &str,
        ) -> crate::autocomplete::ApplyCompletionResult {
            crate::autocomplete::ApplyCompletionResult {
                lines: Vec::new(),
                cursor_line: 0,
                cursor_col: 0,
            }
        }
    }
}
