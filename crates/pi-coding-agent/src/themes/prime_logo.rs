//! Port of packages/coding-agent/src/themes/prime-logo.ts
//!
//! Pre-rendered ASCII versions of the Prime butterfly mark.
//!
//! Source: assets/brand/prime-butterfly.svg
//! Re-render at any width: `uv run scripts/render-logo.py --width N`

/// ~10 rows x 32 cols. The default brand mark - half-block butterfly, splash-ready.
pub const PRIME_BUTTERFLY_LOGO: &str = "                          \u{2584}\u{2584}\u{2588}\u{2588}\u{2588}\u{2580}
    \u{2584}\u{2584}\u{2584}\u{2584}\u{2584}              \u{2584}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2580}
    \u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2584}         \u{2584}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2580}
   \u{2584}\u{2588}\u{2588}\u{2588}\u{2580}\u{2588}\u{2588}\u{2588}\u{2584}     \u{2584}\u{2588}\u{2588}\u{2588}\u{2580}\u{2584}\u{2588}\u{2588}\u{2580}
   \u{2588}\u{2588}\u{2588} \u{2584}\u{2588}\u{2588}\u{2588}\u{2588}\u{2584}\u{2584}\u{2584}\u{2588}\u{2588}\u{2588}\u{2588}\u{2580}\u{2584}\u{2584}\u{2588}\u{2588}
  \u{2580}\u{2588}\u{2588}  \u{2580}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2580}\u{2580}\u{2580}\u{2580}\u{2580}\u{2580}
  \u{2584}\u{2588}\u{2588}   \u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2580}\u{2580} \u{2584}\u{2588}\u{2588}\u{2588}
 \u{2588}\u{2588}\u{2588}\u{2588}\u{2588}    \u{2580}\u{2588}\u{2584}\u{2584}\u{2584}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2580}
\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2584}  \u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2580}
\u{2580}\u{2588}\u{2588}\u{2588}\u{2580}\u{2580}    \u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2580}";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logo_has_ten_rows_and_keeps_the_half_block_art() {
        let rows: Vec<&str> = PRIME_BUTTERFLY_LOGO.split('\n').collect();
        assert_eq!(rows.len(), 10);
        assert!(rows[0].starts_with("                          \u{2584}"));
        assert!(PRIME_BUTTERFLY_LOGO.contains('\u{2588}'));
    }
}
