//! Port of packages/coding-agent/src/core/new-session-command.ts

use regex::Regex;

/// `interface ParsedNewSessionCommand`.
///
/// `name?: string` / `prompt?: string` are absent-vs-present, not
/// absent-vs-null, so both are `Option<String>` and skipped while serializing.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ParsedNewSessionCommand {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
}

/// `parseNewSessionCommand(rawArgs)`.
///
/// Parse `/new`'s raw suffix while preserving the initial prompt verbatim.
pub fn parse_new_session_command(raw_args: &str) -> Result<ParsedNewSessionCommand, String> {
    if raw_args.is_empty() {
        return Ok(ParsedNewSessionCommand::default());
    }
    let args = consume_new_separator(raw_args, "Expected whitespace after /new")?;
    if !args.starts_with('-') {
        return Ok(ParsedNewSessionCommand {
            name: None,
            prompt: Some(args),
        });
    }
    if regex_starts_with_dash_dash(&args) {
        return Ok(ParsedNewSessionCommand {
            name: None,
            prompt: Some(require_new_prompt(slice_chars(&args, 2))?),
        });
    }

    let option = first_non_space_run(&args).unwrap_or_else(|| args.clone());
    if !regex_starts_with_name(&args) {
        return Err(format!("Unknown /new option: {option}"));
    }
    let mut rest = consume_new_separator(
        slice_chars(&args, 6),
        "Missing value for /new option \"--name\"",
    )?;
    if regex_starts_with_name(&rest) {
        return Err("Duplicate /new option: \"--name\"".to_string());
    }
    if rest.is_empty() || rest.starts_with('-') {
        return Err("Missing value for /new option \"--name\"".to_string());
    }

    let name: String;
    let first = rest.chars().next().unwrap();
    if first == '"' || first == '\'' {
        let quote = first;
        let closing = rest.char_indices().skip(1).find(|(_, ch)| *ch == quote).map(|(i, _)| i);
        let Some(closing_quote) = closing else {
            return Err("Unterminated quote in /new --name".to_string());
        };
        name = rest[1..closing_quote].to_string();
        rest = rest[closing_quote + quote.len_utf8()..].to_string();
    } else {
        // `/^(\S+)([\s\S]*)$/` - the run of non-space characters, then the rest.
        let split = rest
            .char_indices()
            .find(|(_, ch)| ch.is_whitespace())
            .map(|(i, _)| i)
            .unwrap_or(rest.len());
        name = rest[..split].to_string();
        rest = rest[split..].to_string();
    }
    if name.trim().is_empty() {
        return Err("/new option \"--name\" cannot be empty".to_string());
    }
    if rest.is_empty() {
        return Ok(ParsedNewSessionCommand {
            name: Some(name),
            prompt: None,
        });
    }
    rest = consume_new_separator(&rest, "Expected \"--\" before the /new prompt")?;
    if regex_starts_with_name(&rest) {
        return Err("Duplicate /new option: \"--name\"".to_string());
    }
    if !regex_starts_with_dash_dash(&rest) {
        if rest.starts_with('-') {
            return Err(format!(
                "Unknown /new option: {}",
                first_non_space_run(&rest).unwrap_or_else(|| rest.clone())
            ));
        }
        return Err("Expected \"--\" before the /new prompt".to_string());
    }
    Ok(ParsedNewSessionCommand {
        name: Some(name),
        prompt: Some(require_new_prompt(slice_chars(&rest, 2))?),
    })
}

/// `consumeNewSeparator(value, error)`.
fn consume_new_separator(value: &str, error: &str) -> Result<String, String> {
    // `/^[ \t]*/` - leading spaces and tabs only.
    let mut offset = value
        .char_indices()
        .find(|(_, ch)| *ch != ' ' && *ch != '\t')
        .map(|(i, _)| i)
        .unwrap_or(value.len());
    if value[offset..].starts_with("\r\n") {
        offset += 2;
    } else {
        let next = value[offset..].chars().next();
        if next == Some('\n') || next == Some('\r') {
            offset += next.expect("checked").len_utf8();
        }
    }
    if offset == 0 {
        return Err(error.to_string());
    }
    Ok(value[offset..].to_string())
}

/// `requireNewPrompt(value)`.
fn require_new_prompt(value: &str) -> Result<String, String> {
    let prompt = consume_new_separator(value, "Missing prompt after /new \"--\"")?;
    if prompt.trim().is_empty() {
        return Err("Missing prompt after /new \"--\"".to_string());
    }
    Ok(prompt)
}

/// `/^--(?:\s|$)/` or `/^--name(?:\s|$)/`.
fn starts_with_flag(value: &str, flag: &str) -> bool {
    let Some(rest) = value.strip_prefix(flag) else {
        return false;
    };
    match rest.chars().next() {
        None => true,
        Some(ch) => ch.is_whitespace(),
    }
}

fn regex_starts_with_dash_dash(value: &str) -> bool {
    starts_with_flag(value, "--")
}

fn regex_starts_with_name(value: &str) -> bool {
    starts_with_flag(value, "--name")
}

/// `/^\S+/` - the first run of non-space characters.
fn first_non_space_run(value: &str) -> Option<String> {
    let end = value
        .char_indices()
        .find(|(_, ch)| ch.is_whitespace())
        .map(|(i, _)| i)
        .unwrap_or(value.len());
    if end == 0 {
        return None;
    }
    Some(value[..end].to_string())
}

/// `string.slice(n)` in UTF-16 units; `/new` arguments are ASCII flags, so a
/// byte slice matches for every input this parser accepts, but stay char-safe.
fn slice_chars(value: &str, count: usize) -> &str {
    match value.char_indices().nth(count) {
        Some((index, _)) => &value[index..],
        None => "",
    }
}

/// Kept for parity with the TypeScript regex imports used by the tests below.
#[allow(dead_code)]
fn compiled_flag_regex() -> &'static Regex {
    static REGEX: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    REGEX.get_or_init(|| Regex::new(r"^--(?:\s|$)").expect("static pattern"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_suffix_parses_to_an_empty_command() {
        assert_eq!(parse_new_session_command("").unwrap(), ParsedNewSessionCommand::default());
    }

    #[test]
    fn a_plain_suffix_is_the_prompt_verbatim() {
        let parsed = parse_new_session_command(" hello world").unwrap();
        assert_eq!(parsed.prompt.as_deref(), Some("hello world"));
        assert!(parsed.name.is_none());
    }

    #[test]
    fn double_dash_starts_the_prompt() {
        let parsed = parse_new_session_command(" -- fix the bug").unwrap();
        assert_eq!(parsed.prompt.as_deref(), Some("fix the bug"));
    }

    #[test]
    fn name_without_a_prompt_keeps_the_name_only() {
        let parsed = parse_new_session_command(" --name worker").unwrap();
        assert_eq!(parsed.name.as_deref(), Some("worker"));
        assert!(parsed.prompt.is_none());
    }

    #[test]
    fn quoted_names_are_unwrapped_and_the_prompt_follows() {
        let parsed = parse_new_session_command(" --name \"my worker\" -- do it").unwrap();
        assert_eq!(parsed.name.as_deref(), Some("my worker"));
        assert_eq!(parsed.prompt.as_deref(), Some("do it"));
    }

    #[test]
    fn missing_whitespace_is_rejected() {
        assert_eq!(
            parse_new_session_command("x").unwrap_err(),
            "Expected whitespace after /new"
        );
    }

    #[test]
    fn unknown_and_duplicate_options_are_rejected() {
        assert_eq!(
            parse_new_session_command(" --bogus x").unwrap_err(),
            "Unknown /new option: --bogus"
        );
        assert_eq!(
            parse_new_session_command(" --name a --name b").unwrap_err(),
            "Duplicate /new option: \"--name\""
        );
        assert_eq!(
            parse_new_session_command(" --name \"unterminated").unwrap_err(),
            "Unterminated quote in /new --name"
        );
        assert_eq!(
            parse_new_session_command(" --name \"x\" --do-it").unwrap_err(),
            "Unknown /new option: --do-it"
        );
    }

    #[test]
    fn empty_prompt_after_double_dash_is_rejected() {
        assert_eq!(
            parse_new_session_command(" --   ").unwrap_err(),
            "Missing prompt after /new \"--\""
        );
    }
}
