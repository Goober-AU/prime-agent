//! Port of packages/coding-agent/src/modes/interactive/components/keybinding-hints.ts
//!
//! Utilities for formatting keybinding hints in the UI.

use pi_tui::keybindings::get_keybindings;

use crate::modes::interactive::theme::theme::theme;

/// Port of `KeyTextOptions`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct KeyTextOptions {
    /// `primaryOnly?: boolean`
    pub primary_only: bool,
}

fn normalize_key_part(part: &str) -> &str {
    if part == "escape" {
        "esc"
    } else {
        part
    }
}

fn format_arrow_key(part: &str) -> Option<&'static str> {
    match part {
        "up" => Some("\u{2191}"),
        "down" => Some("\u{2193}"),
        "left" => Some("\u{2190}"),
        "right" => Some("\u{2192}"),
        _ => None,
    }
}

/// `process.platform` in the Node spelling used by `formatKeyPart`.
fn current_platform() -> &'static str {
    match std::env::consts::OS {
        "windows" => "win32",
        "macos" => "darwin",
        _ => "linux",
    }
}

fn format_key_part(part: &str, platform: &str) -> String {
    let normalized = normalize_key_part(part);
    if let Some(arrow) = format_arrow_key(normalized) {
        return arrow.to_string();
    }
    // Terminals send the literal Control key on macOS, so never label it Cmd.
    if platform == "darwin" && normalized == "alt" {
        return "Option".to_string();
    }
    let mut chars = normalized.chars();
    match chars.next() {
        Some(first) => {
            let mut result = first.to_uppercase().collect::<String>();
            result.push_str(chars.as_str());
            result
        }
        None => String::new(),
    }
}

/// Port of `formatKeyText`. `platform` is the TypeScript default parameter
/// (`= process.platform`); `None` means "use the current platform".
pub fn format_key_text(key: &str, platform: Option<&str>) -> String {
    let current = current_platform();
    let platform: &str = platform.unwrap_or(&current);
    key.split('/')
        .map(|binding| {
            binding
                .split('+')
                .map(|part| format_key_part(part, platform))
                .collect::<Vec<String>>()
                .join("+")
        })
        .collect::<Vec<String>>()
        .join("/")
}

fn format_keys(keys: &[String], options: &KeyTextOptions) -> String {
    let display_keys: Vec<String> = if options.primary_only {
        keys.iter().take(1).cloned().collect()
    } else {
        keys.to_vec()
    };
    if display_keys.is_empty() {
        return String::new();
    }
    if display_keys.len() == 1 {
        return format_key_text(&display_keys[0], None);
    }
    format_key_text(&display_keys.join("/"), None)
}

/// Port of `keyText`.
pub fn key_text(keybinding: &str, options: &KeyTextOptions) -> String {
    format_keys(&get_keybindings().get_keys(keybinding), options)
}

/// Port of `keyHint`.
pub fn key_hint(keybinding: &str, description: &str, options: &KeyTextOptions) -> String {
    theme().fg("dim", &key_text(keybinding, options))
        + &theme().fg("muted", &format!(" {description}"))
}

/// Canonical bracketed expand/collapse hint, e.g. `(Ctrl+O to expand)`, fully dim.
pub fn expand_collapse_hint(keybinding: &str, expanded: bool) -> String {
    theme().fg(
        "dim",
        &format!(
            "({} {})",
            key_text(keybinding, &KeyTextOptions::default()),
            if expanded { "to collapse" } else { "to expand" }
        ),
    )
}

/// Port of `rawKeyHint`.
pub fn raw_key_hint(key: &str, description: &str) -> String {
    theme().fg("dim", &format_key_text(key, None))
        + &theme().fg("muted", &format!(" {description}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_escape_and_arrow_parts() {
        assert_eq!(format_key_text("escape", Some("linux")), "esc");
        assert_eq!(format_key_text("up", Some("linux")), "\u{2191}");
        assert_eq!(format_key_text("ctrl+down", Some("linux")), "Ctrl+\u{2193}");
    }

    #[test]
    fn labels_alt_as_option_only_on_darwin() {
        assert_eq!(format_key_text("alt", Some("darwin")), "Option");
        assert_eq!(format_key_text("alt", Some("linux")), "Alt");
    }

    #[test]
    fn joins_alternative_bindings_with_slashes() {
        assert_eq!(
            format_key_text("ctrl+c/escape", Some("linux")),
            "Ctrl+C/Esc"
        );
    }

    #[test]
    fn primary_only_keeps_the_first_binding() {
        let keys = vec!["ctrl+a".to_string(), "ctrl+b".to_string()];
        assert_eq!(
            format_keys(&keys, &KeyTextOptions { primary_only: true }),
            "Ctrl+A"
        );
        assert_eq!(
            format_keys(&keys, &KeyTextOptions::default()),
            "Ctrl+A/Ctrl+B"
        );
        assert_eq!(format_keys(&[], &KeyTextOptions::default()), "");
    }

    #[test]
    fn key_text_reads_the_registered_keybindings() {
        assert!(!key_text("tui.select.cancel", &KeyTextOptions { primary_only: true }).is_empty());
        assert!(
            key_text("tui.select.cancel", &KeyTextOptions { primary_only: true })
                .chars()
                .any(|c| c.is_ascii_alphabetic())
        );
    }
}
