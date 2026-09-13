//! Port of packages/tui/src/slash-command-context.ts.

/// Where the cursor sits relative to a slash command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlashCommandContext {
    Name {
        prefix: String,
        is_at_prompt_start: bool,
    },
    Argument {
        command_name: String,
        prefix: String,
        is_at_prompt_start: bool,
    },
}

pub fn get_slash_command_context(
    lines: &[String],
    cursor_line: usize,
    cursor_col: usize,
) -> Option<SlashCommandContext> {
    let current_line = lines.get(cursor_line).cloned().unwrap_or_default();
    // `currentLine.slice(0, cursorCol)` (slash-command-context.ts:11) slices at the
    // editor's cursor column. In this port that column is a BYTE offset
    // (editor.rs:1042 `current_line[..cursor_col]`, editor.rs:1100
    // `cursor_col + char.len()`), while TS counts UTF-16 units (editor.ts:1180-1184),
    // so slice bytes here to stay aligned with the editor. TS `slice` never throws,
    // so walk back to a char boundary instead of panicking on a stale offset.
    let mut end = cursor_col.min(current_line.len());
    while end > 0 && !current_line.is_char_boundary(end) {
        end -= 1;
    }
    let text_before_cursor: String = current_line[..end].to_string();
    let trimmed_start = text_before_cursor.trim_start().to_string();

    if cursor_line == 0 && trimmed_start.starts_with('/') {
        let separator_index = trimmed_start
            .char_indices()
            .find(|(_, c)| *c == ' ' || *c == '\t')
            .map(|(i, _)| i);
        let command_token = match separator_index {
            None => trimmed_start.clone(),
            Some(i) => trimmed_start[..i].to_string(),
        };

        let after_slash: String = command_token.chars().skip(1).collect();
        if after_slash.contains('/') {
            return None;
        }

        let separator_index = match separator_index {
            None => {
                return Some(SlashCommandContext::Name {
                    prefix: command_token,
                    is_at_prompt_start: true,
                })
            }
            Some(i) => i,
        };

        let command_name: String = command_token.chars().skip(1).collect();
        if command_name.is_empty() {
            return None;
        }

        let sep_len = trimmed_start[separator_index..]
            .chars()
            .next()
            .map(|c| c.len_utf8())
            .unwrap_or(0);
        return Some(SlashCommandContext::Argument {
            command_name,
            prefix: trimmed_start[separator_index + sep_len..].to_string(),
            is_at_prompt_start: true,
        });
    }

    let space_index = text_before_cursor.rfind(' ');
    let tab_index = text_before_cursor.rfind('\t');
    let token_start = match (space_index, tab_index) {
        (Some(a), Some(b)) => a.max(b) + 1,
        (Some(a), None) => a + 1,
        (None, Some(b)) => b + 1,
        (None, None) => 0,
    };
    // `textBeforeCursor.slice(tokenStart)` (slash-command-context.ts:40): `token_start`
    // comes from `rfind` (slash-command-context.ts:39), i.e. a BYTE index here, so
    // slice bytes instead of counting chars.
    let prefix: String =
        text_before_cursor[token_start.min(text_before_cursor.len())..].to_string();
    let prefix_after_slash: String = prefix.chars().skip(1).collect();
    if !prefix.starts_with('/') || prefix_after_slash.contains('/') {
        return None;
    }

    Some(SlashCommandContext::Name {
        prefix,
        is_at_prompt_start: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_start_name() {
        let lines = vec!["/he".to_string()];
        assert_eq!(
            get_slash_command_context(&lines, 0, 3),
            Some(SlashCommandContext::Name {
                prefix: "/he".to_string(),
                is_at_prompt_start: true
            })
        );
    }

    #[test]
    fn prompt_start_argument() {
        let lines = vec!["/help me".to_string()];
        assert_eq!(
            get_slash_command_context(&lines, 0, 8),
            Some(SlashCommandContext::Argument {
                command_name: "help".to_string(),
                prefix: "me".to_string(),
                is_at_prompt_start: true
            })
        );
    }

    #[test]
    fn nested_slash_is_rejected() {
        let lines = vec!["/a/b".to_string()];
        assert_eq!(get_slash_command_context(&lines, 0, 4), None);
    }

    #[test]
    fn mid_line_token_uses_byte_slices_for_multibyte_lines() {
        // `currentLine.slice(0, cursorCol)` and `textBeforeCursor.slice(tokenStart)`
        // (slash-command-context.ts:11, 40) operate on the editor's cursor column.
        // This port's column is a BYTE offset (editor.rs:1042 `current_line[..cursor_col]`),
        // so `chars().take(cursor_col)` over-took: line "日本 /he" is 6 chars but 10 bytes,
        // so byte column 8 (right after "/") took 8 CHARS, i.e. the whole line, and the
        // prefix came back as "/he" instead of "/".
        // TS reference: getSlashCommandContext(["日本 /he"], 0, 4) -> { kind: "name", prefix: "/" }
        // (TS column 4 is UTF-16 units; byte column 8 is the same position here.)
        let lines = vec!["日本 /he".to_string()];
        assert_eq!(
            get_slash_command_context(&lines, 0, 8),
            Some(SlashCommandContext::Name {
                prefix: "/".to_string(),
                is_at_prompt_start: false
            })
        );

        // Cursor at the end of the line (byte 10) keeps the whole command token.
        assert_eq!(
            get_slash_command_context(&lines, 0, 10),
            Some(SlashCommandContext::Name {
                prefix: "/he".to_string(),
                is_at_prompt_start: false
            })
        );

        // Byte 3 starts the second CJK char: the text before the cursor is "日" with
        // no separator and no "/", so there is no slash-command token yet.
        assert_eq!(get_slash_command_context(&lines, 0, 3), None);
        // A stale byte offset inside a multi-byte char must not panic ("日本" here means
        // the editor cursor is mid-character); TS `slice` clamps to a string.
        assert_eq!(get_slash_command_context(&lines, 0, 4), None);
    }

    #[test]
    fn prompt_start_argument_prefix_uses_byte_slice() {
        // TS getSlashCommandContext(["/he 日本"], 0, 6) -> argument prefix "日本"; byte 10 is
        // the end of that line, i.e. the same position in this port's byte column.
        let lines = vec!["/he 日本".to_string()];
        assert_eq!(
            get_slash_command_context(&lines, 0, 10),
            Some(SlashCommandContext::Argument {
                command_name: "he".to_string(),
                prefix: "日本".to_string(),
                is_at_prompt_start: true
            })
        );
    }

    #[test]
    fn cursor_col_past_line_end_does_not_panic() {
        // TS `slice(0, cursorCol)` clamps; a byte offset past the end must not panic.
        let lines = vec!["/help".to_string()];
        assert_eq!(
            get_slash_command_context(&lines, 0, 99),
            Some(SlashCommandContext::Name {
                prefix: "/help".to_string(),
                is_at_prompt_start: true
            })
        );
    }

    #[test]
    fn mid_line_token() {
        let lines = vec!["hello /mo".to_string()];
        assert_eq!(
            get_slash_command_context(&lines, 0, 9),
            Some(SlashCommandContext::Name {
                prefix: "/mo".to_string(),
                is_at_prompt_start: false
            })
        );
    }
}
