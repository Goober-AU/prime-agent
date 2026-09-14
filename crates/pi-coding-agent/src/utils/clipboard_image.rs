//! Port of packages/coding-agent/src/utils/clipboard-image.ts

use std::collections::HashMap;
use std::path::PathBuf;

use super::child_process::{spawn_sync_hidden, SpawnOptions};
use super::clipboard_native::clipboard;
use super::photon::load_photon;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipboardImage {
    pub bytes: Vec<u8>,
    pub mime_type: String,
}

pub const SUPPORTED_IMAGE_MIME_TYPES: [&str; 4] = ["image/png", "image/jpeg", "image/webp", "image/gif"];

const DEFAULT_LIST_TIMEOUT_MS: u64 = 1000;
const DEFAULT_READ_TIMEOUT_MS: u64 = 3000;
const DEFAULT_POWERSHELL_TIMEOUT_MS: u64 = 5000;
const DEFAULT_MAX_BUFFER_BYTES: usize = 50 * 1024 * 1024;

fn env_value(env: &HashMap<String, String>, name: &str) -> Option<String> {
    env.get(name).cloned()
}

fn process_env() -> HashMap<String, String> {
    std::env::vars().collect()
}

pub fn is_wayland_session_with(env: &HashMap<String, String>) -> bool {
    env_value(env, "WAYLAND_DISPLAY").is_some()
        || env_value(env, "XDG_SESSION_TYPE").as_deref() == Some("wayland")
}

pub fn is_wayland_session() -> bool {
    is_wayland_session_with(&process_env())
}

fn base_mime_type(mime_type: &str) -> String {
    mime_type
        .split(';')
        .next()
        .map(|part| part.trim().to_lowercase())
        .unwrap_or_else(|| mime_type.to_lowercase())
}

pub fn select_preferred_image_mime_type(mime_types: &[String]) -> Option<String> {
    let normalized: Vec<(String, String)> = mime_types
        .iter()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .map(|raw| {
            let base = base_mime_type(&raw);
            (raw, base)
        })
        .collect();

    for preferred in SUPPORTED_IMAGE_MIME_TYPES {
        if let Some((raw, _)) = normalized.iter().find(|(_, base)| base == preferred) {
            return Some(raw.clone());
        }
    }

    normalized
        .iter()
        .find(|(_, base)| base.starts_with("image/"))
        .map(|(raw, _)| raw.clone())
}

pub fn is_supported_image_mime_type(mime_type: &str) -> bool {
    let base = base_mime_type(mime_type);
    SUPPORTED_IMAGE_MIME_TYPES.iter().any(|candidate| *candidate == base)
}

/// Convert unsupported image formats to PNG using Photon.
/// Returns None if conversion is unavailable or fails.
pub async fn convert_to_png(bytes: &[u8]) -> Option<Vec<u8>> {
    let photon = load_photon().await?;
    let image = photon.new_from_byteslice(bytes)?;
    photon.encode_png(&image)
}

pub struct RunCommandOptions {
    pub timeout_ms: Option<u64>,
    pub max_buffer_bytes: Option<usize>,
    pub env: Option<HashMap<String, String>>,
}

pub struct RunCommandResult {
    pub stdout: Vec<u8>,
    pub ok: bool,
}

pub fn run_command(command: &str, args: &[String], options: Option<RunCommandOptions>) -> RunCommandResult {
    let options = options.unwrap_or(RunCommandOptions {
        timeout_ms: None,
        max_buffer_bytes: None,
        env: None,
    });
    let timeout_ms = options.timeout_ms.unwrap_or(DEFAULT_READ_TIMEOUT_MS);
    let max_buffer_bytes = options.max_buffer_bytes.unwrap_or(DEFAULT_MAX_BUFFER_BYTES);

    let spawn_options = SpawnOptions {
        env: options
            .env
            .map(|env| env.into_iter().collect::<Vec<(String, String)>>()),
        capture_stdout: true,
        ..Default::default()
    };

    let output = spawn_sync_hidden_with_timeout(command, args, spawn_options, timeout_ms);
    let output = match output {
        Some(output) => output,
        None => {
            return RunCommandResult {
                ok: false,
                stdout: Vec::new(),
            }
        }
    };

    if !output.status.success() || output.stdout.len() > max_buffer_bytes {
        return RunCommandResult {
            ok: false,
            stdout: Vec::new(),
        };
    }

    RunCommandResult {
        ok: true,
        stdout: output.stdout,
    }
}

fn spawn_sync_hidden_with_timeout(
    command: &str,
    args: &[String],
    options: SpawnOptions,
    timeout_ms: u64,
) -> Option<std::process::Output> {
    // spawnSync's `timeout` kills the child; `maxBuffer` is enforced by run_command.
    let handle = std::thread::spawn({
        let command = command.to_string();
        let args = args.to_vec();
        move || spawn_sync_hidden(&command, &args, options)
    });
    // Poll for completion so the timeout can abandon a hung child.
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
    while !handle.is_finished() {
        if std::time::Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    handle.join().ok()?.ok()
}

fn split_lines(value: &str) -> Vec<String> {
    value
        .split('\n')
        .map(|line| line.trim_end_matches('\r').trim().to_string())
        .filter(|line| !line.is_empty())
        .collect()
}

fn read_clipboard_image_via_wl_paste() -> Option<ClipboardImage> {
    let list = run_command(
        "wl-paste",
        &["--list-types".to_string()],
        Some(RunCommandOptions {
            timeout_ms: Some(DEFAULT_LIST_TIMEOUT_MS),
            max_buffer_bytes: None,
            env: None,
        }),
    );
    if !list.ok {
        return None;
    }

    let types = split_lines(&String::from_utf8_lossy(&list.stdout));

    let selected_type = select_preferred_image_mime_type(&types)?;

    let data = run_command(
        "wl-paste",
        &[
            "--type".to_string(),
            selected_type.clone(),
            "--no-newline".to_string(),
        ],
        None,
    );
    if !data.ok || data.stdout.is_empty() {
        return None;
    }

    Some(ClipboardImage {
        bytes: data.stdout,
        mime_type: base_mime_type(&selected_type),
    })
}

fn is_wsl_with(env: &HashMap<String, String>) -> bool {
    if env_value(env, "WSL_DISTRO_NAME").is_some() || env_value(env, "WSLENV").is_some() {
        return true;
    }

    match std::fs::read_to_string("/proc/version") {
        Ok(release) => {
            let lowered = release.to_lowercase();
            lowered.contains("microsoft") || lowered.contains("wsl")
        }
        Err(_) => false,
    }
}

fn is_wsl() -> bool {
    is_wsl_with(&process_env())
}

/// On WSL, the Linux clipboard (Wayland/X11) does not receive image data from
/// Windows screenshots (Win+Shift+S). PowerShell can access the Windows clipboard
/// directly, so we use it as a fallback.
fn read_clipboard_image_via_power_shell() -> Option<ClipboardImage> {
    let tmp_file = std::env::temp_dir().join(format!("pi-wsl-clip-{}.png", uuid::Uuid::new_v4()));
    let tmp_file_string = tmp_file.to_string_lossy().to_string();

    let result = (|| -> Option<ClipboardImage> {
        let win_path_result = run_command(
            "wslpath",
            &["-w".to_string(), tmp_file_string.clone()],
            Some(RunCommandOptions {
                timeout_ms: Some(DEFAULT_LIST_TIMEOUT_MS),
                max_buffer_bytes: None,
                env: None,
            }),
        );
        if !win_path_result.ok {
            return None;
        }

        let win_path = String::from_utf8_lossy(&win_path_result.stdout).trim().to_string();
        if win_path.is_empty() {
            return None;
        }

        let ps_quoted_win_path = win_path.replace('\'', "''");
        let ps_script = [
            "Add-Type -AssemblyName System.Windows.Forms",
            "Add-Type -AssemblyName System.Drawing",
            &format!("$path = '{}'", ps_quoted_win_path),
            "$img = [System.Windows.Forms.Clipboard]::GetImage()",
            "if ($img) { $img.Save($path, [System.Drawing.Imaging.ImageFormat]::Png); Write-Output 'ok' } else { Write-Output 'empty' }",
        ]
        .join("; ");

        let result = run_command(
            "powershell.exe",
            &[
                "-NoProfile".to_string(),
                "-Command".to_string(),
                ps_script,
            ],
            Some(RunCommandOptions {
                timeout_ms: Some(DEFAULT_POWERSHELL_TIMEOUT_MS),
                max_buffer_bytes: None,
                env: None,
            }),
        );
        if !result.ok {
            return None;
        }

        let output = String::from_utf8_lossy(&result.stdout).trim().to_string();
        if output != "ok" {
            return None;
        }

        let bytes = std::fs::read(&tmp_file).ok()?;
        if bytes.is_empty() {
            return None;
        }

        Some(ClipboardImage {
            bytes,
            mime_type: "image/png".to_string(),
        })
    })();

    // Ignore cleanup errors.
    let _ = std::fs::remove_file(&tmp_file);
    let _ = PathBuf::from(&tmp_file_string);
    result
}

fn read_clipboard_image_via_xclip() -> Option<ClipboardImage> {
    let targets = run_command(
        "xclip",
        &[
            "-selection".to_string(),
            "clipboard".to_string(),
            "-t".to_string(),
            "TARGETS".to_string(),
            "-o".to_string(),
        ],
        Some(RunCommandOptions {
            timeout_ms: Some(DEFAULT_LIST_TIMEOUT_MS),
            max_buffer_bytes: None,
            env: None,
        }),
    );

    let mut candidate_types: Vec<String> = Vec::new();
    if targets.ok {
        candidate_types = split_lines(&String::from_utf8_lossy(&targets.stdout));
    }

    let preferred = if candidate_types.is_empty() {
        None
    } else {
        select_preferred_image_mime_type(&candidate_types)
    };
    let try_types: Vec<String> = match preferred {
        Some(preferred) => {
            let mut types = vec![preferred];
            types.extend(SUPPORTED_IMAGE_MIME_TYPES.iter().map(|value| value.to_string()));
            types
        }
        None => SUPPORTED_IMAGE_MIME_TYPES
            .iter()
            .map(|value| value.to_string())
            .collect(),
    };

    for mime_type in try_types {
        let data = run_command(
            "xclip",
            &[
                "-selection".to_string(),
                "clipboard".to_string(),
                "-t".to_string(),
                mime_type.clone(),
                "-o".to_string(),
            ],
            None,
        );
        if data.ok && !data.stdout.is_empty() {
            return Some(ClipboardImage {
                bytes: data.stdout,
                mime_type: base_mime_type(&mime_type),
            });
        }
    }

    None
}

async fn read_clipboard_image_via_native_clipboard() -> Option<ClipboardImage> {
    let module = clipboard()?;
    if !module.has_image() {
        return None;
    }

    let image_data = module.get_image_binary().await.ok()?;
    if image_data.is_empty() {
        return None;
    }

    Some(ClipboardImage {
        bytes: image_data,
        mime_type: "image/png".to_string(),
    })
}

pub struct ReadClipboardImageOptions {
    pub env: Option<HashMap<String, String>>,
    pub platform: Option<String>,
}

pub async fn read_clipboard_image(options: Option<ReadClipboardImageOptions>) -> Option<ClipboardImage> {
    let options = options.unwrap_or(ReadClipboardImageOptions {
        env: None,
        platform: None,
    });
    let env = options.env.unwrap_or_else(process_env);
    let platform = options
        .platform
        .unwrap_or_else(|| super::pi_user_agent::process_platform().to_string());

    if env_value(&env, "TERMUX_VERSION").is_some() {
        return None;
    }

    let mut image: Option<ClipboardImage> = None;

    if platform == "linux" {
        let wsl = is_wsl_with(&env);
        let wayland = is_wayland_session_with(&env);

        if wayland || wsl {
            image = read_clipboard_image_via_wl_paste().or_else(read_clipboard_image_via_xclip);
        }

        if image.is_none() && wsl {
            image = read_clipboard_image_via_power_shell();
        }

        if image.is_none() && !wayland {
            image = read_clipboard_image_via_native_clipboard().await;
        }
    } else {
        image = read_clipboard_image_via_native_clipboard().await;
    }

    let image = image?;

    // Convert unsupported formats (e.g., BMP from WSLg) to PNG
    if !is_supported_image_mime_type(&image.mime_type) {
        let png_bytes = convert_to_png(&image.bytes).await?;
        return Some(ClipboardImage {
            bytes: png_bytes,
            mime_type: "image/png".to_string(),
        });
    }

    Some(image)
}

/// Exposed for callers that need the WSL probe with a custom environment.
pub fn is_wsl_environment(env: &HashMap<String, String>) -> bool {
    is_wsl_with(env)
}

/// Keeps the default timeout constants observable for tests and callers.
pub fn default_timeouts() -> (u64, u64, u64, usize) {
    (
        DEFAULT_LIST_TIMEOUT_MS,
        DEFAULT_READ_TIMEOUT_MS,
        DEFAULT_POWERSHELL_TIMEOUT_MS,
        DEFAULT_MAX_BUFFER_BYTES,
    )
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
    fn wayland_detection_matches_the_typescript() {
        assert!(is_wayland_session_with(&env(&[("WAYLAND_DISPLAY", "wayland-0")])));
        assert!(is_wayland_session_with(&env(&[("XDG_SESSION_TYPE", "wayland")])));
        assert!(!is_wayland_session_with(&env(&[("XDG_SESSION_TYPE", "x11")])));
        assert!(!is_wayland_session_with(&env(&[])));
    }

    #[test]
    fn base_mime_type_strips_parameters() {
        assert_eq!(base_mime_type("image/png; charset=utf-8"), "image/png");
        assert_eq!(base_mime_type("  IMAGE/PNG  "), "image/png");
    }

    #[test]
    fn selects_the_preferred_supported_type() {
        let types = vec!["text/plain".to_string(), "image/png".to_string(), "image/jpeg".to_string()];
        assert_eq!(select_preferred_image_mime_type(&types), Some("image/png".to_string()));

        let only_jpeg = vec!["image/jpeg".to_string()];
        assert_eq!(select_preferred_image_mime_type(&only_jpeg), Some("image/jpeg".to_string()));

        let unknown_image = vec!["image/bmp".to_string()];
        assert_eq!(select_preferred_image_mime_type(&unknown_image), Some("image/bmp".to_string()));

        let none = vec!["text/plain".to_string()];
        assert_eq!(select_preferred_image_mime_type(&none), None);
    }

    #[test]
    fn supported_mime_type_check_uses_the_base_type() {
        assert!(is_supported_image_mime_type("image/png"));
        assert!(is_supported_image_mime_type("image/gif; charset=binary"));
        assert!(!is_supported_image_mime_type("image/bmp"));
    }

    #[test]
    fn run_command_reports_failures_for_missing_binaries() {
        let result = run_command(
            "definitely-not-a-real-binary-xyz",
            &[],
            Some(RunCommandOptions {
                timeout_ms: Some(DEFAULT_LIST_TIMEOUT_MS),
                max_buffer_bytes: None,
                env: None,
            }),
        );
        assert!(!result.ok);
        assert!(result.stdout.is_empty());
    }

    #[test]
    fn run_command_reports_failures_for_non_zero_exit() {
        let (command, args) = if cfg!(windows) {
            ("cmd", vec!["/c".to_string(), "exit 3".to_string()])
        } else {
            ("sh", vec!["-c".to_string(), "exit 3".to_string()])
        };
        let result = run_command(command, &args, None);
        assert!(!result.ok);
    }

    #[test]
    fn termux_returns_no_image() {
        let mut environment = env(&[("TERMUX_VERSION", "0.118")]);
        environment.insert("WAYLAND_DISPLAY".to_string(), "wayland-0".to_string());
        let result = tokio_test_block_on(read_clipboard_image(Some(ReadClipboardImageOptions {
            env: Some(environment),
            platform: Some("linux".to_string()),
        })));
        assert!(result.is_none());
    }

    #[test]
    fn wsl_probe_reads_the_environment_first() {
        assert!(is_wsl_with(&env(&[("WSL_DISTRO_NAME", "Ubuntu")])));
        assert!(is_wsl_with(&env(&[("WSLENV", "x")])));
    }

    #[test]
    fn default_timeouts_match_the_typescript() {
        assert_eq!(default_timeouts(), (1000, 3000, 5000, 50 * 1024 * 1024));
    }

    /// Minimal blocking executor so the sync tests can await the async entrypoint.
    fn tokio_test_block_on<F: std::future::Future>(future: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(future)
    }
}
