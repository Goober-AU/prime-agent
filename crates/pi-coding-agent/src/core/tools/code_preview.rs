//! Port of packages/coding-agent/src/core/tools/code-preview.ts

use std::collections::HashMap;
use std::sync::OnceLock;

use regex::Regex;
use serde::{Deserialize, Serialize};

use super::ipython_cell_code::parse_ipython_bash_cell;

const DESCRIPTOR_MAX_WIDTH: usize = 64;

fn magic_line_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| Regex::new(r"^\s*!").expect("valid magic line pattern"))
}

fn comment_line_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| Regex::new(r"^\s*#").expect("valid comment pattern"))
}

fn cd_prefix_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| Regex::new(r"^\s*cd\s+([^&;|]+)(?:&&|;)\s*").expect("valid cd pattern"))
}

fn bash_set_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(r"^\s*set\s+[-+][A-Za-z]*(?:\s+[-+]?\w+)*(?:\s+pipefail)?\s*$")
            .expect("valid bash set pattern")
    })
}

fn bash_setup_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| Regex::new(r"^(?:export\s+\w+=|source\s+\S+|\.\s+\S+)").expect("valid bash setup pattern"))
}

fn python_import_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(r"^\s*(?:import\s+\S|from\s+\S+\s+import\s+)").expect("valid python import pattern")
    })
}

fn python_decorator_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| Regex::new(r"^\s*@").expect("valid python decorator pattern"))
}

fn python_definition_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| Regex::new(r"^\s*(?:async\s+def|def|class)\s+").expect("valid definition pattern"))
}

fn python_main_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(r#"^\s*if\s+__name__\s*==\s*['"]__main__['"]\s*:"#).expect("valid main pattern")
    })
}

fn python_control_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(r"^\s*(?:if|elif|else|for|while|with|try|except|finally)\b.*:\s*$")
            .expect("valid python control pattern")
    })
}

fn python_call_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(r"^\s*(?:await\s+)?[A-Za-z_][A-Za-z0-9_.]*\s*\(").expect("valid python call pattern")
    })
}

fn bash_skill_call_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(r#"^\s*(?:[A-Za-z_][A-Za-z0-9_]*\s*=\s*)?(?:await\s+)?bash\s*\(\s*[rR]?(?P<q>"""|'''|"|')"#)
            .expect("valid bash skill call pattern")
    })
}

fn python_low_signal_call_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(r"^\s*(?:await\s+)?(?:print|len|str|repr|int|float|list|dict|set|tuple)\s*\(")
            .expect("valid low signal call pattern")
    })
}

fn python_assignment_call_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(
            r"^\s*[A-Za-z_][A-Za-z0-9_]*(?:\s*:\s*[^=]+)?\s*=\s*(?:await\s+)?[A-Za-z_][A-Za-z0-9_.]*\s*\(",
        )
        .expect("valid assignment call pattern")
    })
}

fn python_low_signal_assignment_call_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(
            r"^\s*[A-Za-z_][A-Za-z0-9_]*(?:\s*:\s*[^=]+)?\s*=\s*(?:await\s+)?(?:Path|pathlib\.Path|json\.loads|json\.dumps|str|int|float|list|dict|set|tuple)\s*\(",
        )
        .expect("valid low signal assignment call pattern")
    })
}

fn python_effect_call_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(
            r"^\s*(?:await\s+)?[A-Za-z_][A-Za-z0-9_.]*\.(?:write_text|write_bytes|mkdir|unlink|rename|replace|touch|append|extend|update|add|remove|discard|close|commit|execute|run)\s*\(",
        )
        .expect("valid effect call pattern")
    })
}

fn heredoc_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(r#"<<-?\s*['"]?([A-Za-z_][A-Za-z0-9_]*)['"]?"#).expect("valid heredoc pattern")
    })
}

fn path_assign_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(r#"^\s*([A-Za-z_][A-Za-z0-9_]*)\s*=\s*(?:Path|pathlib\.Path)\(["']([^"']+)["']\)"#)
            .expect("valid path assign pattern")
    })
}

fn string_assign_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(r#"^\s*([A-Za-z_][A-Za-z0-9_]*)\s*=\s*["']([^"']+)["']"#).expect("valid string assign pattern")
    })
}

fn vitest_cli_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| Regex::new(r"(?:^|/)vitest/dist/cli\.js$").expect("valid vitest pattern"))
}

fn python_runner_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| Regex::new(r"\b(?:uv\s+run\s+)?python3?\b").expect("valid python runner pattern"))
}

fn node_word_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| Regex::new(r"\bnode\b").expect("valid node pattern"))
}

fn cat_write_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| Regex::new(r"\b(?:cat|tee)\b.*(?:>|\s)(\S+)\s*<<-?").expect("valid cat write pattern"))
}

fn apply_patch_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| Regex::new(r"\bapply_patch\b").expect("valid apply patch pattern"))
}

fn bash_score_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(r"\b(?:rm|mv|cp|git\s+(?:add|commit)|npm\s+install|sed\s+-i|perl\s+-pi|tee|cat\s*>|apply_patch)\b")
            .expect("valid bash score pattern")
    })
}

fn python_file_operation_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(
            r"^(?:await\s+)?([A-Za-z_][A-Za-z0-9_]*)\.(write_text|write_bytes|read_text|read_bytes|mkdir|unlink|rename|replace|touch)\s*\(",
        )
        .expect("valid python file operation pattern")
    })
}

fn subprocess_shell_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(r#"subprocess\.(?:run|check_call|check_output|Popen)\(\s*["`]([^"`]+)["`]"#)
            .expect("valid subprocess shell pattern")
    })
}

fn subprocess_list_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(r"subprocess\.(?:run|check_call|check_output|Popen)\(\s*\[([^\]]+)\]")
            .expect("valid subprocess list pattern")
    })
}

fn quoted_word_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| Regex::new(r#"["']([^"']+)["']"#).expect("valid quoted word pattern"))
}

fn shell_words_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| Regex::new(r#""([^"]*)"|'([^']*)'|(\S+)"#).expect("valid shell words pattern"))
}

fn print_call_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| Regex::new(r"^print\((.*)\)$").expect("valid print pattern"))
}

fn command_chain_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| Regex::new(r"\s*(?:&&|;)\s*").expect("valid chain pattern"))
}

fn node_modules_bin_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| Regex::new(r"\S*node_modules/\.bin/").expect("valid node_modules bin pattern"))
}

fn leading_dot_slash_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| Regex::new(r"^\./").expect("valid leading dot slash pattern"))
}

fn leading_whitespace_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| Regex::new(r"^\s*").expect("valid leading whitespace pattern"))
}

fn trailing_colon_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| Regex::new(r":\s*$").expect("valid trailing colon pattern"))
}

/// TypeScript `export type CodePreviewLanguage = "bash" | "python"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CodePreviewLanguage {
    Bash,
    Python,
}

impl CodePreviewLanguage {
    pub fn as_str(self) -> &'static str {
        match self {
            CodePreviewLanguage::Bash => "bash",
            CodePreviewLanguage::Python => "python",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CodePreview {
    pub language: CodePreviewLanguage,
    pub text: String,
}

struct PreviewCandidate {
    language: CodePreviewLanguage,
    text: String,
    score: i64,
    #[allow(dead_code)]
    index: usize,
}

// ---------------------------------------------------------------------------
// redactNoise / descriptor helpers.
//
// The TypeScript regexes use lookaround (`(?=...)`, `(?!...)`, `(?<!...)`),
// which the Rust `regex` crate does not support, so the passes below are
// hand-written scanners that keep the same left-to-right, first-match,
// greedy semantics.
// ---------------------------------------------------------------------------

fn is_word_char(character: char) -> bool {
    character.is_ascii_alphanumeric() || character == '_'
}

fn is_js_space(character: char) -> bool {
    character.is_whitespace() || character == '\u{FEFF}'
}

fn contains_redaction_keyword(identifier: &str) -> bool {
    let lowered = identifier.to_ascii_lowercase();
    lowered.contains("token") || lowered.contains("key") || lowered.contains("secret") || lowered.contains("password")
}

fn redact_blobs(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut index = 0usize;
    while index < chars.len() {
        let character = chars[index];
        if character.is_ascii_alphanumeric() || character == '+' || character == '/' {
            let mut end = index;
            while end < chars.len() {
                let candidate = chars[end];
                if candidate.is_ascii_alphanumeric() || candidate == '+' || candidate == '/' {
                    end += 1;
                } else {
                    break;
                }
            }
            let run = end - index;
            if run >= 80 {
                out.push_str("<blob>");
                let mut equals = 0usize;
                while end < chars.len() && chars[end] == '=' && equals < 2 {
                    end += 1;
                    equals += 1;
                }
                index = end;
                continue;
            }
        }
        out.push(character);
        index += 1;
    }
    out
}

/// `/\b((?=\w*(?:token|key|secret|password))[A-Za-z_]\w*)\s*=\s*(["'])[^"']*\2/gi`
fn redact_quoted_secrets(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut index = 0usize;
    while index < chars.len() {
        let character = chars[index];
        let at_boundary = index == 0 || !is_word_char(chars[index - 1]);
        if (character.is_ascii_alphabetic() || character == '_') && at_boundary {
            let mut end = index;
            while end < chars.len() && is_word_char(chars[end]) {
                end += 1;
            }
            let identifier: String = chars[index..end].iter().collect();
            if contains_redaction_keyword(&identifier) {
                let mut cursor = end;
                while cursor < chars.len() && is_js_space(chars[cursor]) {
                    cursor += 1;
                }
                if cursor < chars.len() && chars[cursor] == '=' {
                    cursor += 1;
                    while cursor < chars.len() && is_js_space(chars[cursor]) {
                        cursor += 1;
                    }
                    if cursor < chars.len() && (chars[cursor] == '"' || chars[cursor] == '\'') {
                        let quote = chars[cursor];
                        let mut value_end = cursor + 1;
                        while value_end < chars.len() && chars[value_end] != quote {
                            value_end += 1;
                        }
                        if value_end < chars.len() {
                            out.push_str(&identifier);
                            out.push_str("=<redacted>");
                            index = value_end + 1;
                            continue;
                        }
                    }
                }
            }
        }
        out.push(character);
        index += 1;
    }
    out
}

/// `/\b((?=\w*(?:token|key|secret|password))[A-Za-z_]\w*)\s*=\s*(?!<redacted>)(?!["'])\S+/gi`
fn redact_unquoted_secrets(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut index = 0usize;
    while index < chars.len() {
        let character = chars[index];
        let at_boundary = index == 0 || !is_word_char(chars[index - 1]);
        if (character.is_ascii_alphabetic() || character == '_') && at_boundary {
            let mut end = index;
            while end < chars.len() && is_word_char(chars[end]) {
                end += 1;
            }
            let identifier: String = chars[index..end].iter().collect();
            if contains_redaction_keyword(&identifier) {
                let mut cursor = end;
                while cursor < chars.len() && is_js_space(chars[cursor]) {
                    cursor += 1;
                }
                if cursor < chars.len() && chars[cursor] == '=' {
                    cursor += 1;
                    while cursor < chars.len() && is_js_space(chars[cursor]) {
                        cursor += 1;
                    }
                    let rest: String = chars[cursor..].iter().collect();
                    let is_already_redacted = rest.starts_with("<redacted>");
                    let starts_quote = cursor < chars.len() && (chars[cursor] == '"' || chars[cursor] == '\'');
                    let starts_value = cursor < chars.len() && !is_js_space(chars[cursor]);
                    if starts_value && !is_already_redacted && !starts_quote {
                        out.push_str(&identifier);
                        out.push_str("=<redacted>");
                        let mut value_end = cursor;
                        while value_end < chars.len() && !is_js_space(chars[value_end]) {
                            value_end += 1;
                        }
                        index = value_end;
                        continue;
                    }
                }
            }
        }
        out.push(character);
        index += 1;
    }
    out
}

/// `/\b(authorization:\s*(?:bearer\s+)?)[^\s"']+/gi`
fn redact_authorization_headers(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let lowered: Vec<char> = text.to_lowercase().chars().collect();
    let lowered_has_same_len = lowered.len() == chars.len();
    let mut out = String::with_capacity(text.len());
    let mut index = 0usize;
    while index < chars.len() {
        let at_boundary = index == 0 || !is_word_char(chars[index - 1]);
        if at_boundary && lowered_has_same_len {
            let rest: String = lowered[index..].iter().collect();
            if rest.starts_with("authorization:") {
                let mut cursor = index + "authorization:".len();
                while cursor < chars.len() && is_js_space(chars[cursor]) {
                    cursor += 1;
                }
                let after_spaces: String = lowered[cursor.min(lowered.len())..].iter().collect();
                let mut value_start = cursor;
                if after_spaces.starts_with("bearer") {
                    let mut probe = cursor + "bearer".len();
                    let next_is_word = probe < chars.len() && is_word_char(chars[probe]);
                    if !next_is_word {
                        while probe < chars.len() && is_js_space(chars[probe]) {
                            probe += 1;
                        }
                        value_start = probe;
                    }
                }
                let mut value_end = value_start;
                while value_end < chars.len()
                    && !is_js_space(chars[value_end])
                    && chars[value_end] != '"'
                    && chars[value_end] != '\''
                {
                    value_end += 1;
                }
                if value_end > value_start {
                    let prefix: String = chars[index..value_start].iter().collect();
                    out.push_str(&prefix);
                    out.push_str("<redacted>");
                    index = value_end;
                    continue;
                }
            }
        }
        out.push(chars[index]);
        index += 1;
    }
    out
}

/// `/(["'])sk-[^"']+\1/g`
fn redact_sk_tokens(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut index = 0usize;
    while index < chars.len() {
        let character = chars[index];
        if character == '"' || character == '\'' {
            let mut cursor = index + 1;
            if cursor + 3 <= chars.len() && chars[cursor] == 's' && chars[cursor + 1] == 'k' && chars[cursor + 2] == '-' {
                cursor += 3;
                let value_start = cursor;
                while cursor < chars.len() && chars[cursor] != character {
                    cursor += 1;
                }
                if cursor > value_start && cursor < chars.len() {
                    out.push(character);
                    out.push_str("<redacted>");
                    out.push(character);
                    index = cursor + 1;
                    continue;
                }
            }
        }
        out.push(character);
        index += 1;
    }
    out
}

/// `/(["']).{160,}\1/g` - `.` does not match line terminators in JavaScript.
fn redact_long_quoted_runs(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut index = 0usize;
    while index < chars.len() {
        let character = chars[index];
        if character == '"' || character == '\'' {
            let mut line_end = index + 1;
            while line_end < chars.len() && !matches!(chars[line_end], '\n' | '\r' | '\u{2028}' | '\u{2029}') {
                line_end += 1;
            }
            let mut closing = None;
            let mut probe = line_end;
            while probe > index + 160 {
                if chars[probe - 1] == character {
                    closing = Some(probe - 1);
                    break;
                }
                probe -= 1;
            }
            if let Some(closing) = closing {
                out.push(character);
                out.push('\u{2026}');
                out.push(character);
                index = closing + 1;
                continue;
            }
        }
        out.push(character);
        index += 1;
    }
    out
}

fn redact_noise(text: &str) -> String {
    let text = redact_blobs(text);
    let text = redact_quoted_secrets(&text);
    let text = redact_unquoted_secrets(&text);
    let text = redact_authorization_headers(&text);
    let text = redact_sk_tokens(&text);
    redact_long_quoted_runs(&text)
}

fn collapse_whitespace(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_space = false;
    for character in text.chars() {
        if is_js_space(character) {
            if !in_space {
                out.push(' ');
                in_space = true;
            }
        } else {
            out.push(character);
            in_space = false;
        }
    }
    out.trim().to_string()
}

fn truncate_descriptor(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= DESCRIPTOR_MAX_WIDTH {
        return text.to_string();
    }
    let head: String = chars[..DESCRIPTOR_MAX_WIDTH - 1].iter().collect();
    format!("{}\u{2026}", head.trim_end())
}

fn descriptor(text: &str) -> String {
    truncate_descriptor(&collapse_whitespace(&redact_noise(text)))
}

// ---------------------------------------------------------------------------
// Bash preview
// ---------------------------------------------------------------------------

fn strip_bash_prefix(line: &str) -> String {
    let without_magic = magic_line_pattern().replace(line, "");
    let trimmed = without_magic.trim();
    let without_cd = cd_prefix_pattern().replace(trimmed, "");
    without_cd.trim().to_string()
}

fn is_skippable_bash_line(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.is_empty()
        || comment_line_pattern().is_match(trimmed)
        || bash_set_pattern().is_match(trimmed)
        || bash_setup_pattern().is_match(trimmed)
}

fn shell_words(line: &str) -> Vec<String> {
    let mut words = Vec::new();
    for caps in shell_words_pattern().captures_iter(line) {
        let value = caps
            .get(1)
            .or_else(|| caps.get(2))
            .or_else(|| caps.get(3))
            .map(|group| group.as_str())
            .unwrap_or("");
        words.push(value.to_string());
    }
    words
}

fn path_tail(path: &str) -> String {
    leading_dot_slash_pattern().replace(path, "").into_owned()
}

fn simplify_runner_command(line: &str) -> Option<String> {
    let words = shell_words(line);
    let joined = words.join(" ");
    let vitest_index = words.iter().position(|word| vitest_cli_pattern().is_match(word));
    if words.first().map(String::as_str) == Some("npx")
        && words.get(1).map(String::as_str) == Some("tsx")
        && vitest_index.map(|index| index >= 2).unwrap_or(false)
    {
        let index = vitest_index.expect("vitest index");
        let tail = words[index + 1..].join(" ");
        return Some(format!("vitest {tail}").trim().to_string());
    }
    if words.first().map(String::as_str) == Some("npm") {
        let prefix_index = words.iter().position(|word| word == "--prefix");
        let cwd = prefix_index.and_then(|index| words.get(index + 1)).cloned();
        let run_index = words.iter().position(|word| word == "run");
        if let Some(run_index) = run_index {
            if let Some(subcommand) = words.get(run_index + 1) {
                let tail = words[run_index + 2..].join(" ");
                let command = format!("npm {subcommand} {tail}").trim().to_string();
                return Some(match cwd {
                    Some(cwd) => format!("{command} ({})", path_tail(&cwd)),
                    None => command,
                });
            }
        }
    }
    if words.first().map(String::as_str) == Some("pnpm") {
        let cwd_index = words.iter().position(|word| word == "-C" || word == "--dir");
        let cwd = cwd_index.and_then(|index| words.get(index + 1)).cloned();
        let rest: Vec<String> = words
            .iter()
            .enumerate()
            .filter(|(index, _)| match cwd_index {
                Some(cwd_index) => *index != cwd_index && *index != cwd_index + 1,
                None => true,
            })
            .map(|(_, word)| word.clone())
            .collect();
        return match cwd {
            Some(cwd) => Some(format!("{} ({})", rest.join(" "), path_tail(&cwd))),
            None => None,
        };
    }
    // The TypeScript predicate is `word === "pytest" || (word === "pytest" && words[index - 1] === "-m")`,
    // which reduces to `word === "pytest"`.
    let pytest_index = words.iter().position(|word| word == "pytest");
    if words.first().map(String::as_str) == Some("uv") && words.get(1).map(String::as_str) == Some("run") {
        if let Some(pytest_index) = pytest_index {
            let tail = words[pytest_index + 1..].join(" ");
            return Some(format!("pytest {tail}").trim().to_string());
        }
    }
    if matches!(words.first().map(String::as_str), Some("python") | Some("python3"))
        && words.get(1).map(String::as_str) == Some("-m")
        && words.get(2).map(String::as_str) == Some("pytest")
    {
        let tail = words[3..].join(" ");
        return Some(format!("pytest {tail}").trim().to_string());
    }
    if joined.contains("node_modules/.bin/") {
        return Some(node_modules_bin_pattern().replace_all(&joined, "").into_owned());
    }
    None
}

fn simplify_mutation_command(line: &str) -> Option<String> {
    let words = shell_words(line);
    if words.is_empty() {
        return None;
    }
    if words.first().map(String::as_str) == Some("cat") && words.get(1).map(String::as_str) == Some(">") {
        if let Some(target) = words.get(2) {
            return Some(format!("write {}", path_tail(target)));
        }
    }
    if words.first().map(String::as_str) == Some("tee") {
        if let Some(last) = words.last() {
            let verb = if words.iter().any(|word| word == "-a") { "append" } else { "write" };
            return Some(format!("{verb} {}", path_tail(last)));
        }
    }
    if words.first().map(String::as_str) == Some("apply_patch") {
        return Some("apply patch".to_string());
    }
    if matches!(words.first().map(String::as_str), Some("rm") | Some("mv") | Some("cp") | Some("git") | Some("npm")) {
        return Some(line.to_string());
    }
    if (words.first().map(String::as_str) == Some("sed") && words.iter().any(|word| word.starts_with("-i")))
        || (words.first().map(String::as_str) == Some("perl") && words.iter().any(|word| word == "-pi"))
    {
        return Some(line.to_string());
    }
    None
}

fn simplify_bash_command_line(line: &str) -> String {
    simplify_runner_command(line)
        .or_else(|| simplify_mutation_command(line))
        .unwrap_or_else(|| line.to_string())
}

fn split_command_chain(line: &str) -> Vec<String> {
    command_chain_pattern()
        .split(line)
        .map(|part| part.trim().to_string())
        .filter(|part| !part.is_empty())
        .collect()
}

fn heredoc_body(lines: &[String], start_index: usize, delimiter: &str) -> Option<String> {
    // While args stream, preview the partial heredoc body rather than the low-signal heredoc opener.
    let mut body: Vec<String> = Vec::new();
    let mut index = start_index + 1;
    while index < lines.len() {
        let line = lines.get(index).cloned().unwrap_or_default();
        if line.trim() == delimiter {
            return Some(body.join("\n"));
        }
        body.push(line);
        index += 1;
    }
    if body.is_empty() {
        None
    } else {
        Some(body.join("\n"))
    }
}

fn preview_heredoc(lines: &[String]) -> Option<CodePreview> {
    // A generic heredoc body is low-signal; keep it as a fallback and prefer a
    // later, more specific heredoc (python/bash/node/write) if one follows.
    let mut fallback: Option<CodePreview> = None;
    for index in 0..lines.len() {
        let line = strip_bash_prefix(lines.get(index).map(String::as_str).unwrap_or(""));
        if is_skippable_bash_line(&line) {
            continue;
        }
        let delimiter = heredoc_pattern()
            .captures(&line)
            .and_then(|caps| caps.get(1))
            .map(|group| group.as_str().to_string());
        let Some(delimiter) = delimiter else {
            continue;
        };
        let Some(body) = heredoc_body(lines, index, &delimiter) else {
            continue;
        };
        if python_runner_pattern().is_match(&line) {
            let preview = preview_python_code(&body);
            if !preview.text.is_empty() {
                return Some(preview);
            }
            continue;
        }
        // Match bash/sh as an interpreter word (incl. /bin/sh), not a path suffix like script.sh.
        if has_bash_interpreter_word(&line) {
            let preview = preview_bash_command(&body);
            return Some(if !preview.text.is_empty() {
                preview
            } else {
                CodePreview {
                    language: CodePreviewLanguage::Bash,
                    text: descriptor(&body),
                }
            });
        }
        if node_word_pattern().is_match(&line) {
            return Some(CodePreview {
                language: CodePreviewLanguage::Bash,
                text: format!("node: {}", descriptor(&body)),
            });
        }
        let cat_write = cat_write_pattern()
            .captures(&line)
            .and_then(|caps| caps.get(1))
            .map(|group| group.as_str().to_string());
        if let Some(target) = cat_write {
            let verb = if line.contains("tee -a") { "append" } else { "write" };
            return Some(CodePreview {
                language: CodePreviewLanguage::Bash,
                text: format!("{verb} {}", path_tail(&target)),
            });
        }
        if apply_patch_pattern().is_match(&line) {
            return Some(CodePreview {
                language: CodePreviewLanguage::Bash,
                text: "apply patch".to_string(),
            });
        }
        if fallback.is_none() {
            fallback = Some(CodePreview {
                language: CodePreviewLanguage::Bash,
                text: descriptor(&body),
            });
        }
    }
    fallback
}

/// `/(?<![\w.])(?:bash|sh)\b/` - the lookbehind is expressed as a manual guard.
fn bash_interpreter_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| Regex::new(r"(?:bash|sh)\b").expect("valid bash interpreter pattern"))
}

fn has_bash_interpreter_word(line: &str) -> bool {
    for found in bash_interpreter_pattern().find_iter(line) {
        let start = found.start();
        let preceded_by_word_or_dot = if start == 0 {
            false
        } else {
            let previous = line[..start].chars().next_back().unwrap_or(' ');
            is_word_char(previous) || previous == '.'
        };
        if !preceded_by_word_or_dot {
            return true;
        }
    }
    false
}

fn bash_line_score(line: &str, index: usize) -> i64 {
    let simplified = simplify_bash_command_line(line);
    let words = shell_words(line);
    let mut score: i64 = 30;
    if simplified != line {
        score += 40;
    }
    if matches!(
        words.first().map(String::as_str),
        Some("rm") | Some("mv") | Some("cp") | Some("git") | Some("npm") | Some("pnpm") | Some("pytest") | Some("vitest")
    ) {
        score += 20;
    }
    if bash_score_pattern().is_match(line) {
        score += 40;
    }
    score + index as i64
}

pub fn preview_bash_command(command: &str) -> CodePreview {
    let lines: Vec<String> = command.split('\n').map(|line| line.to_string()).collect();
    if let Some(heredoc) = preview_heredoc(&lines) {
        if !heredoc.text.is_empty() {
            return CodePreview {
                language: heredoc.language,
                text: descriptor(&heredoc.text),
            };
        }
    }

    let mut best: Option<PreviewCandidate> = None;
    let mut index = 0usize;
    for raw_line in &lines {
        for raw_part in split_command_chain(raw_line) {
            let command_line = strip_bash_prefix(raw_part.trim());
            if command_line.is_empty() || is_skippable_bash_line(&command_line) {
                continue;
            }
            let candidate = PreviewCandidate {
                language: CodePreviewLanguage::Bash,
                text: simplify_bash_command_line(&command_line),
                score: bash_line_score(&command_line, index),
                index,
            };
            if best.as_ref().map(|current| candidate.score > current.score).unwrap_or(true) {
                best = Some(candidate);
            }
            index += 1;
        }
    }
    CodePreview {
        language: CodePreviewLanguage::Bash,
        text: best.map(|candidate| descriptor(&candidate.text)).unwrap_or_default(),
    }
}

// ---------------------------------------------------------------------------
// Python preview
// ---------------------------------------------------------------------------

fn is_skippable_python_line(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.is_empty() || comment_line_pattern().is_match(trimmed) || python_import_pattern().is_match(trimmed)
}

fn python_indent(line: &str) -> usize {
    leading_whitespace_pattern()
        .find(line)
        .map(|found| found.as_str().chars().count())
        .unwrap_or(0)
}

fn python_print_inner_call(line: &str) -> Option<String> {
    let trimmed = line.trim();
    let inner = print_call_pattern()
        .captures(trimmed)
        .and_then(|caps| caps.get(1))
        .map(|group| group.as_str().trim().to_string());
    match inner {
        Some(inner) if !inner.is_empty() && python_call_pattern().is_match(&inner) => Some(inner),
        _ => None,
    }
}

fn python_path_vars(lines: &[String]) -> HashMap<String, String> {
    let mut vars: HashMap<String, String> = HashMap::new();
    for line in lines {
        let captures = path_assign_pattern().captures(line).or_else(|| string_assign_pattern().captures(line));
        if let Some(caps) = captures {
            let name = caps.get(1).map(|group| group.as_str().to_string());
            let value = caps.get(2).map(|group| group.as_str().to_string());
            if let (Some(name), Some(value)) = (name, value) {
                if value.contains('/') {
                    vars.insert(name, value);
                }
            }
        }
    }
    vars
}

fn python_file_operation(line: &str, paths: &HashMap<String, String>) -> Option<String> {
    let caps = python_file_operation_pattern().captures(line.trim())?;
    let variable = caps.get(1)?.as_str();
    let operation = caps.get(2)?.as_str();
    let path = paths.get(variable)?;
    let action = match operation {
        "write_text" | "write_bytes" => "write",
        "read_text" | "read_bytes" => "read",
        "mkdir" => "mkdir",
        "unlink" => "delete",
        "rename" => "rename",
        "replace" => "replace",
        "touch" => "touch",
        other => other,
    };
    Some(format!("{action} {}", path_tail(path)))
}

fn python_subprocess_command(line: &str) -> Option<String> {
    let trimmed = line.trim();
    if let Some(caps) = subprocess_shell_pattern().captures(trimmed) {
        if let Some(command) = caps.get(1) {
            return Some(simplify_bash_command_line(command.as_str()));
        }
    }
    if let Some(caps) = subprocess_list_pattern().captures(trimmed) {
        if let Some(list) = caps.get(1) {
            let words: Vec<String> = quoted_word_pattern()
                .captures_iter(list.as_str())
                .filter_map(|caps| caps.get(1).map(|group| group.as_str().to_string()))
                .collect();
            return Some(simplify_bash_command_line(&words.join(" ")));
        }
    }
    None
}

fn simplify_python_preview_line(line: &str, paths: &HashMap<String, String>) -> String {
    python_file_operation(line, paths)
        .or_else(|| python_subprocess_command(line))
        .or_else(|| python_print_inner_call(line))
        .unwrap_or_else(|| line.trim().to_string())
}

fn python_preview_line(lines: &[String], index: usize, paths: &HashMap<String, String>) -> String {
    let line = lines.get(index).cloned().unwrap_or_default();
    if index > 0 && python_definition_pattern().is_match(&line) {
        let previous = lines.get(index - 1).cloned().unwrap_or_default();
        if python_decorator_pattern().is_match(previous.trim()) {
            return format!("{} {}", previous.trim(), line.trim());
        }
    }
    if python_control_pattern().is_match(&line) {
        if let Some(child_index) = first_python_child_line(lines, index) {
            let child = lines.get(child_index).cloned().unwrap_or_default();
            let head = trailing_colon_pattern().replace(line.trim(), ":").into_owned();
            return format!("{head} {}", simplify_python_preview_line(&child, paths));
        }
    }
    simplify_python_preview_line(&line, paths)
}

fn first_python_child_line(lines: &[String], parent_index: usize) -> Option<usize> {
    let parent_indent = python_indent(lines.get(parent_index).map(String::as_str).unwrap_or(""));
    let mut index = parent_index + 1;
    while index < lines.len() {
        let line = lines.get(index).cloned().unwrap_or_default();
        if is_skippable_python_line(&line) || python_decorator_pattern().is_match(line.trim()) {
            index += 1;
            continue;
        }
        if python_indent(&line) <= parent_indent {
            return None;
        }
        return Some(index);
    }
    None
}

fn python_line_score(lines: &[String], index: usize, paths: &HashMap<String, String>) -> i64 {
    let line = lines.get(index).cloned().unwrap_or_default();
    let trimmed = line.trim();
    if is_skippable_python_line(&line) || python_decorator_pattern().is_match(trimmed) {
        return -1;
    }
    if python_file_operation(&line, paths).is_some() {
        return 95;
    }
    if python_subprocess_command(&line).is_some() {
        return 90;
    }
    if python_main_pattern().is_match(&line) {
        return 70;
    }
    if python_effect_call_pattern().is_match(&line) {
        return 80;
    }
    if python_control_pattern().is_match(&line) {
        return match first_python_child_line(lines, index) {
            None => 20,
            Some(child_index) => std::cmp::max(20, python_line_score(lines, child_index, paths) - 5),
        };
    }
    if python_definition_pattern().is_match(&line) {
        return 50;
    }
    if python_low_signal_assignment_call_pattern().is_match(&line) {
        return 25;
    }
    if let Some(print_inner_call) = python_print_inner_call(&line) {
        if !python_low_signal_call_pattern().is_match(&print_inner_call) {
            return 55;
        }
    }
    if python_assignment_call_pattern().is_match(&line) {
        return 60;
    }
    if python_call_pattern().is_match(&line) && !python_low_signal_call_pattern().is_match(&line) {
        return 65;
    }
    if python_call_pattern().is_match(&line) {
        return 15;
    }
    30
}

fn python_preview_index(lines: &[String], index: usize) -> usize {
    let line = lines.get(index).cloned().unwrap_or_default();
    if !python_control_pattern().is_match(&line) {
        return index;
    }
    match first_python_child_line(lines, index) {
        None => index,
        Some(child_index) => python_preview_index(lines, child_index),
    }
}

fn python_escape(next: char) -> Option<&'static str> {
    match next {
        // backslash-newline is a line continuation
        '\n' => Some(""),
        '"' => Some("\""),
        '\'' => Some("'"),
        '\\' => Some("\\"),
        'n' => Some("\n"),
        'r' => Some("\r"),
        't' => Some("\t"),
        _ => None,
    }
}

fn is_unsupported_escape(next: char) -> bool {
    matches!(next, 'x' | 'u' | 'U' | 'N' | '0'..='7' | 'a' | 'b' | 'f' | 'v')
}

struct PythonStringScan {
    value: String,
    end: usize,
    closed: bool,
    /// Saw a cooked escape (\x, \u, octal, \a...) whose value is not computed here.
    unsupported_escape: bool,
}

/// Walks a python string-literal body from just after the opening delimiter,
/// following python's escape rules (in raw strings backslash-quote never closes).
fn scan_python_string_literal(code: &str, start: usize, quote: &str, raw: bool) -> PythonStringScan {
    let chars: Vec<char> = code.chars().collect();
    let mut value = String::new();
    let mut index = start;
    let mut unsupported_escape = false;
    let quote_chars: Vec<char> = quote.chars().collect();
    while index < chars.len() {
        let character = chars[index];
        if character == '\\' && index + 1 < chars.len() {
            let next = chars[index + 1];
            if !raw && is_unsupported_escape(next) {
                unsupported_escape = true;
            }
            if raw {
                value.push(character);
                value.push(next);
            } else {
                match python_escape(next) {
                    Some(mapped) => value.push_str(mapped),
                    None => {
                        value.push(character);
                        value.push(next);
                    }
                }
            }
            index += 2;
            continue;
        }
        if starts_with_at(&chars, index, &quote_chars) {
            return PythonStringScan {
                value,
                end: index + quote_chars.len(),
                closed: true,
                unsupported_escape,
            };
        }
        if quote_chars.len() == 1 && character == '\n' {
            break; // single-quoted literals cannot span lines
        }
        value.push(character);
        index += 1;
    }
    PythonStringScan {
        value,
        end: index,
        closed: false,
        unsupported_escape,
    }
}

fn starts_with_at(chars: &[char], index: usize, needle: &[char]) -> bool {
    if index + needle.len() > chars.len() {
        return false;
    }
    chars[index..index + needle.len()] == *needle
}

/// True when the lines end inside an unterminated triple-quoted string.
fn ends_inside_multiline_string(lines: &[String]) -> bool {
    let text = lines.join("\n");
    let chars: Vec<char> = text.chars().collect();
    let mut index = 0usize;
    while index < chars.len() {
        let character = chars[index];
        if character == '#' {
            let newline = chars[index..].iter().position(|&c| c == '\n');
            match newline {
                None => return false,
                Some(offset) => {
                    index = index + offset + 1;
                    continue;
                }
            }
        }
        if character == '"' || character == '\'' {
            let triple: Vec<char> = vec![character, character, character];
            let quote_chars: Vec<char> = if starts_with_at(&chars, index, &triple) {
                triple
            } else {
                vec![character]
            };
            let quote: String = quote_chars.iter().collect();
            let scan = scan_python_string_literal(&text, index + quote_chars.len(), &quote, true);
            if !scan.closed && scan.end >= chars.len() {
                return quote_chars.len() == 3;
            }
            index = scan.end;
            continue;
        }
        index += 1;
    }
    false
}

fn extract_bash_skill_command(code: &str) -> Option<String> {
    let caps = bash_skill_call_pattern().captures(code)?;
    let whole = caps.get(0)?;
    let quote = caps.get(1)?.as_str().to_string();
    let start = whole.as_str().chars().count();
    let chars: Vec<char> = code.chars().collect();
    let prefix_index = start.checked_sub(quote.chars().count() + 1)?;
    let prefix_char = *chars.get(prefix_index)?;
    let scan = scan_python_string_literal(code, start, &quote, prefix_char == 'r' || prefix_char == 'R');
    if !scan.closed || scan.unsupported_escape {
        return None;
    }
    let rest: String = chars[scan.end.min(chars.len())..].iter().collect();
    let rest = rest.trim_start();
    // Require a plain literal first argument; concatenation or other expressions fall back.
    if !rest.starts_with(',') && !rest.starts_with(')') {
        return None;
    }
    Some(scan.value)
}

pub fn preview_python_code(code: &str) -> CodePreview {
    let lines: Vec<String> = code.split('\n').map(|line| line.to_string()).collect();
    let paths = python_path_vars(&lines);
    let mut best_index: Option<usize> = None;
    let mut best_score: i64 = -1;

    for index in 0..lines.len() {
        let score = python_line_score(&lines, index, &paths);
        if score > best_score {
            best_index = Some(index);
            best_score = score;
        }
    }

    if let Some(best_index) = best_index {
        if best_score >= 0 {
            let preview_index = python_preview_index(&lines, best_index);
            // Extract from the full tail (literals may span lines), unless the chosen line is string text.
            let head: Vec<String> = lines[..preview_index].to_vec();
            let bash_command = if ends_inside_multiline_string(&head) {
                None
            } else {
                extract_bash_skill_command(&lines[preview_index..].join("\n"))
            };
            if let Some(bash_command) = bash_command {
                return preview_bash_command(&bash_command);
            }
            return CodePreview {
                language: CodePreviewLanguage::Python,
                text: descriptor(&python_preview_line(&lines, preview_index, &paths)),
            };
        }
    }
    CodePreview {
        language: CodePreviewLanguage::Python,
        text: String::new(),
    }
}

pub fn preview_ipython_code(code: &str) -> CodePreview {
    let trimmed_code = code.trim_end();
    if let Some(bash_cell) = parse_ipython_bash_cell(trimmed_code) {
        return preview_bash_command(&bash_cell.body);
    }
    preview_python_code(trimmed_code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptor_collapses_whitespace_and_truncates() {
        assert_eq!(descriptor("  a\n\n  b  "), "a b");
        // A continuous 80-character token is redacted before truncation.
        let long = "abcdefghij ".repeat(10);
        let truncated = descriptor(&long);
        assert_eq!(truncated.chars().count(), DESCRIPTOR_MAX_WIDTH);
        assert!(truncated.ends_with('\u{2026}'));
    }

    #[test]
    fn redact_noise_masks_secrets_and_blobs() {
        assert_eq!(descriptor("API_TOKEN=abc123"), "API_TOKEN=<redacted>");
        assert_eq!(descriptor("api_key = \"abc123\""), "api_key=<redacted>");
        assert_eq!(descriptor("Authorization: Bearer abc.def"), "Authorization: Bearer <redacted>");
        assert_eq!(descriptor("x='sk-abcdef'"), "x='<redacted>'");
        let blob = "A".repeat(120);
        assert_eq!(descriptor(&format!("data {blob}")), "data <blob>");
    }

    #[test]
    fn preview_bash_command_simplifies_runner_commands() {
        let preview = preview_bash_command("npx tsx ../../node_modules/vitest/dist/cli.js --run test/a.test.ts");
        assert_eq!(preview.language, CodePreviewLanguage::Bash);
        assert_eq!(preview.text, "vitest --run test/a.test.ts");

        let npm = preview_bash_command("npm --prefix packages/coding-agent run check");
        assert_eq!(npm.text, "npm check (packages/coding-agent)");

        let pytest = preview_bash_command("uv run pytest -q tests");
        assert_eq!(pytest.text, "pytest -q tests");
    }

    #[test]
    fn preview_bash_command_skips_setup_lines_and_uses_latest_high_score() {
        let preview = preview_bash_command("set -e\ncd /tmp && ls\nrm -rf build");
        assert_eq!(preview.text, "rm -rf build");
    }

    #[test]
    fn preview_bash_command_prefers_heredoc_body() {
        let command = "python3 - <<'PY'\nfrom pathlib import Path\nPath('/tmp/x').write_text('hi')\nPY\n";
        let preview = preview_bash_command(command);
        assert_eq!(preview.language, CodePreviewLanguage::Python);
        assert_eq!(preview.text, "Path('/tmp/x').write_text('hi')");
    }

    #[test]
    fn preview_bash_command_mutation_forms() {
        assert_eq!(preview_bash_command("cat > out.txt <<'EOF'\nbody\nEOF").text, "write out.txt");
        assert_eq!(preview_bash_command("tee -a out.txt <<'EOF'\nbody\nEOF").text, "append out.txt");
        assert_eq!(preview_bash_command("apply_patch <<'P'\n*** Begin Patch\nP").text, "apply patch");
    }

    #[test]
    fn preview_python_code_scores_side_effects() {
        let code = "import os\np = Path('/tmp/a.txt')\np.write_text('x')\nprint('done')\n";
        let preview = preview_python_code(code);
        assert_eq!(preview.language, CodePreviewLanguage::Python);
        assert_eq!(preview.text, "write /tmp/a.txt");
    }

    #[test]
    fn preview_python_code_prefers_effectful_control_child() {
        let code = "if True:\n    subprocess.run('rm -rf build', shell=True)\n";
        let preview = preview_python_code(code);
        assert_eq!(preview.text, "subprocess.run('rm -rf build', shell=True)");
        assert_eq!(preview_python_code("if True:\n    subprocess.run(\"rm -rf build\", shell=True)\n").text, "rm -rf build");
    }

    #[test]
    fn preview_python_code_extracts_bash_skill_command() {
        let code = "await bash(\"ls -la\")\n";
        let preview = preview_python_code(code);
        assert_eq!(preview.language, CodePreviewLanguage::Bash);
        assert_eq!(preview.text, "ls -la");
    }

    #[test]
    fn preview_python_code_returns_empty_for_imports_only() {
        let preview = preview_python_code("import os\nfrom pathlib import Path\n");
        assert_eq!(preview.text, "");
    }

    #[test]
    fn preview_ipython_code_uses_bash_cell_body() {
        let preview = preview_ipython_code("%%bash\nrm -rf build\n");
        assert_eq!(preview.language, CodePreviewLanguage::Bash);
        assert_eq!(preview.text, "rm -rf build");
    }

    #[test]
    fn ends_inside_multiline_string_detects_open_triple_quote() {
        assert!(ends_inside_multiline_string(&["x = \"\"\"abc".to_string()]));
        assert!(!ends_inside_multiline_string(&["x = \"\"\"abc\"\"\"".to_string()]));
        assert!(!ends_inside_multiline_string(&["# \"\"\"".to_string()]));
    }

    #[test]
    fn shell_words_respects_quotes() {
        assert_eq!(shell_words("a \"b c\" 'd e'"), vec!["a", "b c", "d e"]);
    }
}
