//! Port of packages/coding-agent/src/core/session-id.ts

const DISPLAY_ID_LENGTH: usize = 12;

/// `normalizeSessionId(id)`: `id.replaceAll("-", "").toLowerCase()`.
pub fn normalize_session_id(id: &str) -> String {
    id.replace('-', "").to_lowercase()
}

fn is_hex_id(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn normalize_hex_session_id(id: &str) -> Option<String> {
    let normalized = normalize_session_id(id);
    if is_hex_id(&normalized) {
        Some(normalized)
    } else {
        None
    }
}

/// JS `String.prototype.slice(-n)`; returns the whole string when shorter than `n`.
fn tail(value: &str, n: usize) -> String {
    let chars: Vec<char> = value.chars().collect();
    if chars.len() <= n {
        return value.to_string();
    }
    chars[chars.len() - n..].iter().collect()
}

pub fn format_session_display_id(id: &str) -> String {
    match normalize_hex_session_id(id) {
        Some(normalized) => {
            if normalized.chars().count() > DISPLAY_ID_LENGTH {
                tail(&normalized, DISPLAY_ID_LENGTH)
            } else {
                normalized
            }
        }
        None => {
            if id.chars().count() > DISPLAY_ID_LENGTH {
                tail(id, DISPLAY_ID_LENGTH)
            } else {
                id.to_string()
            }
        }
    }
}

pub fn matches_session_id_suffix(candidate: &str, suffix: &str) -> bool {
    let normalized_candidate = normalize_hex_session_id(candidate);
    let normalized_suffix = normalize_hex_session_id(suffix);
    match (normalized_candidate, normalized_suffix) {
        (Some(candidate), Some(suffix)) => candidate.ends_with(&suffix),
        _ => false,
    }
}

pub fn matches_saved_session_selector(candidate: &str, selector: &str) -> bool {
    let normalized_candidate = normalize_hex_session_id(candidate);
    let normalized_selector = normalize_hex_session_id(selector);
    if let (Some(candidate), Some(selector)) = (normalized_candidate, normalized_selector) {
        return candidate.starts_with(&selector) || candidate.ends_with(&selector);
    }
    candidate.starts_with(selector)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_display_ids_from_hex_ids() {
        assert_eq!(format_session_display_id("01924f7a-1234-7abc-8def-0123456789ab"), "0123456789ab");
        assert_eq!(format_session_display_id("abc"), "abc");
        assert_eq!(format_session_display_id("not-a-hex-id-1234567890"), "d-1234567890");
    }

    #[test]
    fn matches_suffixes_and_selectors() {
        assert!(matches_session_id_suffix("01924f7a-1234-7abc-8def-0123456789ab", "0123456789ab"));
        assert!(!matches_session_id_suffix("01924f7a-1234-7abc-8def-0123456789ab", "zz"));
        assert!(matches_saved_session_selector("01924f7a-1234-7abc-8def-0123456789ab", "01924f7a"));
        assert!(matches_saved_session_selector("01924f7a-1234-7abc-8def-0123456789ab", "0123456789ab"));
        assert!(matches_saved_session_selector("plain-name", "plai"));
    }
}
