//! Port of packages/coding-agent/src/utils/child-process.ts

use std::io::Read;
use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

pub const EXIT_STDIO_GRACE_MS: u64 = 100;

/// `windowsHide` for every non-interactive spawn.
#[derive(Debug, Clone, Default)]
pub struct SpawnOptions {
    pub cwd: Option<String>,
    pub env: Option<Vec<(String, String)>>,
    pub detached: bool,
    /// Run through the platform shell (`cmd /c`) as Node's `shell: true` does.
    pub shell: bool,
    pub capture_stdout: bool,
    pub capture_stderr: bool,
    /// `stdio[0] = "pipe"`: the caller writes the child's stdin.
    pub stdin_piped: bool,
}

pub struct ChildProcessHandle {
    pub child: tokio::process::Child,
}

pub fn spawn_hidden(command: &str, args: &[String], options: SpawnOptions) -> std::io::Result<ChildProcessHandle> {
    let mut builder = build_tokio_command(command, args, &options);
    builder.kill_on_drop(false);
    Ok(ChildProcessHandle {
        child: builder.spawn()?,
    })
}

pub fn spawn_sync_hidden(
    command: &str,
    args: &[String],
    options: SpawnOptions,
) -> std::io::Result<std::process::Output> {
    let mut builder = build_std_command(command, args, &options);
    builder.output()
}

pub fn exec_sync_hidden(command: &str, options: SpawnOptions) -> std::io::Result<std::process::Output> {
    // Node runs the string through the shell; cmd.exe /c and /bin/sh -c are the equivalents.
    let mut builder = if cfg!(windows) {
        let mut builder = std::process::Command::new("cmd");
        builder.arg("/c").arg(command);
        builder
    } else {
        let mut builder = std::process::Command::new("/bin/sh");
        builder.arg("-c").arg(command);
        builder
    };
    apply_std_options(&mut builder, &options);
    builder.output()
}

/// `execSync(command, { input, timeout, stdio: ["pipe", "ignore", "ignore"] })`.
pub fn exec_sync_hidden_with_input(
    command: &str,
    input: &str,
    options: SpawnOptions,
    timeout_ms: u64,
) -> std::io::Result<std::process::ExitStatus> {
    use std::io::Write;
    let mut builder = if cfg!(windows) {
        let mut builder = std::process::Command::new("cmd");
        builder.arg("/c").arg(command);
        builder
    } else {
        let mut builder = std::process::Command::new("/bin/sh");
        builder.arg("-c").arg(command);
        builder
    };
    let mut options = options;
    options.stdin_piped = true;
    apply_std_options(&mut builder, &options);
    let mut child = builder.spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(input.as_bytes());
        let _ = stdin.flush();
        drop(stdin);
    }
    match wait_with_timeout(&mut child, timeout_ms) {
        Some(status) => Ok(status),
        None => Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            format!("command timed out after {}ms: {}", timeout_ms, command),
        )),
    }
}

pub fn exec_file_hidden(
    file: &str,
    args: &[String],
    options: SpawnOptions,
) -> std::io::Result<std::process::Output> {
    let mut builder = build_std_command(file, args, &options);
    builder.output()
}

pub fn exec_file_sync_hidden(
    file: &str,
    args: &[String],
    options: SpawnOptions,
) -> std::io::Result<std::process::Output> {
    let mut builder = build_std_command(file, args, &options);
    builder.output()
}

fn apply_std_options(builder: &mut std::process::Command, options: &SpawnOptions) {
    if let Some(cwd) = &options.cwd {
        builder.current_dir(cwd);
    }
    if let Some(env) = &options.env {
        builder.envs(env.iter().map(|(key, value)| (key.clone(), value.clone())));
    }
    builder.stdin(if options.stdin_piped {
        Stdio::piped()
    } else {
        Stdio::null()
    });
    builder.stdout(if options.capture_stdout {
        Stdio::piped()
    } else {
        Stdio::null()
    });
    builder.stderr(if options.capture_stderr {
        Stdio::piped()
    } else {
        Stdio::null()
    });
    hide_console_window_std(builder);
}

fn build_std_command(command: &str, args: &[String], options: &SpawnOptions) -> std::process::Command {
    let mut builder = if options.shell && cfg!(windows) {
        let mut builder = std::process::Command::new("cmd");
        builder.arg("/c").arg(command);
        builder.args(args);
        builder
    } else {
        let mut builder = std::process::Command::new(command);
        builder.args(args);
        builder
    };
    apply_std_options(&mut builder, options);
    builder
}

fn build_tokio_command(command: &str, args: &[String], options: &SpawnOptions) -> tokio::process::Command {
    let mut builder = if options.shell && cfg!(windows) {
        let mut builder = tokio::process::Command::new("cmd");
        builder.arg("/c").arg(command);
        builder.args(args);
        builder
    } else {
        let mut builder = tokio::process::Command::new(command);
        builder.args(args);
        builder
    };
    if let Some(cwd) = &options.cwd {
        builder.current_dir(cwd);
    }
    if let Some(env) = &options.env {
        builder.envs(env.iter().map(|(key, value)| (key.clone(), value.clone())));
    }
    builder.stdin(if options.stdin_piped {
        Stdio::piped()
    } else {
        Stdio::null()
    });
    builder.stdout(if options.capture_stdout {
        Stdio::piped()
    } else {
        Stdio::null()
    });
    builder.stderr(if options.capture_stderr {
        Stdio::piped()
    } else {
        Stdio::null()
    });
    if options.detached {
        builder.process_group(0);
    }
    hide_console_window_tokio(&mut builder);
    builder
}

#[cfg(windows)]
fn hide_console_window_std(builder: &mut std::process::Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    builder.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn hide_console_window_std(_builder: &mut std::process::Command) {}

#[cfg(windows)]
fn hide_console_window_tokio(builder: &mut tokio::process::Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    builder.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn hide_console_window_tokio(_builder: &mut tokio::process::Command) {}

const WINDOWS_SHELL_COMMANDS: [&str; 6] = ["npm", "npx", "pnpm", "yarn", "yarnpkg", "corepack"];

pub fn should_use_windows_shell(command: &str) -> bool {
    if !cfg!(windows) {
        return false;
    }
    let command_name = Path::new(command)
        .file_name()
        .map(|name| name.to_string_lossy().to_lowercase())
        .unwrap_or_else(|| command.to_lowercase());
    command_name.ends_with(".cmd")
        || command_name.ends_with(".bat")
        || WINDOWS_SHELL_COMMANDS.contains(&command_name.as_str())
}

/// Cheap kill(0) existence probe; counts zombies as existing.
pub fn process_id_exists(pid: i32) -> bool {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
        unsafe {
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid as u32);
            if handle.is_null() {
                // ERROR_ACCESS_DENIED means the process exists but is not ours.
                return std::io::Error::last_os_error().raw_os_error() == Some(5);
            }
            CloseHandle(handle);
            true
        }
    }
    #[cfg(not(windows))]
    {
        unsafe {
            if libc::kill(pid, 0) == 0 {
                return true;
            }
            std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
        }
    }
}

/// A zombie has already exited; it only lingers until its parent reaps it.
pub fn is_zombie_process(pid: i32) -> bool {
    if cfg!(windows) {
        return false;
    }

    if let Ok(stat) = std::fs::read_to_string(format!("/proc/{}/stat", pid)) {
        if let Some(index) = stat.rfind(')') {
            let state = stat[index + 1..].trim_start().chars().next();
            return state == Some('Z');
        }
    }

    // Fall through to the portable process listing used on macOS and BSD.
    match exec_file_sync_hidden(
        "ps",
        &["-p".to_string(), pid.to_string(), "-o".to_string(), "stat=".to_string()],
        SpawnOptions {
            capture_stdout: true,
            ..Default::default()
        },
    ) {
        Ok(output) => String::from_utf8_lossy(&output.stdout).trim().starts_with('Z'),
        Err(_) => false,
    }
}

/// True only for a process that is actually running: zombies do not count.
pub fn is_process_alive(pid: i32) -> bool {
    process_id_exists(pid) && !is_zombie_process(pid)
}

/// True while the group has any member left, zombies included; a group can outlive its leader.
pub fn process_group_exists(pgid: i32) -> bool {
    if cfg!(windows) {
        return false;
    }
    #[cfg(not(windows))]
    unsafe {
        if libc::kill(-pgid, 0) == 0 {
            return true;
        }
        std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
    #[cfg(windows)]
    false
}

/// True while the group has a RUNNING member; unreaped zombies have exited and must not block a group stop.
pub fn process_group_has_live_member(pgid: i32) -> bool {
    if !process_group_exists(pgid) {
        return false;
    }

    let listing = exec_file_sync_hidden(
        "ps",
        &[
            "-A".to_string(),
            "-o".to_string(),
            "pgid=".to_string(),
            "-o".to_string(),
            "stat=".to_string(),
        ],
        SpawnOptions {
            capture_stdout: true,
            ..Default::default()
        },
    );
    match listing {
        Ok(output) => {
            for line in String::from_utf8_lossy(&output.stdout).split('\n') {
                let fields: Vec<&str> = line.split_whitespace().collect();
                if fields.len() < 2 {
                    continue;
                }
                if fields[0].parse::<i32>() == Ok(pgid) && !fields[1].starts_with('Z') {
                    return true;
                }
            }
            false
        }
        // Unverifiable listing reads alive: callers keep escalating instead of dropping records over live descendants.
        Err(_) => true,
    }
}

/// Signal the group only while it is provably still the target.
pub fn signal_process_group_if_held(pgid: i32, signal: Signal) -> bool {
    if !process_id_exists(pgid) && !process_group_has_live_member(pgid) {
        return false;
    }
    signal_process_group_or_process(pgid, signal);
    true
}

pub fn signal_process_group_or_process(pid: i32, signal: Signal) {
    if send_signal(-pid, signal) {
        return;
    }
    // Fall back when process groups are unavailable or the group already exited.
    let _ = send_signal(pid, signal);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    Hup,
    Int,
    Term,
    Kill,
    Usr1,
    Usr2,
}

impl Signal {
    pub fn name(self) -> &'static str {
        match self {
            Signal::Hup => "SIGHUP",
            Signal::Int => "SIGINT",
            Signal::Term => "SIGTERM",
            Signal::Kill => "SIGKILL",
            Signal::Usr1 => "SIGUSR1",
            Signal::Usr2 => "SIGUSR2",
        }
    }

    /// Node's `os.constants.signals` numbers for the signals used in this crate.
    pub fn number(self) -> i32 {
        match self {
            Signal::Hup => 1,
            Signal::Int => 2,
            Signal::Kill => 9,
            Signal::Usr1 => 10,
            Signal::Usr2 => 12,
            Signal::Term => 15,
        }
    }
}

#[cfg(not(windows))]
fn send_signal(pid: i32, signal: Signal) -> bool {
    let number = match signal {
        Signal::Hup => libc::SIGHUP,
        Signal::Int => libc::SIGINT,
        Signal::Term => libc::SIGTERM,
        Signal::Kill => libc::SIGKILL,
        Signal::Usr1 => libc::SIGUSR1,
        Signal::Usr2 => libc::SIGUSR2,
    };
    unsafe { libc::kill(pid, number) == 0 }
}

#[cfg(windows)]
fn send_signal(pid: i32, _signal: Signal) -> bool {
    // Windows has no POSIX signals; terminating the process is the only equivalent.
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{OpenProcess, TerminateProcess, PROCESS_TERMINATE};
    unsafe {
        let handle = OpenProcess(PROCESS_TERMINATE, 0, pid as u32);
        if handle.is_null() {
            return false;
        }
        let ok = TerminateProcess(handle, 1) != 0;
        CloseHandle(handle);
        ok
    }
}

pub fn signal_exit_code(signal: Option<Signal>) -> Option<i32> {
    let signal = signal?;
    Some(128 + signal.number())
}

pub fn normalized_exit_code(code: Option<i32>, signal: Option<Signal>) -> Option<i32> {
    code.or_else(|| signal_exit_code(signal))
}

/// Wait for a child process to terminate without hanging on inherited stdio handles.
///
/// On Windows, daemonized descendants can inherit the child's stdout/stderr pipe
/// handles. In that case the child emits `exit`, but `close` can hang forever even
/// though the original process is already gone. We wait briefly for stdio to end,
/// then forcibly stop tracking the inherited handles.
pub async fn wait_for_child_process(mut child: tokio::process::Child) -> std::io::Result<Option<i32>> {
    let status = child.wait().await?;
    if let Some(mut stdout) = child.stdout.take() {
        let mut sink = Vec::new();
        let _ = tokio::time::timeout(Duration::from_millis(EXIT_STDIO_GRACE_MS), async {
            let _ = tokio::io::AsyncReadExt::read_to_end(&mut stdout, &mut sink).await;
        })
        .await;
    }
    if let Some(mut stderr) = child.stderr.take() {
        let mut sink = Vec::new();
        let _ = tokio::time::timeout(Duration::from_millis(EXIT_STDIO_GRACE_MS), async {
            let _ = tokio::io::AsyncReadExt::read_to_end(&mut stderr, &mut sink).await;
        })
        .await;
    }

    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        Ok(normalized_exit_code(status.code(), status.signal().map(|number| match number {
            1 => Signal::Hup,
            2 => Signal::Int,
            9 => Signal::Kill,
            15 => Signal::Term,
            _ => Signal::Term,
        })))
    }
    #[cfg(not(unix))]
    {
        Ok(status.code())
    }
}

/// Reads a child's captured stdout after the process finished.
pub fn read_child_stdout(output: &mut std::process::Child) -> String {
    let mut buffer = String::new();
    if let Some(stdout) = output.stdout.as_mut() {
        let _ = stdout.read_to_string(&mut buffer);
    }
    buffer
}

/// Bounded wait used by the synchronous clipboard helpers (`execSync` timeout).
pub fn wait_with_timeout(child: &mut std::process::Child, timeout_ms: u64) -> Option<std::process::ExitStatus> {
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    return None;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(_) => return None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_shell_detection_matches_the_typescript() {
        if !cfg!(windows) {
            assert!(!should_use_windows_shell("npm"));
            return;
        }
        assert!(should_use_windows_shell("npm"));
        assert!(should_use_windows_shell("C:\\tools\\npx.cmd"));
        assert!(should_use_windows_shell("build.bat"));
        assert!(!should_use_windows_shell("git"));
    }

    #[test]
    fn own_process_exists_and_is_not_a_zombie() {
        let pid = std::process::id() as i32;
        assert!(process_id_exists(pid));
        assert!(!is_zombie_process(pid));
        assert!(is_process_alive(pid));
    }

    #[test]
    fn signal_exit_codes_match_node() {
        assert_eq!(signal_exit_code(None), None);
        assert_eq!(signal_exit_code(Some(Signal::Term)), Some(128 + 15));
        assert_eq!(normalized_exit_code(Some(3), Some(Signal::Kill)), Some(3));
        assert_eq!(normalized_exit_code(None, Some(Signal::Kill)), Some(128 + 9));
    }

    #[test]
    fn spawn_sync_hidden_captures_stdout() {
        let command = if cfg!(windows) { "cmd" } else { "sh" };
        let args = if cfg!(windows) {
            vec!["/c".to_string(), "echo hi".to_string()]
        } else {
            vec!["-c".to_string(), "echo hi".to_string()]
        };
        let output = spawn_sync_hidden(
            command,
            &args,
            SpawnOptions {
                capture_stdout: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert!(String::from_utf8_lossy(&output.stdout).contains("hi"));
    }

    #[tokio::test]
    async fn wait_for_child_process_reports_the_exit_code() {
        let command = if cfg!(windows) { "cmd" } else { "sh" };
        let args = if cfg!(windows) {
            vec!["/c".to_string(), "exit 7".to_string()]
        } else {
            vec!["-c".to_string(), "exit 7".to_string()]
        };
        let handle = spawn_hidden(
            command,
            &args,
            SpawnOptions {
                capture_stdout: true,
                capture_stderr: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(wait_for_child_process(handle.child).await.unwrap(), Some(7));
    }

    #[test]
    fn process_group_probes_are_false_on_windows() {
        if cfg!(windows) {
            assert!(!process_group_exists(1234));
            assert!(!process_group_has_live_member(1234));
        }
    }
}
