//! Port of packages/coding-agent/src/modes/interactive/theme/code-highlighter.ts
//!
//! Thin wrapper around cli-highlight so the heavy highlight.js dependency
//! (~350ms import) can be loaded lazily via dynamic import. The Rust port has no
//! cli-highlight equivalent, so the module only exposes the load state that
//! `theme.ts` (`preloadCodeHighlighter`, `highlightCode`) observes.

/// `supportsLanguage(lang)` - the highlighter is never available in the Rust port.
pub fn supports_language(_lang: &str) -> bool {
    false
}

/// `highlight(code, options)` - returns the input unchanged until a highlighter exists.
pub fn highlight(code: &str) -> String {
    code.to_string()
}

/// True once the (never-completing) highlighter import resolved.
pub fn is_loaded() -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn highlighter_is_never_available() {
        assert!(!is_loaded());
        assert!(!supports_language("typescript"));
        assert_eq!(highlight("let x = 1;"), "let x = 1;");
    }
}
