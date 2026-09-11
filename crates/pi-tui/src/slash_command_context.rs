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
    let text_before_cursor: String = current_line.chars().take(cursor_col).collect();
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
    let prefix: String = text_before_cursor.chars().skip(token_start).collect();
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
