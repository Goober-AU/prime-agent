//! Port of packages/coding-agent/src/modes/interactive/components/modal-back.ts
//!
//! Shared helper for treating the left arrow as "go back" inside dialogs.
//!
//! The left arrow is bound to `app.modal.back` so that, like Esc, it dismisses a
//! dialog and returns to the previous screen. Dialogs that contain a text field
//! (model filter, login input, session search) must not steal the left arrow
//! while the user is editing: it only acts as back when the field is empty or the
//! cursor is already at the start (column 0). This mirrors the chat editor's
//! existing `onAgentsBack` guard so behaviour stays consistent across the app.

use pi_tui::keybindings::get_keybindings;

/// A text input whose cursor position can be inspected.
pub trait BackGuardInput {
    fn get_cursor(&self) -> usize;
}

/// Returns true when `data` should dismiss the current dialog (act like Esc).
///
/// Pass the dialog's text input to keep the left arrow available for cursor
/// movement: back only triggers when the cursor sits at column 0. Omit `input`
/// for dialogs that have no text field, where left is always back.
pub fn should_treat_as_back(data: &str, input: Option<&dyn BackGuardInput>) -> bool {
    if !get_keybindings().matches(data, "app.modal.back") {
        return false;
    }
    match input {
        None => true,
        Some(input) => input.get_cursor() == 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FixedCursor(usize);

    impl BackGuardInput for FixedCursor {
        fn get_cursor(&self) -> usize {
            self.0
        }
    }

    #[test]
    fn left_arrow_is_back_only_at_column_zero() {
        crate::core::keybindings::KeybindingsManager::new(Default::default(), None).install();
        // `tui.editor.cursorLeft` and `app.modal.back` share the left arrow key.
        let keys = get_keybindings().get_keys("app.modal.back");
        assert!(!keys.is_empty(), "app.modal.back must have a default key");
        assert_eq!(keys[0], "left");
        let left = "\x1b[D";
        assert!(should_treat_as_back(&left, None));
        assert!(should_treat_as_back(&left, Some(&FixedCursor(0))));
        assert!(!should_treat_as_back(&left, Some(&FixedCursor(1))));
    }

    #[test]
    fn other_keys_are_never_back() {
        assert!(!should_treat_as_back("x", None));
        assert!(!should_treat_as_back("x", Some(&FixedCursor(0))));
    }
}
