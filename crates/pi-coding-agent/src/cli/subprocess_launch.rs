//! Port of packages/coding-agent/src/cli/subprocess-launch.ts
//!
//! The TypeScript reads three runtime globals: `process.execPath`,
//! `process.argv[1]` and `process.execArgv`. The port's CLI has no script
//! argument, so the entrypoint resolves to the running binary itself. That keeps
//! `createCliSubprocessLaunchSpec` infallible here, while the TypeScript's
//! "Cannot determine current CLI entrypoint for subprocess launch" error stays
//! reachable only through the explicit `entrypoint: None` argument.
//!
//! TODO(slice): `crate::config::is_bun_binary` (ca-root slice) has not landed;
//! the port has no Bun runtime, so `is_bun_binary()` returns false.

use std::path::{Path, PathBuf};

pub const TSX_TSCONFIG_PATH_ENV: &str = "TSX_TSCONFIG_PATH";

/// Local stand-in for `isBunBinary` from ../config.js.
fn is_bun_binary() -> bool {
    false
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliSubprocessLaunchSpec {
    pub command: String,
    pub args: Vec<String>,
}

/// Process environment map. The TypeScript spreads `NodeJS.ProcessEnv`; the port
/// keeps an explicit map so an absent key stays distinguishable from an empty one.
pub type ProcessEnv = indexmap::IndexMap<String, String>;

pub fn create_cli_subprocess_env(
    source: &ProcessEnv,
    entrypoint: Option<&str>,
    exec_args: &[String],
) -> ProcessEnv {
    let mut environment = source.clone();
    if environment.contains_key(TSX_TSCONFIG_PATH_ENV)
        || entrypoint.is_none()
        || !exec_args.iter().any(|arg| arg.contains("tsx"))
    {
        return environment;
    }
    let entrypoint = entrypoint.unwrap();
    let mut directory = resolve(entrypoint)
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default();
    loop {
        let tsconfig_path = directory.join("tsconfig.json");
        if tsconfig_path.exists()
            && directory
                .join("node_modules")
                .join("tsx")
                .join("package.json")
                .exists()
        {
            environment.insert(
                TSX_TSCONFIG_PATH_ENV.to_string(),
                tsconfig_path.to_string_lossy().to_string(),
            );
            return environment;
        }
        let parent = match directory.parent() {
            Some(parent) => parent.to_path_buf(),
            None => return environment,
        };
        if parent == directory {
            return environment;
        }
        directory = parent;
    }
}

/// Node's `path.isAbsolute` on the current platform.
///
/// This must NOT be `std::path::Path::is_absolute`: on win32 Node reports
/// `isAbsolute('/abs') === true` (and true for `'\\abs'`), while Rust treats a
/// drive-less rooted path as *relative* and `Path::join` then prefixes the
/// current drive (`current_dir().join("/cli.ts")` == `"C:/cli.ts"`), silently
/// rewriting an argv string the TypeScript passes through verbatim.
fn is_node_absolute(path: &str) -> bool {
    if cfg!(windows) {
        let bytes = path.as_bytes();
        let drive_absolute = bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && (bytes[2] == b'\\' || bytes[2] == b'/');
        return drive_absolute || matches!(bytes.first(), Some(b'\\') | Some(b'/'));
    }
    path.starts_with('/')
}

/// Node's `path.resolve` for a single path.
fn resolve(path: &str) -> PathBuf {
    let candidate = PathBuf::from(path);
    if candidate.is_absolute() {
        candidate
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(candidate)
    }
}

fn quote_command_argument(value: &str) -> String {
    let safe = !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_./:@%+=,-".contains(c));
    if safe {
        value.to_string()
    } else {
        format!("'{}'", value.replace('\'', "'\"'\"'"))
    }
}

pub fn format_current_cli_command(args: &[String], environment: &ProcessEnv) -> String {
    if let Some(launcher_path) = environment.get("PRIME_AGENT_LAUNCHER_PATH") {
        let mut parts = vec![launcher_path.clone()];
        parts.extend(args.iter().cloned());
        return parts
            .iter()
            .map(|part| quote_command_argument(part))
            .collect::<Vec<_>>()
            .join(" ");
    }
    let launch = create_cli_subprocess_launch_spec(args, None, &[], None);
    let mut parts = vec![launch.command];
    parts.extend(launch.args);
    parts
        .iter()
        .map(|part| quote_command_argument(part))
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn create_cli_subprocess_launch_spec(
    args: &[String],
    executable: Option<&str>,
    exec_args: &[String],
    entrypoint: Option<&str>,
) -> CliSubprocessLaunchSpec {
    let executable = executable
        .map(str::to_string)
        .unwrap_or_else(|| current_exec_path());
    if is_bun_binary() || (exec_args.is_empty() && entrypoint.is_none_or(|path| path == executable))
    {
        return CliSubprocessLaunchSpec {
            command: executable,
            args: args.to_vec(),
        };
    }
    let entrypoint = match entrypoint {
        Some(entrypoint) if !entrypoint.is_empty() => entrypoint.to_string(),
        _ => current_entrypoint(),
    };
    // subprocess-launch.ts:59
    //   `const resolvedEntrypoint = isAbsolute(entrypoint) ? entrypoint : resolve(entrypoint);`
    // The entrypoint is an argv string, not a filesystem operation, so an
    // absolute entrypoint is passed through byte-for-byte.
    let resolved_entrypoint = if is_node_absolute(&entrypoint) {
        entrypoint
    } else {
        resolve(&entrypoint).to_string_lossy().to_string()
    };
    let mut launch_args = exec_args.to_vec();
    launch_args.push(resolved_entrypoint);
    launch_args.extend(args.iter().cloned());
    CliSubprocessLaunchSpec {
        command: executable,
        args: launch_args,
    }
}

/// `process.execPath`: the running binary in the port.
pub(crate) fn current_exec_path() -> String {
    std::env::current_exe()
        .map(|path| path.to_string_lossy().to_string())
        .unwrap_or_default()
}

/// `process.argv[1]`: the port's entrypoint is its own binary.
pub(crate) fn current_entrypoint() -> String {
    current_exec_path()
}

/// `process.execArgv`: Rust has no runtime prelude arguments.
pub(crate) fn current_exec_args() -> Vec<String> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> ProcessEnv {
        pairs
            .iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect()
    }

    #[test]
    fn quotes_only_unsafe_arguments() {
        assert_eq!(quote_command_argument("plain/path.txt"), "plain/path.txt");
        assert_eq!(quote_command_argument("a b"), "'a b'");
        assert_eq!(quote_command_argument("it's"), "'it'\"'\"'s'");
        assert_eq!(quote_command_argument(""), "''");
    }

    #[test]
    fn launcher_path_wins_over_the_exec_path() {
        let environment = env(&[("PRIME_AGENT_LAUNCHER_PATH", "/usr/local/bin/prime-agent")]);
        assert_eq!(
            format_current_cli_command(
                &["shutdown".to_string(), "--force".to_string()],
                &environment
            ),
            "/usr/local/bin/prime-agent shutdown --force"
        );
    }

    #[test]
    fn falls_back_to_the_exec_path_and_entrypoint() {
        let command = format_current_cli_command(&["shutdown".to_string()], &env(&[]));
        assert!(command.ends_with(" shutdown"));
        let launch = create_cli_subprocess_launch_spec(
            &["--mode".to_string(), "daemon".to_string()],
            Some("/usr/bin/node"),
            &[],
            Some("/opt/prime-agent/cli.js"),
        );
        assert_eq!(launch.command, "/usr/bin/node");
        assert_eq!(
            launch.args,
            vec!["/opt/prime-agent/cli.js", "--mode", "daemon"]
        );
    }

    #[test]
    fn native_subprocess_does_not_pass_its_executable_as_a_prompt() {
        let args = vec!["--mode".to_string(), "daemon".to_string()];
        let launch = create_cli_subprocess_launch_spec(&args, None, &[], None);
        assert_eq!(launch.command, current_exec_path());
        assert_eq!(launch.args, args);
        let launch = create_cli_subprocess_launch_spec(
            &args,
            Some("/tmp/optimus-rust"),
            &[],
            Some("/tmp/optimus-rust"),
        );
        assert_eq!(launch.args, args);
    }

    #[test]
    fn exec_args_are_prepended_and_relative_entrypoints_are_resolved() {
        let launch = create_cli_subprocess_launch_spec(
            &["x".to_string()],
            Some("node"),
            &["--experimental".to_string()],
            Some("cli.js"),
        );
        assert!(launch.args[0] == "--experimental");
        assert!(launch.args[1].ends_with("cli.js"));
        assert!(Path::new(&launch.args[1]).is_absolute());
        assert_eq!(launch.args[2], "x");
    }

    #[test]
    fn subprocess_env_is_untouched_without_tsx() {
        let source = env(&[("A", "1")]);
        let result = create_cli_subprocess_env(&source, Some("/tmp/cli.js"), &[]);
        assert_eq!(result, source);
    }

    #[test]
    fn subprocess_env_is_untouched_when_already_set() {
        let source = env(&[(TSX_TSCONFIG_PATH_ENV, "/tmp/tsconfig.json")]);
        let result = create_cli_subprocess_env(&source, Some("/tmp/cli.js"), &["tsx".to_string()]);
        assert_eq!(result, source);
    }

    #[test]
    fn missing_tsconfig_leaves_the_env_untouched() {
        let source = env(&[("A", "1")]);
        let result = create_cli_subprocess_env(
            &source,
            Some("/definitely/not/here/cli.js"),
            &["tsx".to_string()],
        );
        assert_eq!(result, source);
    }
}
