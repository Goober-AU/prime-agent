//! Port of packages/coding-agent/src/utils/clipboard-native.ts

use std::future::Future;
use std::pin::Pin;
use std::sync::OnceLock;

use super::pi_user_agent::process_platform;

pub type ClipboardFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;

/// The `@mariozechner/clipboard` addon surface used by clipboard.ts.
pub trait ClipboardModule: Send + Sync {
    fn set_text(&self, text: String) -> ClipboardFuture<Result<(), String>>;
    fn has_image(&self) -> bool;
    fn get_image_binary(&self) -> ClipboardFuture<Result<Vec<u8>, String>>;
}

static CLIPBOARD: OnceLock<Option<Box<dyn ClipboardModule>>> = OnceLock::new();

fn has_display() -> bool {
    if process_platform() != "linux" {
        return true;
    }
    let has = |name: &str| std::env::var(name).map(|value| !value.is_empty()).unwrap_or(false);
    has("DISPLAY") || has("WAYLAND_DISPLAY")
}

/// The addon is a native Node module; without a Rust binding the module stays
/// unavailable and every caller falls back to the platform tools, exactly like
/// the TypeScript catch branch that sets `clipboard = null`.
fn load_native_module() -> Option<Box<dyn ClipboardModule>> {
    if std::env::var("TERMUX_VERSION").is_ok() || !has_display() {
        return None;
    }
    None
}

pub fn clipboard() -> Option<&'static dyn ClipboardModule> {
    CLIPBOARD
        .get_or_init(load_native_module)
        .as_deref()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn has_display_matches_the_platform_rule() {
        if process_platform() == "linux" {
            let saved_display = std::env::var("DISPLAY").ok();
            let saved_wayland = std::env::var("WAYLAND_DISPLAY").ok();
            std::env::remove_var("DISPLAY");
            std::env::remove_var("WAYLAND_DISPLAY");
            assert!(!has_display());
            std::env::set_var("WAYLAND_DISPLAY", "wayland-0");
            assert!(has_display());
            std::env::remove_var("WAYLAND_DISPLAY");
            if let Some(value) = saved_display {
                std::env::set_var("DISPLAY", value);
            }
            if let Some(value) = saved_wayland {
                std::env::set_var("WAYLAND_DISPLAY", value);
            }
        } else {
            assert!(has_display());
        }
    }

    #[test]
    fn the_native_addon_is_unavailable_without_a_rust_binding() {
        std::env::set_var("TERMUX_VERSION", "1");
        assert!(clipboard().is_none());
        std::env::remove_var("TERMUX_VERSION");
    }
}
