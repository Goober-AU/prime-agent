//! Port of packages/ai/src/utils/sanitize-unicode.ts

/// Removes unpaired Unicode surrogate characters from UTF-16 code units.
///
/// Unpaired surrogates (high surrogates 0xD800-0xDBFF without matching low
/// surrogates 0xDC00-0xDFFF, or vice versa) cause JSON serialization errors in
/// many API providers. Valid pairs are preserved.
pub fn sanitize_surrogate_units(units: &[u16]) -> String {
    let mut out: Vec<u16> = Vec::with_capacity(units.len());
    let mut i = 0usize;
    while i < units.len() {
        let unit = units[i];
        let is_high = (0xD800..=0xDBFF).contains(&unit);
        let is_low = (0xDC00..=0xDFFF).contains(&unit);
        if is_high {
            let next_is_low = units.get(i + 1).map(|n| (0xDC00..=0xDFFF).contains(n)).unwrap_or(false);
            if next_is_low {
                out.push(unit);
                out.push(units[i + 1]);
                i += 2;
                continue;
            }
            i += 1;
            continue;
        }
        if is_low {
            // A low surrogate that was not consumed as part of a pair is unpaired.
            i += 1;
            continue;
        }
        out.push(unit);
        i += 1;
    }
    String::from_utf16_lossy(&out)
}

/// `sanitizeSurrogates(text: string): string`.
///
/// Rust `String` values are valid UTF-8 and therefore cannot hold unpaired
/// surrogates, so this is the identity for every possible input. The function is
/// kept (and is not inlined away) so provider code keeps the same call boundary
/// as the TypeScript; raw UTF-16 input goes through `sanitize_surrogate_units`.
pub fn sanitize_surrogates(text: &str) -> String {
    let units: Vec<u16> = text.encode_utf16().collect();
    sanitize_surrogate_units(&units)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_valid_emoji() {
        assert_eq!(sanitize_surrogates("Hello \u{1F648} World"), "Hello \u{1F648} World");
    }

    #[test]
    fn removes_unpaired_high_surrogate() {
        let units: Vec<u16> = "Text ".encode_utf16().chain([0xD83D]).chain(" here".encode_utf16()).collect();
        assert_eq!(sanitize_surrogate_units(&units), "Text  here");
    }

    #[test]
    fn removes_unpaired_low_surrogate() {
        let units: Vec<u16> = [0xDE00].into_iter().chain("tail".encode_utf16()).collect();
        assert_eq!(sanitize_surrogate_units(&units), "tail");
    }

    #[test]
    fn keeps_paired_surrogates() {
        let units: Vec<u16> = vec![0xD83D, 0xDE48];
        assert_eq!(sanitize_surrogate_units(&units), "\u{1F648}");
    }
}
