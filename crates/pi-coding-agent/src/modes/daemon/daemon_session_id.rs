//! Port of packages/coding-agent/src/modes/daemon/daemon-session-id.ts
//!
//! Re-export shim: the daemon surfaces use the shared session-id helpers from
//! `core/session-id.ts`. That module belongs to another slice; the local
//! implementation here keeps the same behaviour until it lands.

pub const DISPLAY_ID_LENGTH: usize = 12;

pub fn normalize_session_id(id: &str) -> String {
    id.replace('-', "").to_lowercase()
}

fn normalize_hex_session_id(id: &str) -> Option<String> {
    let normalized = normalize_session_id(id);
    if !normalized.is_empty() && normalized.chars().all(|c| c.is_ascii_hexdigit()) {
        Some(normalized)
    } else {
        None
    }
}

pub fn format_session_display_id(id: &str) -> String {
    match normalize_hex_session_id(id) {
        Some(normalized) => tail(&normalized, DISPLAY_ID_LENGTH),
        None => tail(id, DISPLAY_ID_LENGTH),
    }
}

fn tail(value: &str, length: usize) -> String {
    let chars: Vec<char> = value.chars().collect();
    if chars.len() > length {
        chars[chars.len() - length..].iter().collect()
    } else {
        value.to_string()
    }
}

pub fn matches_session_id_suffix(candidate: &str, suffix: &str) -> bool {
    match (normalize_hex_session_id(candidate), normalize_hex_session_id(suffix)) {
        (Some(candidate), Some(suffix)) => candidate.ends_with(&suffix),
        _ => false,
    }
}

pub fn matches_saved_session_selector(candidate: &str, selector: &str) -> bool {
    match (normalize_hex_session_id(candidate), normalize_hex_session_id(selector)) {
        (Some(candidate), Some(selector)) => candidate.starts_with(&selector) || candidate.ends_with(&selector),
        _ => candidate.starts_with(selector),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_id_is_the_trailing_twelve_characters() {
        assert_eq!(format_session_display_id("0011223344556677"), "223344556677");
        assert_eq!(format_session_display_id("abc"), "abc");
        assert_eq!(
            format_session_display_id("00112233-4455-6677"),
            "223344556677"
        );
    }

    #[test]
    fn suffix_matching_requires_hex_identifiers() {
        assert!(matches_session_id_suffix("00112233", "2233"));
        assert!(!matches_session_id_suffix("hello", "lo"));
    }
}
