//! Port of packages/coding-agent/src/modes/shared/startup-notices.ts
//!
//! Global, environment-scoped startup notices (app update, extension updates,
//! tmux setup). These are not tied to any single conversation, so they are
//! surfaced on the agents view rather than appended to a session's chat stream.

use std::sync::Arc;

use crate::modes::interactive::theme::theme::theme;
use crate::utils::child_process::{spawn_hidden, SpawnOptions};
use crate::utils::version_check::check_for_new_pi_version;

/// `StartupNotices`.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartupNotices {
    /// Newer Prime Agent version available, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_version: Option<String>,
    /// Display names of extensions with available updates.
    pub package_updates: Vec<String>,
    /// tmux keyboard setup warning, if the current tmux config is suboptimal.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tmux_warning: Option<String>,
}

/// `StartupNoticeCheckOptions`.
#[derive(Clone)]
pub struct StartupNoticeCheckOptions {
    pub version: String,
    pub cwd: String,
    pub agent_dir: String,
    /// `SettingsManager` (core/settings-manager.ts); only forwarded to the
    /// package manager, which this slice does not own.
    pub settings_manager: Arc<dyn std::any::Any + Send + Sync>,
}

/// The `DefaultPackageManager` surface `checkForPackageUpdates` uses.
///
/// blocked_on: `core/package-manager.ts` belongs to another slice.
pub trait StartupNoticePackageManager: Send + Sync {
    /// `packageManager.checkForAvailableUpdates()`.
    fn check_for_available_updates(
        &self,
    ) -> pi_ai::types::BoxFuture<Result<Vec<StartupNoticePackageUpdate>, String>>;
}

/// `{ displayName: string }` from an available update.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartupNoticePackageUpdate {
    pub display_name: String,
}

/// `PI_OFFLINE` gate read once, like `process.env.PI_OFFLINE`.
pub fn is_pi_offline() -> bool {
    std::env::var("PI_OFFLINE").map(|value| !value.is_empty()).unwrap_or(false)
}

/// Run every startup check in parallel and collect the results.
pub async fn gather_startup_notices(
    options: StartupNoticeCheckOptions,
    package_manager: Option<Arc<dyn StartupNoticePackageManager>>,
) -> StartupNotices {
    let version_future = check_for_new_pi_version(&options.version);
    let updates_future = check_for_package_updates(
        &options.cwd,
        &options.agent_dir,
        package_manager,
    );
    let tmux_future = check_tmux_keyboard_setup();
    let (new_version, package_updates, tmux_warning) = tokio::join!(version_future, updates_future, tmux_future);
    StartupNotices {
        new_version,
        package_updates,
        tmux_warning,
    }
}

/// `checkForPackageUpdates(options)`.
pub async fn check_for_package_updates(
    cwd: &str,
    agent_dir: &str,
    package_manager: Option<Arc<dyn StartupNoticePackageManager>>,
) -> Vec<String> {
    if is_pi_offline() {
        return Vec::new();
    }
    let _ = (cwd, agent_dir);
    let Some(package_manager) = package_manager else {
        // blocked_on: core/package-manager.ts (DefaultPackageManager) belongs to
        // another slice; without it the check reports no updates.
        return Vec::new();
    };
    match package_manager.check_for_available_updates().await {
        Ok(updates) => updates.into_iter().map(|update| update.display_name).collect(),
        Err(_) => Vec::new(),
    }
}

/// `checkTmuxKeyboardSetup()`.
pub async fn check_tmux_keyboard_setup() -> Option<String> {
    if std::env::var("TMUX").map(|value| value.is_empty()).unwrap_or(true) {
        return None;
    }

    let extended_keys = run_tmux_show("extended-keys").await;
    let extended_keys_format = run_tmux_show("extended-keys-format").await;

    // If tmux could not be queried (timeout, sandbox, etc.), do not warn.
    let extended_keys = extended_keys?;

    if extended_keys != "on" && extended_keys != "always" {
        return Some(
            "tmux extended-keys is off. Modified Enter keys may not work. Add `set -g extended-keys on` to ~/.tmux.conf and restart tmux."
                .to_string(),
        );
    }

    if extended_keys_format.as_deref() == Some("xterm") {
        return Some(
            "tmux extended-keys-format is xterm. Pi works best with csi-u. Add `set -g extended-keys-format csi-u` to ~/.tmux.conf and restart tmux."
                .to_string(),
        );
    }

    None
}

/// `runTmuxShow(option)`.
async fn run_tmux_show(option: &str) -> Option<String> {
    let mut handle = spawn_hidden(
        "tmux",
        &["show".to_string(), "-gv".to_string(), option.to_string()],
        SpawnOptions {
            capture_stdout: true,
            ..Default::default()
        },
    )
    .ok()?;
    let stdout = handle.child.stdout.take();
    let mut output = String::new();
    let read = async {
        if let Some(mut stdout) = stdout {
            use tokio::io::AsyncReadExt;
            let mut buffer = Vec::new();
            let _ = stdout.read_to_end(&mut buffer).await;
            output = String::from_utf8_lossy(&buffer).to_string();
        }
    };
    let status = tokio::time::timeout(std::time::Duration::from_millis(2000), async {
        tokio::join!(read, handle.child.wait())
    })
    .await;
    match status {
        Ok((_, Ok(exit))) => {
            if exit.code() == Some(0) {
                Some(output.trim().to_string())
            } else {
                None
            }
        }
        // Timeout or spawn error: `proc.kill(); resolve(undefined)`.
        _ => {
            let _ = handle.child.start_kill();
            None
        }
    }
}

/// `formatUpdateAvailableNotice(newVersion)`.
pub fn format_update_available_notice(new_version: &str) -> String {
    let theme = theme();
    format!(
        "{} {}{}",
        theme.bold(&theme.fg("accent", "Update available:")),
        theme.fg("muted", &format!("v{new_version}. Run ")),
        theme.fg("accent", "/update")
    )
}

/// `formatPackageUpdateNotice(packages)`.
pub fn format_package_update_notice(packages: &[String]) -> String {
    let theme = theme();
    let package_list = packages.join(", ");
    format!(
        "{} {}{}",
        theme.bold(&theme.fg("warning", "Package updates available:")),
        theme.fg("muted", &format!("{package_list}. Run ")),
        theme.fg("accent", "/update --extensions")
    )
}

/// `formatTmuxWarningNotice(message)`.
pub fn format_tmux_warning_notice(message: &str) -> String {
    theme().fg("warning", &format!("\u{26a0} {message}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offline_env_suppresses_package_updates() {
        std::env::set_var("PI_OFFLINE", "1");
        let updates = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(check_for_package_updates(".", ".", None));
        assert!(updates.is_empty());
        std::env::remove_var("PI_OFFLINE");
    }

    #[test]
    fn notices_default_to_no_warnings() {
        let notices = StartupNotices::default();
        assert_eq!(notices.package_updates, Vec::<String>::new());
        assert!(notices.new_version.is_none());
        assert!(notices.tmux_warning.is_none());
    }

    #[test]
    fn tmux_check_is_skipped_without_tmux_env() {
        std::env::remove_var("TMUX");
        let warning = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(check_tmux_keyboard_setup());
        assert!(warning.is_none());
    }

    #[test]
    fn notices_serialize_with_camel_case_keys() {
        let notices = StartupNotices {
            new_version: Some("1.2.3".to_string()),
            package_updates: vec!["alpha".to_string()],
            tmux_warning: None,
        };
        assert_eq!(
            serde_json::to_value(&notices).unwrap(),
            serde_json::json!({"newVersion": "1.2.3", "packageUpdates": ["alpha"]})
        );
    }
}
