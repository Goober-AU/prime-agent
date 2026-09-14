//! Port of packages/tui/src/components/input.ts

use crate::keybindings::get_keybindings;
use crate::keys::decode_kitty_printable;
use crate::kill_ring::KillRing;
use crate::tui::{Component, Focusable, CURSOR_MARKER};
use crate::undo_stack::UndoStack;
use crate::utils::{graphemes, is_punctuation_char, is_whitespace_char, slice_by_column, visible_width};

/// Port of the `InputState` interface.
#[derive(Clone, Default)]
struct InputState {
    value: String,
    cursor: usize,
}

/// Port of the `lastAction` union: "kill" | "yank" | "type-word" | null.
#[derive(Clone, Copy, PartialEq, Eq)]
enum LastAction {
    Kill,
    Yank,
    TypeWord,
}

/// Input component - single-line text input with horizontal scrolling
pub struct Input {
    value: String,
    cursor: usize,
    pub on_submit: Option<Box<dyn FnMut(&str)>>,
    pub on_escape: Option<Box<dyn FnMut()>>,

    focused: bool,

    paste_buffer: String,
    is_in_paste: bool,

    kill_ring: KillRing,
    last_action: Option<LastAction>,

    undo_stack: UndoStack<InputState>,
}

impl Input {
    pub fn new() -> Self {
        Self {
            value: String::new(),
            cursor: 0,
            on_submit: None,
            on_escape: None,
            focused: false,
            paste_buffer: String::new(),
            is_in_paste: false,
            kill_ring: KillRing::new(),
            last_action: None,
            undo_stack: UndoStack::new(),
        }
    }

    pub fn get_value(&self) -> &str {
        &self.value
    }

    pub fn get_cursor(&self) -> usize {
        self.cursor
    }

    pub fn set_value(&mut self, value: String) {
        self.cursor = self.cursor.min(value.chars().count());
        // `Math.min(this.cursor, value.length)` is a UTF-16 length; the port keeps
        // the cursor on a char boundary so the value stays sliceable.
        self.value = value;
        if self.cursor > self.value.chars().count() {
            self.cursor = self.value.chars().count();
        }
    }

    pub fn kill_ring(&self) -> &KillRing {
        &self.kill_ring
    }

    fn char_index_to_byte(&self, index: usize) -> usize {
        self.value
            .char_indices()
            .nth(index)
            .map(|(byte, _)| byte)
            .unwrap_or(self.value.len())
    }

    fn slice_chars(&self, start: usize, end: usize) -> String {
        let start_byte = self.char_index_to_byte(start);
        let end_byte = self.char_index_to_byte(end);
        self.value[start_byte..end_byte].to_string()
    }

    fn push_undo(&mut self) {
        self.undo_stack.push(&InputState {
            value: self.value.clone(),
            cursor: self.cursor,
        });
    }

    fn undo(&mut self) {
        if let Some(snapshot) = self.undo_stack.pop() {
            self.value = snapshot.value;
            self.cursor = snapshot.cursor;
            self.last_action = None;
        }
    }

    fn insert_character(&mut self, char: &str) {
        if is_whitespace_char(char) || self.last_action != Some(LastAction::TypeWord) {
            self.push_undo();
        }
        self.last_action = Some(LastAction::TypeWord);

        let cursor_byte = self.char_index_to_byte(self.cursor);
        self.value.insert_str(cursor_byte, char);
        self.cursor += char.chars().count();
    }

    fn handle_backspace(&mut self) {
        self.last_action = None;
        if self.cursor > 0 {
            self.push_undo();
            let before_cursor = self.slice_chars(0, self.cursor);
            let segments = graphemes(&before_cursor);
            let grapheme_length = segments
                .last()
                .map(|segment| segment.chars().count())
                .unwrap_or(1);
            let start_byte = self.char_index_to_byte(self.cursor - grapheme_length);
            let end_byte = self.char_index_to_byte(self.cursor);
            self.value.replace_range(start_byte..end_byte, "");
            self.cursor -= grapheme_length;
        }
    }

    fn handle_forward_delete(&mut self) {
        self.last_action = None;
        if self.cursor < self.value.chars().count() {
            self.push_undo();
            let after_cursor = self.slice_chars(self.cursor, self.value.chars().count());
            let segments = graphemes(&after_cursor);
            let grapheme_length = segments
                .first()
                .map(|segment| segment.chars().count())
                .unwrap_or(1);
            let start_byte = self.char_index_to_byte(self.cursor);
            let end_byte = self.char_index_to_byte(self.cursor + grapheme_length);
            self.value.replace_range(start_byte..end_byte, "");
        }
    }

    fn delete_to_line_start(&mut self) {
        if self.cursor == 0 {
            return;
        }
        self.push_undo();
        let deleted_text = self.slice_chars(0, self.cursor);
        self.kill_ring.push(
            &deleted_text,
            true,
            self.last_action == Some(LastAction::Kill),
        );
        self.last_action = Some(LastAction::Kill);
        let cursor_byte = self.char_index_to_byte(self.cursor);
        self.value = self.value[cursor_byte..].to_string();
        self.cursor = 0;
    }

    fn delete_to_line_end(&mut self) {
        let total = self.value.chars().count();
        if self.cursor >= total {
            return;
        }
        self.push_undo();
        let deleted_text = self.slice_chars(self.cursor, total);
        self.kill_ring.push(
            &deleted_text,
            false,
            self.last_action == Some(LastAction::Kill),
        );
        self.last_action = Some(LastAction::Kill);
        let cursor_byte = self.char_index_to_byte(self.cursor);
        self.value = self.value[..cursor_byte].to_string();
    }

    fn delete_word_backwards(&mut self) {
        if self.cursor == 0 {
            return;
        }

        // Save lastAction before cursor movement (moveWordBackwards resets it)
        let was_kill = self.last_action == Some(LastAction::Kill);

        self.push_undo();

        let old_cursor = self.cursor;
        self.move_word_backwards();
        let delete_from = self.cursor;
        self.cursor = old_cursor;

        let deleted_text = self.slice_chars(delete_from, self.cursor);
        self.kill_ring.push(&deleted_text, true, was_kill);
        self.last_action = Some(LastAction::Kill);

        let start_byte = self.char_index_to_byte(delete_from);
        let end_byte = self.char_index_to_byte(self.cursor);
        self.value.replace_range(start_byte..end_byte, "");
        self.cursor = delete_from;
    }

    fn delete_word_forward(&mut self) {
        let total = self.value.chars().count();
        if self.cursor >= total {
            return;
        }

        // Save lastAction before cursor movement (moveWordForwards resets it)
        let was_kill = self.last_action == Some(LastAction::Kill);

        self.push_undo();

        let old_cursor = self.cursor;
        self.move_word_forwards();
        let delete_to = self.cursor;
        self.cursor = old_cursor;

        let deleted_text = self.slice_chars(self.cursor, delete_to);
        self.kill_ring.push(&deleted_text, false, was_kill);
        self.last_action = Some(LastAction::Kill);

        let start_byte = self.char_index_to_byte(self.cursor);
        let end_byte = self.char_index_to_byte(delete_to);
        self.value.replace_range(start_byte..end_byte, "");
    }

    fn yank(&mut self) {
        let text = match self.kill_ring.peek() {
            Some(text) if !text.is_empty() => text,
            _ => return,
        };

        self.push_undo();

        let cursor_byte = self.char_index_to_byte(self.cursor);
        self.value.insert_str(cursor_byte, &text);
        self.cursor += text.chars().count();
        self.last_action = Some(LastAction::Yank);
    }

    fn yank_pop(&mut self) {
        if self.last_action != Some(LastAction::Yank) || self.kill_ring.len() <= 1 {
            return;
        }

        self.push_undo();

        let prev_text = self.kill_ring.peek().unwrap_or_default();
        let prev_len = prev_text.chars().count();
        let start_byte = self.char_index_to_byte(self.cursor.saturating_sub(prev_len));
        let end_byte = self.char_index_to_byte(self.cursor);
        self.value.replace_range(start_byte..end_byte, "");
        self.cursor -= prev_len.min(self.cursor);

        self.kill_ring.rotate();
        let text = self.kill_ring.peek().unwrap_or_default();
        let cursor_byte = self.char_index_to_byte(self.cursor);
        self.value.insert_str(cursor_byte, &text);
        self.cursor += text.chars().count();
        self.last_action = Some(LastAction::Yank);
    }

    fn move_word_backwards(&mut self) {
        if self.cursor == 0 {
            return;
        }

        self.last_action = None;
        let text_before_cursor = self.slice_chars(0, self.cursor);
        let mut segments = graphemes(&text_before_cursor);

        while let Some(last) = segments.last() {
            if !is_whitespace_char(last) {
                break;
            }
            let segment = segments.pop().unwrap_or_default();
            self.cursor -= segment.chars().count();
        }

        if let Some(last_grapheme) = segments.last().cloned() {
            if is_punctuation_char(&last_grapheme) {
                while let Some(last) = segments.last() {
                    if !is_punctuation_char(last) {
                        break;
                    }
                    let segment = segments.pop().unwrap_or_default();
                    self.cursor -= segment.chars().count();
                }
            } else {
                while let Some(last) = segments.last() {
                    if is_whitespace_char(last) || is_punctuation_char(last) {
                        break;
                    }
                    let segment = segments.pop().unwrap_or_default();
                    self.cursor -= segment.chars().count();
                }
            }
        }
    }

    fn move_word_forwards(&mut self) {
        let total = self.value.chars().count();
        if self.cursor >= total {
            return;
        }

        self.last_action = None;
        let text_after_cursor = self.slice_chars(self.cursor, total);
        let segments = graphemes(&text_after_cursor);
        let mut index = 0usize;

        while index < segments.len() && is_whitespace_char(&segments[index]) {
            self.cursor += segments[index].chars().count();
            index += 1;
        }

        if index < segments.len() {
            let first_grapheme = segments[index].clone();
            if is_punctuation_char(&first_grapheme) {
                while index < segments.len() && is_punctuation_char(&segments[index]) {
                    self.cursor += segments[index].chars().count();
                    index += 1;
                }
            } else {
                while index < segments.len()
                    && !is_whitespace_char(&segments[index])
                    && !is_punctuation_char(&segments[index])
                {
                    self.cursor += segments[index].chars().count();
                    index += 1;
                }
            }
        }
    }

    fn handle_paste(&mut self, pasted_text: &str) {
        self.last_action = None;
        self.push_undo();

        let clean_text = pasted_text
            .replace("\r\n", "")
            .replace('\r', "")
            .replace('\n', "")
            .replace('\t', "    ");

        let cursor_byte = self.char_index_to_byte(self.cursor);
        self.value.insert_str(cursor_byte, &clean_text);
        self.cursor += clean_text.chars().count();
    }
}

impl Default for Input {
    fn default() -> Self {
        Self::new()
    }
}

impl Focusable for Input {
    fn focused(&self) -> bool {
        self.focused
    }

    fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
    }
}

impl Component for Input {
    /// `isFocusable(component)` gate (`packages/tui/src/tui.ts:418-421`).
    fn as_focusable(&mut self) -> Option<&mut dyn super::super::tui::Focusable> {
        Some(self)
    }

    fn handle_input(&mut self, data: &str) {
        let mut data = data.to_string();
        if data.contains("\x1b[200~") {
            self.is_in_paste = true;
            self.paste_buffer = String::new();
            // `data.replace("\x1b[200~", "")` (input.ts:51) removes only the FIRST
            // start marker; a second marker in the same chunk stays as pasted text.
            if let Some(start_index) = data.find("\x1b[200~") {
                data.replace_range(start_index..start_index + 6, ""); // 6 = length of \x1b[200~
            }
        }

        if self.is_in_paste {
            self.paste_buffer.push_str(&data);

            if let Some(end_index) = self.paste_buffer.find("\x1b[201~") {
                let paste_content = self.paste_buffer[..end_index].to_string();

                self.handle_paste(&paste_content);

                self.is_in_paste = false;

                let remaining = self.paste_buffer[end_index + 6..].to_string(); // 6 = length of \x1b[201~
                self.paste_buffer = String::new();
                if !remaining.is_empty() {
                    self.handle_input(&remaining);
                }
            }
            return;
        }

        let kb = get_keybindings();

        if kb.matches(&data, "tui.select.cancel") {
            if let Some(callback) = self.on_escape.as_mut() {
                callback();
            }
            return;
        }

        if kb.matches(&data, "tui.editor.undo") {
            self.undo();
            return;
        }

        if kb.matches(&data, "tui.input.submit") || data == "\n" {
            if let Some(callback) = self.on_submit.as_mut() {
                callback(&self.value);
            }
            return;
        }

        if kb.matches(&data, "tui.editor.deleteCharBackward") {
            self.handle_backspace();
            return;
        }

        if kb.matches(&data, "tui.editor.deleteCharForward") {
            self.handle_forward_delete();
            return;
        }

        if kb.matches(&data, "tui.editor.deleteWordBackward") {
            self.delete_word_backwards();
            return;
        }

        if kb.matches(&data, "tui.editor.deleteWordForward") {
            self.delete_word_forward();
            return;
        }

        if kb.matches(&data, "tui.editor.deleteToLineStart") {
            self.delete_to_line_start();
            return;
        }

        if kb.matches(&data, "tui.editor.deleteToLineEnd") {
            self.delete_to_line_end();
            return;
        }

        if kb.matches(&data, "tui.editor.yank") {
            self.yank();
            return;
        }
        if kb.matches(&data, "tui.editor.yankPop") {
            self.yank_pop();
            return;
        }

        if kb.matches(&data, "tui.editor.cursorLeft") {
            self.last_action = None;
            if self.cursor > 0 {
                let before_cursor = self.slice_chars(0, self.cursor);
                let segments = graphemes(&before_cursor);
                let last_grapheme = segments.last().map(|segment| segment.chars().count());
                self.cursor -= last_grapheme.unwrap_or(1);
            }
            return;
        }

        if kb.matches(&data, "tui.editor.cursorRight") {
            self.last_action = None;
            let total = self.value.chars().count();
            if self.cursor < total {
                let after_cursor = self.slice_chars(self.cursor, total);
                let segments = graphemes(&after_cursor);
                let first_grapheme = segments.first().map(|segment| segment.chars().count());
                self.cursor += first_grapheme.unwrap_or(1);
            }
            return;
        }

        if kb.matches(&data, "tui.editor.cursorLineStart") {
            self.last_action = None;
            self.cursor = 0;
            return;
        }

        if kb.matches(&data, "tui.editor.cursorLineEnd") {
            self.last_action = None;
            self.cursor = self.value.chars().count();
            return;
        }

        if kb.matches(&data, "tui.editor.cursorWordLeft") {
            self.move_word_backwards();
            return;
        }

        if kb.matches(&data, "tui.editor.cursorWordRight") {
            self.move_word_forwards();
            return;
        }

        // Kitty CSI-u printable character (e.g. \x1b[97u for 'a').
        // Terminals with Kitty protocol flag 1 (disambiguate) send CSI-u for all keys,
        // including plain printable characters. Decode before the control-char check
        // since CSI-u sequences contain \x1b which would be rejected.
        if let Some(kitty_printable) = decode_kitty_printable(&data) {
            self.insert_character(&kitty_printable);
            return;
        }

        // Regular character input - accept printable characters including Unicode,
        // but reject control characters (C0: 0x00-0x1F, DEL: 0x7F, C1: 0x80-0x9F)
        let has_control_chars = data.chars().any(|ch| {
            let code = ch as u32;
            code < 32 || code == 0x7f || (0x80..=0x9f).contains(&code)
        });
        if !has_control_chars {
            self.insert_character(&data);
        }
    }

    fn invalidate(&mut self) {}

    fn render(&mut self, width: f64) -> Vec<String> {
        let width = width.max(0.0).floor() as usize;
        let prompt = "> ";
        // `width - prompt.length` stays signed because TypeScript can go negative.
        let available_width = width as i64 - prompt.len() as i64;

        if available_width <= 0 {
            return vec![prompt.to_string()];
        }
        let available_width = available_width as usize;

        let visible_text;
        // `cursorDisplay = this.cursor` (input.ts:413) is a UTF-16 index that is
        // used directly as a slice offset (`visibleText.slice(cursorDisplay)`,
        // input.ts:443-448). This port's cursor is a CHAR index, so the
        // no-scroll branch below converts it to a byte offset of `visible_text`
        // (== `self.value` there); the scroll branch instead derives it from
        // `before_cursor.len()` (input.rs:578), which is already a byte offset,
        // so the conversion must not be applied twice.
        let mut cursor_display = self.cursor;
        let total_width = visible_width(&self.value);

        if total_width < available_width {
            visible_text = self.value.clone();
            cursor_display = self.char_index_to_byte(self.cursor);
        } else {
            let scroll_width = if self.cursor == self.value.chars().count() {
                available_width.saturating_sub(1)
            } else {
                available_width
            };
            let cursor_col = visible_width(&self.slice_chars(0, self.cursor));

            if scroll_width > 0 {
                let half_width = scroll_width / 2;
                let start_col;

                if cursor_col < half_width {
                    start_col = 0;
                } else if cursor_col > total_width.saturating_sub(half_width) {
                    start_col = total_width.saturating_sub(scroll_width);
                } else {
                    start_col = cursor_col.saturating_sub(half_width);
                }

                visible_text = slice_by_column(&self.value, start_col, scroll_width, true);
                let before_cursor = slice_by_column(
                    &self.value,
                    start_col,
                    cursor_col.saturating_sub(start_col),
                    true,
                );
                // `cursorDisplay = beforeCursor.length` (input.ts:436) keeps this a
                // byte offset into `visible_text`, so no conversion is needed here.
                cursor_display = before_cursor.len();
            } else {
                visible_text = String::new();
                cursor_display = 0;
            }
        }

        let after_cursor_text = visible_text
            .get(cursor_display..)
            .unwrap_or_default()
            .to_string();
        let segments = graphemes(&after_cursor_text);
        let cursor_grapheme = segments.first().cloned();

        let before_cursor = visible_text
            .get(..cursor_display)
            .unwrap_or_default()
            .to_string();
        let at_cursor = cursor_grapheme.unwrap_or_else(|| " ".to_string()); // Character at cursor, or space if at end
        let after_cursor = visible_text
            .get(cursor_display + at_cursor.len()..)
            .unwrap_or_default()
            .to_string();

        // Hardware cursor marker (zero-width, emitted before fake cursor for IME positioning)
        let marker = if self.focused { CURSOR_MARKER } else { "" };

        let cursor_char = format!("\x1b[7m{at_cursor}\x1b[27m"); // ESC[7m = reverse video, ESC[27m = normal
        let text_with_cursor = format!("{before_cursor}{marker}{cursor_char}{after_cursor}");

        let visual_length = visible_width(&text_with_cursor);
        let padding = " ".repeat(available_width.saturating_sub(visual_length));
        let line = format!("{prompt}{text_with_cursor}{padding}");

        vec![line]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn value_and_cursor_accessors() {
        let mut input = Input::new();
        assert_eq!(input.get_value(), "");
        assert_eq!(input.get_cursor(), 0);
        input.set_value("hello".to_string());
        assert_eq!(input.get_value(), "hello");
    }

    #[test]
    fn render_marks_cursor_with_reverse_video() {
        let mut input = Input::new();
        input.set_value("ab".to_string());
        let lines = input.render(10.0);
        // Cursor at column 0 highlights "a"; marker absent while unfocused.
        // width 10 - prompt 2 = 8 available, 2 visible cells, 6 spaces of padding.
        assert_eq!(lines, vec!["> \x1b[7ma\x1b[27mb      ".to_string()]);
    }

    #[test]
    fn render_emits_cursor_marker_when_focused() {
        let mut input = Input::new();
        input.set_focused(true);
        let lines = input.render(6.0);
        // width 6 - prompt 2 = 4 available; the reverse-video space occupies 1 cell.
        assert_eq!(lines, vec!["> \x1b_pi:c\x07\x1b[7m \x1b[27m   ".to_string()]);
    }

    #[test]
    fn render_returns_prompt_when_too_narrow() {
        let mut input = Input::new();
        assert_eq!(input.render(2.0), vec!["> ".to_string()]);
        assert_eq!(input.render(1.0), vec!["> ".to_string()]);
    }

    #[test]
    fn render_no_scroll_uses_byte_offset_for_multibyte_cursor() {
        // TS slices `visibleText.slice(cursorDisplay)` with `cursorDisplay = this.cursor`
        // (input.ts:413; `visibleText.slice(cursorDisplay)`, input.ts:443-448). Using the
        // char-count cursor as a byte offset made text vanish off a char boundary
        // (`str::get` returns None), so "\u{e9}\u{e9}a" with cursor 2 rendered "> " plus a cursor on space.
        let mut input = Input::new();
        input.set_value("\u{e9}\u{e9}a".to_string());
        input.handle_input("\x05"); // ctrl+e = end of line -> char cursor 3 (byte 5)
        assert_eq!(input.get_cursor(), 3);
        assert_eq!(
            input.render(8.0),
            vec!["> \u{e9}\u{e9}a\x1b[7m \x1b[27m  ".to_string()]
        );

        input.handle_input("\x1b[D"); // left arrow -> char cursor 2 (byte 4)
        assert_eq!(input.get_cursor(), 2);
        assert_eq!(
            input.render(8.0),
            vec!["> \u{e9}\u{e9}\x1b[7ma\x1b[27m   ".to_string()]
        );

        // A single multi-byte char before the cursor: char cursor 1 == byte offset 2.
        let mut input = Input::new();
        input.set_value("\u{e9}a".to_string());
        input.handle_input("\x05"); // end -> char cursor 2
        input.handle_input("\x1b[D"); // left -> char cursor 1 (byte 2)
        assert_eq!(input.get_cursor(), 1);
        assert_eq!(
            input.render(8.0),
            vec!["> \u{e9}\x1b[7ma\x1b[27m    ".to_string()]
        );
    }

    #[test]
    fn paste_start_marker_is_stripped_only_once() {
        // `data.replace("\x1b[200~", "")` (input.ts:51) strips only the FIRST marker;
        // a second marker in the same chunk stays part of the pasted text.
        let mut input = Input::new();
        input.handle_input("\x1b[200~\x1b[200~ab\x1b[201~");
        assert_eq!(input.get_value(), "\x1b[200~ab");
    }

    #[test]
    fn insert_characters_and_backspace() {
        let mut input = Input::new();
        input.handle_input("a");
        input.handle_input("b");
        assert_eq!(input.get_value(), "ab");
        assert_eq!(input.get_cursor(), 2);
        input.handle_input("\x7f");
        assert_eq!(input.get_value(), "a");
        assert_eq!(input.get_cursor(), 1);
    }

    #[test]
    fn control_characters_are_rejected() {
        let mut input = Input::new();
        input.handle_input("\x01");
        assert_eq!(input.get_value(), "");
    }

    #[test]
    fn submit_invokes_callback_with_value() {
        let mut input = Input::new();
        input.set_value("hi".to_string());
        let captured = std::rc::Rc::new(std::cell::RefCell::new(String::new()));
        let sink = std::rc::Rc::clone(&captured);
        input.on_submit = Some(Box::new(move |value| {
            *sink.borrow_mut() = value.to_string();
        }));
        input.handle_input("\r");
        assert_eq!(*captured.borrow(), "hi");
    }

    #[test]
    fn kill_and_yank_round_trip() {
        let mut input = Input::new();
        input.set_value("hello world".to_string());
        input.cursor = 11;
        // ctrl+k = delete to line end
        input.handle_input("\x0b");
        assert_eq!(input.get_value(), "hello world");
        // ctrl+u = delete to line start
        input.handle_input("\x15");
        assert_eq!(input.get_value(), "");
        assert_eq!(input.get_cursor(), 0);
        // ctrl+y = yank
        input.handle_input("\x19");
        assert_eq!(input.get_value(), "hello world");
    }

    #[test]
    fn paste_strips_newlines_and_expands_tabs() {
        let mut input = Input::new();
        input.handle_input("\x1b[200~a\r\nb\tc\x1b[201~");
        assert_eq!(input.get_value(), "ab    c");
    }

    #[test]
    fn word_movement_uses_word_boundaries() {
        let mut input = Input::new();
        input.set_value("foo bar".to_string());
        input.cursor = 7;
        // alt+b = cursor word left
        input.handle_input("\x1bb");
        assert_eq!(input.get_cursor(), 4);
        input.handle_input("\x1bb");
        assert_eq!(input.get_cursor(), 0);
        // alt+f = cursor word right
        input.handle_input("\x1bf");
        assert_eq!(input.get_cursor(), 3);
    }
}
