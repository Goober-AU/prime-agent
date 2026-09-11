//! Port of packages/coding-agent/src/utils/clipboard.ts

use std::collections::HashMap;

use base64::Engine as _;

use super::child_process::{exec_sync_hidden_with_input, spawn_hidden, SpawnOptions};
use super::clipboard_image::is_wayland_session;
use super::clipboard_native::clipboard;
use super::pi_user_agent::process_platform;

pub struct NativeClipboardExecOptions {
    pub input: String,
    pub timeout: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum ClipboardError {
    #[error("Failed to copy to clipboard")]
    Failed,
}

fn copy_to_x11_clipboard(options: &NativeClipboardExecOptions) -> bool {
    let spawn_options = SpawnOptions {
        capture_stdout: false,
        capture_stderr: false,
        stdin_piped: true,
        ..Default::default()
    };
    if exec_sync_hidden_with_input("xclip -selection clipboard", &options.input, spawn_options.clone(), options.timeout)
        .is_ok()
    {
        return true;
    }
    exec_sync_hidden_with_input("xsel --clipboard --input", &options.input, spawn_options, options.timeout).is_ok()
}

const MAX_OSC52_ENCODED_LENGTH: usize = 100_000;

pub fn is_remote_session_with(env: &HashMap<String, String>) -> bool {
    env.contains_key("SSH_CONNECTION") || env.contains_key("SSH_CLIENT") || env.contains_key("MOSH_CONNECTION")
}

fn is_remote_session() -> bool {
    is_remote_session_with(&std::env::vars().collect())
}

pub fn emit_osc52(text: &str) -> bool {
    let encoded = base64::engine::general_purpose::STANDARD.encode(text.as_bytes());
    if encoded.len() > MAX_OSC52_ENCODED_LENGTH {
        return false;
    }
    use std::io::Write;
    let mut stdout = std::io::stdout();
    let _ = write!(stdout, "\x1b]52;c;{}\x07", encoded);
    let _ = stdout.flush();
    true
}

pub async fn copy_to_clipboard(text: &str) -> Result<(), ClipboardError> {
    let mut copied = false;

    let p = process_platform();

    // Prefer direct clipboard writes. Emitting OSC 52 first can make terminals
    // write the same native clipboard concurrently with the addon, and very large
    // OSC 52 payloads can desynchronize terminal rendering.
    //
    // On Linux, skip the native addon. The underlying `clipboard-rs` crate is
    // X11-only and does not retain selection ownership after `set_text`
    // resolves, so on Wayland-only compositors (Hyprland, Niri, ...) and even
    // some X11 sessions the call resolves successfully without populating the
    // clipboard. The platform tools below (wl-copy, xclip, xsel) properly
    // daemonize and keep ownership.
    if p != "linux" {
        if let Some(module) = clipboard() {
            if module.set_text(text.to_string()).await.is_ok() {
                copied = true;
            }
        }
    }

    let remote = is_remote_session();
    if copied && !remote {
        return Ok(());
    }

    let options = NativeClipboardExecOptions {
        input: text.to_string(),
        timeout: 5000,
    };

    if !copied {
        let spawn_options = SpawnOptions {
            capture_stdout: false,
            capture_stderr: false,
            stdin_piped: true,
            ..Default::default()
        };

        if p == "darwin" {
            if exec_sync_hidden_with_input("pbcopy", &options.input, spawn_options, options.timeout).is_ok() {
                copied = true;
            }
        } else if p == "win32" {
            if exec_sync_hidden_with_input("clip", &options.input, spawn_options, options.timeout).is_ok() {
                copied = true;
            }
        } else if std::env::var("TERMUX_VERSION").is_ok() {
            // Linux. Try Termux, Wayland, or X11 clipboard tools.
            if exec_sync_hidden_with_input(
                "termux-clipboard-set",
                &options.input,
                spawn_options.clone(),
                options.timeout,
            )
            .is_ok()
            {
                copied = true;
            }

            if !copied {
                copied = try_wayland_or_x11_clipboard(&options, &spawn_options);
            }
        } else {
            copied = try_wayland_or_x11_clipboard(&options, &spawn_options);
        }
    }

    if remote || !copied {
        let osc52_copied = emit_osc52(text);
        copied = copied || osc52_copied;
    }

    if !copied {
        return Err(ClipboardError::Failed);
    }

    Ok(())
}

fn try_wayland_or_x11_clipboard(options: &NativeClipboardExecOptions, spawn_options: &SpawnOptions) -> bool {
    let has_wayland_display = std::env::var("WAYLAND_DISPLAY").map(|v| !v.is_empty()).unwrap_or(false);
    let has_x11_display = std::env::var("DISPLAY").map(|v| !v.is_empty()).unwrap_or(false);
    let is_wayland = is_wayland_session();
    if is_wayland && has_wayland_display {
        // Verify wl-copy exists (spawn errors are async and won't be caught)
        let which_options = SpawnOptions {
            capture_stdout: false,
            capture_stderr: false,
            ..Default::default()
        };
        if std::process::Command::new("which")
            .arg("wl-copy")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
        {
            // wl-copy with execSync hangs due to fork behavior; use spawn instead
            let wl_options = SpawnOptions {
                capture_stdout: false,
                capture_stderr: false,
                stdin_piped: true,
                ..Default::default()
            };
            if let Ok(mut handle) = spawn_hidden("wl-copy", &[], wl_options) {
                if let Some(mut stdin) = handle.child.stdin.take() {
                    use tokio::io::AsyncWriteExt;
                    let text = options.input.clone();
                    tokio::spawn(async move {
                        // Ignore EPIPE errors if wl-copy exits early
                        let _ = stdin.write_all(text.as_bytes()).await;
                        let _ = stdin.shutdown().await;
                    });
                }
                return true;
            }
            if has_x11_display {
                return copy_to_x11_clipboard(options);
            }
            return false;
        }
        if has_x11_display {
            return copy_to_x11_clipboard(options);
        }
        return false;
    }
    if has_x11_display {
        return copy_to_x11_clipboard(options);
    }
    let _ = (spawn_options, options);
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect()
    }

    #[test]
    fn remote_session_detection_matches_the_typescript() {
        assert!(is_remote_session_with(&env(&[("SSH_CONNECTION", "1.2.3.4")])));
        assert!(is_remote_session_with(&env(&[("SSH_CLIENT", "1.2.3.4")])));
        assert!(is_remote_session_with(&env(&[("MOSH_CONNECTION", "1.2.3.4")])));
        assert!(!is_remote_session_with(&env(&[])));
    }

    #[test]
    fn osc52_rejects_payloads_over_the_limit() {
        let short = "hello";
        assert!(emit_osc52(short));

        let long = "a".repeat(MAX_OSC52_ENCODED_LENGTH);
        assert!(!emit_osc52(&long));
    }

    #[test]
    fn osc52_encodes_as_standard_base64() {
        let encoded = base64::engine::general_purpose::STANDARD.encode("hi");
        assert_eq!(encoded, "aGk=");
    }
}
