//! Port of packages/coding-agent/src/modes/agents-view/session-view-search.ts
//!
//! Regex mode uses the `regex` crate. JavaScript `RegExp` and Rust `regex` differ
//! in syntax details, so an invalid pattern reports the Rust error message instead
//! of the V8 one; the "Empty regex" literal and the case-insensitive default match
//! the reference. `text.search(re)` returns a UTF-16 code-unit index in JS and a
//! byte index here, which only changes the score, never `matches`.

use regex::{Regex, RegexBuilder};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SearchTokenKind {
    Fuzzy,
    Phrase,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchToken {
    pub kind: SearchTokenKind,
    pub value: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SearchMode {
    Tokens,
    Regex,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ParsedSearchQuery {
    pub mode: SearchMode,
    pub tokens: Vec<SearchToken>,
    pub regex: Option<Regex>,
    /// If set, parsing failed and we should treat query as non-matching.
    pub error: Option<String>,
}

impl ParsedSearchQuery {
    fn tokens(tokens: Vec<SearchToken>) -> Self {
        Self { mode: SearchMode::Tokens, tokens, regex: None, error: None }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MatchResult {
    pub matches: bool,
    /// Lower is better; only meaningful when matches === true
    pub score: f64,
}

fn normalize_whitespace_lower(text: &str) -> String {
    let lowered = text.to_lowercase();
    let mut out = String::with_capacity(lowered.len());
    let mut in_ws = false;
    for ch in lowered.chars() {
        if ch.is_whitespace() {
            in_ws = true;
        } else {
            if in_ws && !out.is_empty() {
                out.push(' ');
            }
            in_ws = false;
            out.push(ch);
        }
    }
    out
}

/// Join arbitrary session fields into the common search corpus.
pub fn create_session_search_text(parts: &[Option<&str>]) -> String {
    parts
        .iter()
        .flatten()
        .filter(|part| !part.is_empty())
        .copied()
        .collect::<Vec<&str>>()
        .join(" ")
}

pub fn parse_search_query(query: &str) -> ParsedSearchQuery {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return ParsedSearchQuery::tokens(Vec::new());
    }

    // Regex mode: re:<pattern>
    if let Some(rest) = trimmed.strip_prefix("re:") {
        let pattern = rest.trim();
        if pattern.is_empty() {
            return ParsedSearchQuery {
                mode: SearchMode::Regex,
                tokens: Vec::new(),
                regex: None,
                error: Some("Empty regex".to_string()),
            };
        }
        match RegexBuilder::new(pattern).case_insensitive(true).build() {
            Ok(regex) => {
                return ParsedSearchQuery {
                    mode: SearchMode::Regex,
                    tokens: Vec::new(),
                    regex: Some(regex),
                    error: None,
                }
            }
            Err(error) => {
                return ParsedSearchQuery {
                    mode: SearchMode::Regex,
                    tokens: Vec::new(),
                    regex: None,
                    error: Some(error.to_string()),
                }
            }
        }
    }

    // Token mode with quote support.
    // Example: foo "node cve" bar
    let mut tokens: Vec<SearchToken> = Vec::new();
    let mut buf = String::new();
    let mut in_quote = false;
    let mut had_unclosed_quote = false;

    let flush = |buf: &mut String, kind: SearchTokenKind, tokens: &mut Vec<SearchToken>| {
        let value = buf.trim().to_string();
        buf.clear();
        if value.is_empty() {
            return;
        }
        tokens.push(SearchToken { kind, value });
    };

    for ch in trimmed.chars() {
        if ch == '"' {
            if in_quote {
                flush(&mut buf, SearchTokenKind::Phrase, &mut tokens);
                in_quote = false;
            } else {
                flush(&mut buf, SearchTokenKind::Fuzzy, &mut tokens);
                in_quote = true;
            }
            continue;
        }
        if !in_quote && ch.is_whitespace() {
            flush(&mut buf, SearchTokenKind::Fuzzy, &mut tokens);
            continue;
        }
        buf.push(ch);
    }

    if in_quote {
        had_unclosed_quote = true;
    }

    // If quotes were unbalanced, fall back to plain whitespace tokenization.
    if had_unclosed_quote {
        let tokens = trimmed
            .split_whitespace()
            .map(|token| token.trim())
            .filter(|token| !token.is_empty())
            .map(|token| SearchToken { kind: SearchTokenKind::Fuzzy, value: token.to_string() })
            .collect();
        return ParsedSearchQuery::tokens(tokens);
    }

    flush(
        &mut buf,
        if in_quote { SearchTokenKind::Phrase } else { SearchTokenKind::Fuzzy },
        &mut tokens,
    );

    ParsedSearchQuery::tokens(tokens)
}

/// Local port of `fuzzyMatch` from `@earendil-works/pi-tui` (packages/tui/src/fuzzy.ts).
///
/// TODO(port): pi-tui::fuzzy::fuzzy_match is empty in this workspace, so the
/// scorer lives here until that slice lands. Move the call site then.
pub fn fuzzy_match(query: &str, text: &str) -> MatchResult {
    let query_lower = query.to_lowercase();
    let text_lower = text.to_lowercase();

    let match_query = |normalized_query: &str| -> MatchResult {
        if normalized_query.is_empty() {
            return MatchResult { matches: true, score: 0.0 };
        }
        if normalized_query.len() > text_lower.len() {
            return MatchResult { matches: false, score: 0.0 };
        }

        let query_chars: Vec<char> = normalized_query.chars().collect();
        let text_chars: Vec<char> = text_lower.chars().collect();
        let mut query_index = 0usize;
        let mut score = 0.0f64;
        let mut last_match_index: i64 = -1;
        let mut consecutive_matches = 0i64;

        for (i, ch) in text_chars.iter().enumerate() {
            if query_index >= query_chars.len() {
                break;
            }
            if *ch == query_chars[query_index] {
                let is_word_boundary = i == 0
                    || matches!(
                        text_chars[i - 1],
                        ' ' | '\t' | '\n' | '\r' | '-' | '_' | '.' | '/' | ':'
                    );

                if last_match_index == i as i64 - 1 {
                    consecutive_matches += 1;
                    score -= consecutive_matches as f64 * 5.0;
                } else {
                    consecutive_matches = 0;
                    if last_match_index >= 0 {
                        score += (i as i64 - last_match_index - 1) as f64 * 2.0;
                    }
                }

                if is_word_boundary {
                    score -= 10.0;
                }

                score += i as f64 * 0.1;

                last_match_index = i as i64;
                query_index += 1;
            }
        }

        if query_index < query_chars.len() {
            return MatchResult { matches: false, score: 0.0 };
        }
        if normalized_query == text_lower {
            score -= 100.0;
        }
        MatchResult { matches: true, score }
    };

    let primary_match = match_query(&query_lower);
    if primary_match.matches {
        return primary_match;
    }

    let swapped_query = swapped_query_of(&query_lower).unwrap_or_default();
    if swapped_query.is_empty() {
        return primary_match;
    }
    let swapped_match = match_query(&swapped_query);
    if !swapped_match.matches {
        return primary_match;
    }
    MatchResult { matches: true, score: swapped_match.score + 5.0 }
}

/// `alphaNumericMatch` swaps letters+digits to digits+letters; `numericAlphaMatch`
/// swaps digits+letters to letters+digits; anything else has no swapped query.
fn swapped_query_of(value: &str) -> Option<String> {
    if let Some((letters, digits)) = split_exact(value, false) {
        return Some(format!("{digits}{letters}"));
    }
    split_exact(value, true).map(|(letters, digits)| format!("{letters}{digits}"))
}

/// Exact `^[a-z]+[0-9]+$` (digits_first = false) or `^[0-9]+[a-z]+$` (true).
fn split_exact(value: &str, digits_first: bool) -> Option<(String, String)> {
    let chars: Vec<char> = value.chars().collect();
    if chars.is_empty() {
        return None;
    }
    let first: fn(char) -> bool = if digits_first { |ch: char| ch.is_ascii_digit() } else { |ch: char| ch.is_ascii_lowercase() };
    let second: fn(char) -> bool = if digits_first { |ch: char| ch.is_ascii_lowercase() } else { |ch: char| ch.is_ascii_digit() };
    let boundary = chars.iter().position(|ch| second(*ch))?;
    if boundary == 0 || !chars[..boundary].iter().all(|ch| first(*ch)) {
        return None;
    }
    if !chars[boundary..].iter().all(|ch| second(*ch)) {
        return None;
    }
    let letters: String;
    let digits: String;
    if digits_first {
        digits = chars[..boundary].iter().collect();
        letters = chars[boundary..].iter().collect();
    } else {
        letters = chars[..boundary].iter().collect();
        digits = chars[boundary..].iter().collect();
    }
    Some((letters, digits))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_query_matches_everything() {
        let parsed = parse_search_query("   ");
        assert_eq!(parsed.mode, SearchMode::Tokens);
        assert!(parsed.tokens.is_empty());
        assert!(match_search_text("anything", &parsed).matches);
        assert!(matches_search_text("anything", ""));
    }

    #[test]
    fn regex_mode_is_case_insensitive_and_reports_errors() {
        let parsed = parse_search_query("re:no+de");
        assert_eq!(parsed.mode, SearchMode::Regex);
        assert!(parsed.error.is_none());
        let result = match_search_text("a NODE here", &parsed);
        assert!(result.matches);
        assert!((result.score - 0.2).abs() < 1e-9);

        let empty = parse_search_query("re:   ");
        assert_eq!(empty.error.as_deref(), Some("Empty regex"));
        assert!(!match_search_text("x", &empty).matches);

        let broken = parse_search_query("re:[");
        assert!(broken.error.is_some());
        assert!(!match_search_text("x", &broken).matches);
    }

    #[test]
    fn quoted_tokens_are_phrases() {
        let parsed = parse_search_query("foo \"node cve\" bar");
        assert_eq!(parsed.tokens.len(), 3);
        assert_eq!(parsed.tokens[0].kind, SearchTokenKind::Fuzzy);
        assert_eq!(parsed.tokens[1].kind, SearchTokenKind::Phrase);
        assert_eq!(parsed.tokens[1].value, "node cve");
        assert_eq!(parsed.tokens[2].value, "bar");
        assert!(match_search_text("a node cve report", &parsed).matches);
        assert!(!match_search_text("a node report", &parsed).matches);
    }

    #[test]
    fn unbalanced_quotes_fall_back_to_whitespace_tokenization() {
        let parsed = parse_search_query("foo \"bar baz");
        assert_eq!(parsed.tokens.len(), 3);
        assert!(parsed.tokens.iter().all(|token| token.kind == SearchTokenKind::Fuzzy));
        assert_eq!(parsed.tokens[1].value, "bar");
    }

    #[test]
    fn fuzzy_tokens_must_stay_under_the_strict_score() {
        let parsed = parse_search_query("zzzzzzzz");
        assert_eq!(parsed.tokens.len(), 1);
        assert!(!match_search_text("a completely different session", &parsed).matches);
    }

    #[test]
    fn corpus_join_skips_empty_parts() {
        assert_eq!(create_session_search_text(&[Some("a"), None, Some(""), Some("b")]), "a b");
        assert_eq!(create_session_search_text(&[]), "");
    }
}
