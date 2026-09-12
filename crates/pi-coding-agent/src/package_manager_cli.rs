//! Port of packages/coding-agent/src/package-manager-cli.ts
//!
//! `handleConfigCommand`, `handlePackageCommand` and the daemon update-restart
//! coordinator they can spawn.

use std::path::Path;
use std::sync::Arc;

use serde_json::Value;

use crate::cli::config_selector::{select_config, ConfigSelectorOptions};
use crate::cli::daemon_launch::{
    ensure_interactive_daemon_running, is_daemon_session_summary, is_session_busy, probe_running_daemon_sessions,
    shutdown_connected_daemon_and_wait, RunningDaemonProbe, StaleDaemonError,
};
use crate::cli::daemon_stop_confirm::{
    confirm_daemon_session_loss, pluralize_sessions, ConfirmIo, ConfirmOptions, DaemonSessionLossCopy,
};
use crate::cli::daemon_update_restart::{
    acquire_daemon_shutdown_admission, acquire_daemon_update_restart_coordinator, build_daemon_update_restart_report,
    launch_daemon_update_restart_coordinator, wait_for_active_daemon_update_restart_coordinator,
    DaemonUpdateRestartCoordinatorAlreadyRunningError, DaemonUpdateRestartCounts, DaemonUpdateRestartFailure,
    DaemonUpdateRestartProcessIdentity, DaemonUpdateRestartStatus, DaemonUpdateRestartStatusWriter,
    DaemonUpdateRestartUpdate, DAEMON_UPDATE_RESTART_COORDINATOR_FLAG, DAEMON_UPDATE_RESTART_ORIGIN_FLAG,
    DAEMON_UPDATE_RESTART_STATUS_FLAG,
};
use crate::config::{
    get_agent_dir, get_daemon_update_restart_manifest_path, get_legacy_daemon_update_restart_manifest_path,
    get_self_update_command, get_self_update_unavailable_instruction, APP_NAME, CONFIG_DIR_NAME, PACKAGE_NAME,
    SELF_UPDATE_INTERACTIVE_CHILD_ENV, SELF_UPDATE_NOT_ATTEMPTED_EXIT_CODE, VERSION,
};
use crate::core::messages::{is_session_slash_command, CustomMessage};
use crate::core::package_manager::{DefaultPackageManager, PackageManagerOptions, ProgressEvent};
use crate::core::settings_manager::{SettingsError, SettingsManager};
use crate::modes::daemon::daemon_client::protocol::{
    is_unknown_daemon_command_error, DaemonCommand, DaemonHello, DaemonResponse,
    DAEMON_UPDATE_RESTART_FORMAT_VERSION,
};
use crate::modes::daemon::daemon_client::{DaemonClient, DaemonClientRequestOptions};
use crate::modes::daemon::daemon_socket::{default_daemon_socket_path, normalize_socket_path};
use crate::modes::daemon::daemon_supervisor_ownership::{
    persist_daemon_startup_fence_from_owner, wait_for_daemon_startup_fence,
};
use crate::modes::daemon::daemon_worker_protocol::{
    DAEMON_WORKER_ACTIVE_SESSION_ID_ENV, DAEMON_WORKER_SUPERVISOR_SOCKET_ENV,
};
use crate::utils::child_process::{should_use_windows_shell, SpawnOptions};
use crate::utils::update_source::PRIME_AGENT_UPDATE_REPOSITORY_URL;
use crate::utils::version_check::{get_latest_pi_release, is_main_build_update_available};

pub type PackageCommand = &'static str;

pub const PACKAGE_COMMANDS: [PackageCommand; 4] = ["install", "remove", "update", "list"];

const UPDATE_RESTART_PREDECESSOR_FENCE_TIMEOUT_MS: u64 = 60_000;

/// `UpdateTarget`.
#[derive(Debug, Clone, PartialEq)]
pub enum UpdateTarget {
    All,
    Self_,
    Extensions { source: Option<String> },
}

/// `isSelfUpdateSource(source)`.
pub fn is_self_update_source(source: &str) -> bool {
    source == "self" || source == "pi" || source == APP_NAME
}

/// `PackageCommandOptions`.
#[derive(Debug, Clone, Default)]
pub struct PackageCommandOptions {
    pub command: String,
    pub source: Option<String>,
    pub update_target: Option<UpdateTarget>,
    pub local: bool,
    pub force: bool,
    pub help: bool,
    pub daemon_socket_path: Option<String>,
    pub restart_coordinator: bool,
    pub restart_status_path: Option<String>,
    pub restart_origin_active_session_id: Option<String>,
    pub invalid_option: Option<String>,
    pub invalid_argument: Option<String>,
    pub missing_option_value: Option<String>,
    pub conflicting_options: Option<String>,
}

fn yellow(message: &str) -> String {
    format!("\u{1b}[33m{message}\u{1b}[39m")
}

fn red(message: &str) -> String {
    format!("\u{1b}[31m{message}\u{1b}[39m")
}

fn green(message: &str) -> String {
    format!("\u{1b}[32m{message}\u{1b}[39m")
}

fn dim(message: &str) -> String {
    format!("\u{1b}[2m{message}\u{1b}[22m")
}

fn bold(message: &str) -> String {
    format!("\u{1b}[1m{message}\u{1b}[22m")
}

fn gray(message: &str) -> String {
    format!("\u{1b}[90m{message}\u{1b}[39m")
}

/// `reportSettingsErrors(settingsManager, context)`.
fn report_settings_errors(settings_manager: &mut SettingsManager, context: &str) {
    let errors: Vec<SettingsError> = settings_manager.drain_errors(None);
    for settings_error in errors {
        eprintln!(
            "{}",
            yellow(&format!(
                "Warning ({}, {} settings): {}",
                context, settings_error.scope, settings_error.error.message
            ))
        );
        if let Some(stack) = &settings_error.error.stack {
            eprintln!("{}", dim(stack));
        }
    }
}

fn get_package_command_usage(command: &str) -> String {
    match command {
        "install" => format!("{APP_NAME} package install <source> [--local]"),
        "remove" => format!("{APP_NAME} package remove <source> [--local]"),
        "update" => format!("{APP_NAME} update [--force] or {APP_NAME} package update [source]"),
        _ => format!("{APP_NAME} package list"),
    }
}

fn print_package_command_help(command: &str) {
    match command {
        "install" => println!(
            "{}
  {}

Install a package and add it to settings.

Options:
  --local    Install project-locally ({CONFIG_DIR_NAME}/settings.json)

Examples:
  {APP_NAME} package install npm:@foo/bar
  {APP_NAME} package install git:github.com/user/repo
  {APP_NAME} package install git:git@github.com:user/repo
  {APP_NAME} package install https://github.com/user/repo
  {APP_NAME} package install ssh://git@github.com/user/repo
  {APP_NAME} package install ./local/path
",
            bold("Usage:"),
            get_package_command_usage("install")
        ),
        "remove" => println!(
            "{}
  {}

Remove a package and its source from settings.

Options:
  --local    Remove from project settings ({CONFIG_DIR_NAME}/settings.json)

Examples:
  {APP_NAME} package remove npm:@foo/bar
",
            bold("Usage:"),
            get_package_command_usage("remove")
        ),
        "update" => println!(
            "{}
  {}

Update {APP_NAME} or installed packages.

Options:
  --self                  Update {APP_NAME} only
  --extensions            Update installed packages only
  --extension <source>    Update one package only
  --force                 Reinstall {APP_NAME} even if the current version is latest
  --daemon-socket <path>  Restart the daemon listening on this exact socket

Commands:
  {APP_NAME} update                Update {APP_NAME}
  {APP_NAME} package update        Update installed packages
  {APP_NAME} package update <source> Update one package
",
            bold("Usage:"),
            get_package_command_usage("update")
        ),
        _ => println!(
            "{}
  {}

List installed packages from user and project settings.
",
            bold("Usage:"),
            get_package_command_usage("list")
        ),
    }
}

/// `parsePackageCommand(args)`.
pub fn parse_package_command(args: &[String]) -> Option<PackageCommandOptions> {
    let raw_command = args.first().map(String::as_str)?;
    let rest = &args[1..];
    let command: &'static str = if raw_command == "uninstall" {
        "remove"
    } else {
        match raw_command {
            "install" => "install",
            "remove" => "remove",
            "update" => "update",
            "list" => "list",
            _ => return None,
        }
    };

    let mut local = false;
    let mut force = false;
    let mut help = false;
    let mut invalid_option: Option<String> = None;
    let mut invalid_argument: Option<String> = None;
    let mut missing_option_value: Option<String> = None;
    let mut conflicting_options: Option<String> = None;
    let mut source: Option<String> = None;
    let mut self_flag = false;
    let mut extensions_flag = false;
    let mut extension_flag_source: Option<String> = None;
    let mut daemon_socket_path: Option<String> = None;
    let mut restart_coordinator = false;
    let mut restart_status_path: Option<String> = None;
    let mut restart_origin_active_session_id: Option<String> = None;

    let mut index = 0usize;
    while index < rest.len() {
        let arg = rest[index].clone();
        if arg == "-h" || arg == "--help" {
            help = true;
            index += 1;
            continue;
        }

        if arg == "--local" {
            if command == "install" || command == "remove" {
                local = true;
            } else if invalid_option.is_none() {
                invalid_option = Some(arg);
            }
            index += 1;
            continue;
        }

        if arg == "--self" {
            if command == "update" {
                self_flag = true;
            } else if invalid_option.is_none() {
                invalid_option = Some(arg);
            }
            index += 1;
            continue;
        }

        if arg == "--extensions" {
            if command == "update" {
                extensions_flag = true;
            } else if invalid_option.is_none() {
                invalid_option = Some(arg);
            }
            index += 1;
            continue;
        }

        if arg == "--force" {
            if command == "update" {
                force = true;
            } else if invalid_option.is_none() {
                invalid_option = Some(arg);
            }
            index += 1;
            continue;
        }

        if arg == "--daemon-socket" {
            if command != "update" {
                if invalid_option.is_none() {
                    invalid_option = Some(arg);
                }
                index += 1;
                continue;
            }
            let value = rest.get(index + 1);
            match value {
                None => {
                    if missing_option_value.is_none() {
                        missing_option_value = Some(arg);
                    }
                    index += 1;
                }
                Some(value) if value.starts_with('-') => {
                    if missing_option_value.is_none() {
                        missing_option_value = Some(arg);
                    }
                    index += 1;
                }
                Some(value) if daemon_socket_path.is_some() => {
                    if conflicting_options.is_none() {
                        conflicting_options = Some("--daemon-socket can only be provided once".to_string());
                    }
                    index += 2;
                }
                Some(value) => {
                    daemon_socket_path = Some(normalize_socket_path(value, None));
                    index += 2;
                }
            }
            continue;
        }

        if arg == DAEMON_UPDATE_RESTART_COORDINATOR_FLAG {
            if command == "update" {
                restart_coordinator = true;
            } else if invalid_option.is_none() {
                invalid_option = Some(arg);
            }
            index += 1;
            continue;
        }

        if arg == DAEMON_UPDATE_RESTART_STATUS_FLAG || arg == DAEMON_UPDATE_RESTART_ORIGIN_FLAG {
            if command != "update" {
                if invalid_option.is_none() {
                    invalid_option = Some(arg);
                }
                index += 1;
                continue;
            }
            let value = rest.get(index + 1);
            match value {
                None => {
                    if missing_option_value.is_none() {
                        missing_option_value = Some(arg);
                    }
                    index += 1;
                }
                Some(value) if value.starts_with('-') => {
                    if missing_option_value.is_none() {
                        missing_option_value = Some(arg);
                    }
                    index += 1;
                }
                Some(value) => {
                    if arg == DAEMON_UPDATE_RESTART_STATUS_FLAG {
                        restart_status_path = Some(value.clone());
                    } else {
                        restart_origin_active_session_id = Some(value.clone());
                    }
                    index += 2;
                }
            }
            continue;
        }

        if arg == "--extension" {
            if command != "update" {
                if invalid_option.is_none() {
                    invalid_option = Some(arg);
                }
                index += 1;
                continue;
            }

            let value = rest.get(index + 1);
            match value {
                None => {
                    if missing_option_value.is_none() {
                        missing_option_value = Some(arg);
                    }
                    index += 1;
                }
                Some(value) if value.starts_with('-') => {
                    if missing_option_value.is_none() {
                        missing_option_value = Some(arg);
                    }
                    index += 1;
                }
                Some(value) if extension_flag_source.is_some() => {
                    if conflicting_options.is_none() {
                        conflicting_options = Some("--extension can only be provided once".to_string());
                    }
                    index += 2;
                }
                Some(value) => {
                    extension_flag_source = Some(value.clone());
                    index += 2;
                }
            }
            continue;
        }

        if arg.starts_with('-') {
            if invalid_option.is_none() {
                invalid_option = Some(arg);
            }
            index += 1;
            continue;
        }

        if source.is_none() {
            source = Some(arg);
        } else if invalid_argument.is_none() {
            invalid_argument = Some(arg);
        }
        index += 1;
    }

    let mut update_target: Option<UpdateTarget> = None;
    if command == "update" {
        if let Some(extension_flag_source) = extension_flag_source.clone() {
            if self_flag || extensions_flag {
                if conflicting_options.is_none() {
                    conflicting_options =
                        Some("--extension cannot be combined with --self or --extensions".to_string());
                }
            }
            if source.is_some() {
                if conflicting_options.is_none() {
                    conflicting_options =
                        Some("--extension cannot be combined with a positional source".to_string());
                }
            }
            update_target = Some(UpdateTarget::Extensions {
                source: Some(extension_flag_source),
            });
        } else if let Some(source) = source.clone() {
            if is_self_update_source(&source) {
                update_target = Some(if extensions_flag { UpdateTarget::All } else { UpdateTarget::Self_ });
            } else {
                if extensions_flag || self_flag {
                    if conflicting_options.is_none() {
                        conflicting_options = Some(
                            "positional update targets cannot be combined with --self or --extensions".to_string(),
                        );
                    }
                }
                update_target = Some(UpdateTarget::Extensions { source: Some(source) });
            }
        } else if self_flag && extensions_flag {
            update_target = Some(UpdateTarget::All);
        } else if self_flag {
            update_target = Some(UpdateTarget::Self_);
        } else if extensions_flag {
            update_target = Some(UpdateTarget::Extensions { source: None });
        } else {
            update_target = Some(UpdateTarget::All);
        }
    }

    Some(PackageCommandOptions {
        command: command.to_string(),
        source,
        update_target,
        local,
        force,
        help,
        daemon_socket_path,
        restart_coordinator,
        restart_status_path,
        restart_origin_active_session_id,
        invalid_option,
        invalid_argument,
        missing_option_value,
        conflicting_options,
    })
}

fn update_target_includes_self(target: &UpdateTarget) -> bool {
    matches!(target, UpdateTarget::All | UpdateTarget::Self_)
}

fn update_target_includes_extensions(target: &UpdateTarget) -> bool {
    matches!(target, UpdateTarget::All | UpdateTarget::Extensions { .. })
}

/// `resolveUpdateDaemonSocketPath(explicitSocketPath?)`.
pub fn resolve_update_daemon_socket_path(explicit_socket_path: Option<&str>) -> String {
    let socket_path = explicit_socket_path
        .map(str::to_string)
        .or_else(|| std::env::var(DAEMON_WORKER_SUPERVISOR_SOCKET_ENV).ok())
        .unwrap_or_else(default_daemon_socket_path);
    normalize_socket_path(&socket_path, None)
}

fn report_daemon_update_restart_status(status: &DaemonUpdateRestartStatus) {
    let report = build_daemon_update_restart_report(status);
    for message in report.info {
        println!("{}", green(&message));
    }
    for warning in report.warnings {
        eprintln!("{}", yellow(&format!("Warning: {warning}")));
    }
}

fn print_self_update_unavailable(npm_command: Option<&[String]>, update_spec: &str, update_package_name: &str) {
    eprintln!("error: {APP_NAME} cannot self-update this installation.");
    eprintln!(
        "{}",
        get_self_update_unavailable_instruction(
            PACKAGE_NAME,
            npm_command,
            update_spec,
            update_package_name
        )
    );

    if let Some(entrypoint) = std::env::args().nth(1) {
        eprintln!();
        eprintln!("Location of pi executable: {entrypoint}");
    }
}

fn print_self_update_fallback(command: &crate::config::SelfUpdateCommand) {
    eprintln!(
        "{}",
        dim(&format!(
            "If this keeps failing, run this command yourself: {}",
            command.display
        ))
    );
}

/// `SelfUpdatePlan`.
#[derive(Debug, Clone, PartialEq)]
pub struct SelfUpdatePlan {
    pub install_spec: String,
    pub package_name: String,
    pub should_run: bool,
    pub target_version: Option<String>,
}

fn set_self_update_no_change_exit_code() {
    if std::env::var(SELF_UPDATE_INTERACTIVE_CHILD_ENV).as_deref() == Ok("1") {
        std::env::set_var("PI_EXIT_CODE", SELF_UPDATE_NOT_ATTEMPTED_EXIT_CODE.to_string());
    }
}

/// `getSelfUpdatePlan(force)`.
pub async fn get_self_update_plan(force: bool) -> Result<SelfUpdatePlan, String> {
    let latest_release = get_latest_pi_release(VERSION).await.ok_or_else(|| {
        format!(
            "No installable main build is available from {PRIME_AGENT_UPDATE_REPOSITORY_URL}. Check the connection and the main release build, then retry."
        )
    })?;
    let should_run = force || is_main_build_update_available(&latest_release.version, VERSION);
    if !should_run {
        println!("{}", green(&format!("{APP_NAME} is already up to date (v{VERSION})")));
    }
    Ok(SelfUpdatePlan {
        install_spec: latest_release.install_spec,
        package_name: latest_release.package_name,
        should_run,
        target_version: Some(latest_release.version),
    })
}
