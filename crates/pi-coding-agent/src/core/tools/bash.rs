//! Port of packages/coding-agent/src/core/tools/bash.ts

use std::sync::Arc;

use pi_agent_core::types::{AgentTool, AgentToolResult, AgentToolUpdateCallback};
use pi_agent_core::types::ContentBlock as AgentContentBlock;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use super::code_preview::preview_bash_command;
use super::output_accumulator::{OutputAccumulator, OutputAccumulatorOptions, OutputSnapshot};
use super::render_utils::{get_text_output, invalid_arg_text, str_value, RenderContentBlock, RenderResultLike, TextOutputOptions, ToolTheme};
use super::tool_definition_wrapper::wrap_tool_definition;
use super::truncate::{format_size, DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES};
use super::{ExtensionContext, ToolDefinition, ToolExecuteFn};

pub const BASH_SCHEMA: &str = r#"{
  "type": "object",
  "properties": {
    "command": { "type": "string", "description": "Bash command to execute" },
    "timeout": { "type": "number", "description": "Timeout in seconds (optional, no default timeout)" }
  },
  "required": ["command"]
}"#;

/// TypeScript `type BashToolInput = Static<typeof bashSchema>`.
#[derive(Debug, Clone, Default)]
pub struct BashToolInput {
    pub command: String,
    pub timeout: Option<f64>,
}

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct BashToolDetails {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub truncation: Option<super::truncate::TruncationResult>,
    #[serde(rename = "fullOutputPath", skip_serializing_if = "Option::is_none")]
    pub full_output_path: Option<String>,
}

/// Execution options passed to [`BashOperations::exec`].
#[derive(Clone)]
pub struct BashExecOptions {
    /// Receives each output chunk in arrival order.
    pub on_data: Arc<dyn Fn(&[u8]) + Send + Sync>,
    pub signal: Option<CancellationToken>,
    /// Timeout in seconds.
    pub timeout: Option<f64>,
    pub env: Option<Vec<(String, String)>>,
}

/// Pluggable operations for the bash tool.
/// Override these to delegate command execution to remote systems (for example SSH).
pub trait BashOperations: Send + Sync {
    /// Execute a command and stream output.
    /// Resolves to the exit code (`None` if killed).
    fn exec(
        &self,
        command: &str,
        cwd: &str,
        options: BashExecOptions,
    ) -> futures::future::BoxFuture<'static, Result<BashExecResult, String>>;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct BashExecResult {
    pub exit_code: Option<i32>,
}

/// Create bash operations using pi's built-in local shell execution backend.
///
/// This is useful for extensions that intercept user_bash and still want pi's
/// standard local shell behavior while wrapping or rewriting commands.
pub fn create_local_bash_operations(options: Option<LocalBashOperationsOptions>) -> Arc<dyn BashOperations> {
    Arc::new(LocalBashOperations {
        shell_path: options.and_then(|options| options.shell_path),
    })
}

#[derive(Debug, Clone, Default)]
pub struct LocalBashOperationsOptions {
    pub shell_path: Option<String>,
}

/// Local shell backend.
///
/// The TypeScript version spawns `getShellConfig(shellPath)` through
/// `spawnHidden` and tracks detached child pids. The port runs the same shell
/// command and streams stdout/stderr in arrival order; process-tree tracking is
/// owned by `utils/child-process.rs` in another slice.
pub struct LocalBashOperations {
    pub shell_path: Option<String>,
}

impl BashOperations for LocalBashOperations {
    fn exec(
        &self,
        command: &str,
        cwd: &str,
        options: BashExecOptions,
    ) -> futures::future::BoxFuture<'static, Result<BashExecResult, String>> {
        let command = command.to_string();
        let cwd = cwd.to_string();
        let shell_path = self.shell_path.clone();
        Box::pin(async move {
            if !std::path::Path::new(&cwd).exists() {
                return Err(format!(
                    "Working directory does not exist: {cwd}\nCannot execute bash commands."
                ));
            }

            let (shell, args) = get_shell_config(shell_path.as_deref());
            let mut command_builder = tokio::process::Command::new(&shell);
            for arg in &args {
                command_builder.arg(arg);
            }
            command_builder.arg(&command);
            command_builder.current_dir(&cwd);
            command_builder.stdin(std::process::Stdio::null());
            command_builder.stdout(std::process::Stdio::piped());
            command_builder.stderr(std::process::Stdio::piped());
            command_builder.env_clear();
            for (key, value) in options.env.clone().unwrap_or_else(get_shell_env) {
                command_builder.env(key, value);
            }
            if !cfg!(windows) {
                command_builder.process_group(0);
            }

            let mut child = match command_builder.spawn() {
                Ok(child) => child,
                Err(error) => return Err(error.to_string()),
            };

            let stdout = child.stdout.take();
            let stderr = child.stderr.take();
            let on_data = options.on_data.clone();
            let on_data_err = options.on_data.clone();

            let stdout_task = stdout.map(|stdout| {
                let on_data = on_data.clone();
                tokio::spawn(async move { pump(stdout, on_data).await })
            });
            let stderr_task = stderr.map(|stderr| {
                let on_data = on_data_err.clone();
                tokio::spawn(async move { pump(stderr, on_data).await })
            });

            let mut timed_out = false;
            let mut aborted = false;
            let wait = child.wait();
            tokio::pin!(wait);

            let signal = options.signal.clone();
            let timeout = options.timeout;
            let result = loop {
                let deadline = async {
                    match timeout {
                        Some(seconds) if seconds > 0.0 => {
                            tokio::time::sleep(std::time::Duration::from_secs_f64(seconds)).await;
                            "timeout"
                        }
                        _ => {
                            futures::future::pending::<&'static str>().await
                        }
                    }
                };
                let abort = async {
                    match signal.as_ref() {
                        Some(token) => {
                            token.cancelled().await;
                            "abort"
                        }
                        None => futures::future::pending::<&'static str>().await,
                    }
                };
                tokio::select! {
                    status = &mut wait => break Some(status),
                    reason = deadline => {
                        if reason == "timeout" { timed_out = true; }
                        break None;
                    }
                    reason = abort => {
                        if reason == "abort" { aborted = true; }
                        break None;
                    }
                }
            };

            if result.is_none() {
                kill_process_tree(child.id());
            }

            if let Some(task) = stdout_task {
                let _ = task.await;
            }
            if let Some(task) = stderr_task {
                let _ = task.await;
            }

            if aborted {
                return Err("aborted".to_string());
            }
            if timed_out {
                return Err(format!("timeout:{}", options.timeout.unwrap_or(0.0)));
            }

            match result {
                Some(Ok(status)) => Ok(BashExecResult {
                    exit_code: status.code(),
                }),
                Some(Err(error)) => Err(error.to_string()),
                None => Ok(BashExecResult { exit_code: None }),
            }
        })
    }
}

async fn pump<R>(mut reader: R, on_data: Arc<dyn Fn(&[u8]) + Send + Sync>)
where
    R: tokio::io::AsyncRead + Unpin,
{
    use tokio::io::AsyncReadExt;
    let mut buffer = vec![0u8; 8192];
    loop {
        match reader.read(&mut buffer).await {
            Ok(0) => break,
            Ok(read) => on_data(&buffer[..read]),
            Err(_) => break,
        }
    }
}

/// Port of `utils/shell.ts getShellConfig`.
pub fn get_shell_config(shell_path: Option<&str>) -> (String, Vec<String>) {
    if let Some(shell_path) = shell_path {
        return (shell_path.to_string(), vec!["-c".to_string()]);
    }
    if cfg!(windows) {
        let bash = std::env::var("PRIME_AGENT_BASH_SHELL").unwrap_or_else(|_| "bash".to_string());
        return (bash, vec!["-c".to_string()]);
    }
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string());
    (shell, vec!["-c".to_string()])
}

/// Port of `utils/shell.ts getShellEnv`.
pub fn get_shell_env() -> Vec<(String, String)> {
    std::env::vars().collect()
}

/// Port of `utils/shell.ts killProcessTree`.
pub fn kill_process_tree(pid: Option<u32>) {
    let Some(pid) = pid else {
        return;
    };
    kill_process_tree_raw(pid);
}

#[cfg(windows)]
fn kill_process_tree_raw(pid: u32) {
    let _ = std::process::Command::new("taskkill")
        .args(["/T", "/F", "/PID", &pid.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

#[cfg(unix)]
fn kill_process_tree_raw(pid: u32) {
    unsafe {
        libc::kill(-(pid as i32), libc::SIGKILL);
        libc::kill(pid as i32, libc::SIGKILL);
    }
}

/// TypeScript `interface BashSpawnContext`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BashSpawnContext {
    pub command: String,
    pub cwd: String,
    pub env: Vec<(String, String)>,
}

/// TypeScript `type BashSpawnHook`.
pub type BashSpawnHook = Arc<dyn Fn(BashSpawnContext) -> BashSpawnContext + Send + Sync>;

fn resolve_spawn_context(command: &str, cwd: &str, spawn_hook: Option<&BashSpawnHook>) -> BashSpawnContext {
    let base_context = BashSpawnContext {
        command: command.to_string(),
        cwd: cwd.to_string(),
        env: get_shell_env(),
    };
    match spawn_hook {
        Some(hook) => hook(base_context),
        None => base_context,
    }
}

/// TypeScript `interface BashToolOptions`.
#[derive(Clone, Default)]
pub struct BashToolOptions {
    /// Custom operations for command execution. Default: local shell
    pub operations: Option<Arc<dyn BashOperations>>,
    /// Command prefix prepended to every command (for example shell setup commands)
    pub command_prefix: Option<String>,
    /// Optional explicit shell path from settings
    pub shell_path: Option<String>,
    /// Hook to adjust command, cwd, or env before execution
    pub spawn_hook: Option<BashSpawnHook>,
}

pub const BASH_PREVIEW_LINES: usize = 5;
pub const BASH_UPDATE_THROTTLE_MS: u64 = 100;

/// TypeScript `type BashRenderState`.
#[derive(Default)]
pub struct BashRenderState {
    pub started_at: Option<f64>,
    pub ended_at: Option<f64>,
}

/// TypeScript `type BashResultRenderState`.
#[derive(Default)]
pub struct BashResultRenderState {
    pub cached_width: Option<usize>,
    pub cached_lines: Option<Vec<String>>,
    pub cached_skipped: Option<usize>,
}

pub fn format_duration(ms: f64) -> String {
    format!("{:.1}s", ms / 1000.0)
}

pub fn format_bash_call(args: Option<&BashToolInput>, theme: &dyn ToolTheme) -> String {
    let command = str_value(args.map(|args| &Value::String(args.command.clone())));
    let timeout = args.and_then(|args| args.timeout);
    let timeout_suffix = match timeout {
        Some(timeout) if timeout != 0.0 => theme.fg("muted", &format!(" (timeout {timeout}s)")),
        Some(timeout) if timeout == 0.0 => theme.fg("muted", &format!(" (timeout {timeout}s)")),
        _ => String::new(),
    };
    let command_display = match command {
        None => invalid_arg_text(theme),
        Some(command) if !command.is_empty() => {
            let preview = preview_bash_command(&command);
            let label = if preview.language == super::code_preview::CodePreviewLanguage::Bash {
                String::new()
            } else {
                format!("{}: ", preview.language.as_str())
            };
            if !preview.text.is_empty() {
                format!("{label}{}", preview.text)
            } else {
                command
            }
        }
        Some(_) => theme.fg("toolOutput", "..."),
    };
    format!("{}{}", theme.fg("toolTitle", &theme.bold(&format!("$ {command_display}"))), timeout_suffix)
}

/// TypeScript `type BashResultRenderComponent`.
///
/// The TypeScript component is a `Container` of child rows; the port keeps the
/// same children as a `Vec<String>` plus the cached preview state the renderer
/// reuses between frames.
#[derive(Default)]
pub struct BashResultRenderComponent {
    pub state: BashResultRenderState,
    pub children: Vec<String>,
}

impl BashResultRenderComponent {
    pub fn clear(&mut self) {
        self.children.clear();
    }

    pub fn add_child(&mut self, text: String) {
        self.children.push(text);
    }
}

/// Port of `rebuildBashResultRenderComponent`.
pub fn rebuild_bash_result_render_component(
    component: &mut BashResultRenderComponent,
    result: Option<&RenderResultLike>,
    details: Option<&BashToolDetails>,
    options: super::ToolRenderResultOptions,
    show_images: bool,
    include_image_dimensions: bool,
    show_expand_hint: bool,
    started_at: Option<f64>,
    ended_at: Option<f64>,
    theme: &dyn ToolTheme,
) {
    component.clear();

    let output = get_text_output(
        result,
        show_images,
        TextOutputOptions {
            include_image_dimensions: Some(include_image_dimensions),
        },
    )
    .trim()
    .to_string();

    if !output.is_empty() {
        let styled_output = output
            .split('\n')
            .map(|line| theme.fg("toolOutput", line))
            .collect::<Vec<String>>()
            .join("\n");

        if options.expanded {
            component.add_child(format!("\n{styled_output}"));
        } else {
            let state = &mut component.state;
            let lines: Vec<String> = styled_output.split('\n').map(str::to_string).collect();
            let width = BASH_PREVIEW_LINES;
            let skipped = lines.len().saturating_sub(width);
            let visual_lines = lines[skipped.min(lines.len())..].to_vec();
            state.cached_width = Some(width);
            state.cached_lines = Some(visual_lines.clone());
            state.cached_skipped = Some(skipped);
            if skipped > 0 {
                // `expandCollapseHint("app.tools.expand", false)` renders the
                // shortcut suffix; without the keybinding manager it stays empty.
                let hint = if show_expand_hint {
                    theme.fg("muted", &format!("... {skipped} earlier lines"))
                } else {
                    theme.fg("muted", &format!("... ({skipped} earlier lines)"))
                };
                component.add_child(String::new());
                component.add_child(hint);
            } else {
                component.add_child(String::new());
            }
            for line in visual_lines {
                component.add_child(line);
            }
        }
    }

    let truncation = details.and_then(|details| details.truncation.as_ref());
    let full_output_path = details.and_then(|details| details.full_output_path.clone());
    let is_truncated = truncation.map(|truncation| truncation.truncated).unwrap_or(false);
    if is_truncated || full_output_path.is_some() {
        let mut warnings: Vec<String> = Vec::new();
        if let Some(full_output_path) = full_output_path {
            warnings.push(format!("Full output: {full_output_path}"));
        }
        if is_truncated {
            let truncation = truncation.expect("truncation present");
            if truncation.truncated_by == Some(super::truncate::TruncatedBy::Lines) {
                warnings.push(format!(
                    "Truncated: showing {} of {} lines",
                    truncation.output_lines, truncation.total_lines
                ));
            } else {
                warnings.push(format!(
                    "Truncated: {} lines shown ({} limit)",
                    truncation.output_lines,
                    format_size(truncation.max_bytes.unwrap_or(DEFAULT_MAX_BYTES))
                ));
            }
        }
        component.add_child(format!(
            "\n{}",
            theme.fg("warning", &format!("[{}]", warnings.join(". ")))
        ));
    }

    if let Some(started_at) = started_at {
        let label = if options.is_partial { "Elapsed" } else { "Took" };
        let end_time = ended_at.unwrap_or_else(now_ms);
        component.add_child(format!(
            "\n{}",
            theme.fg("muted", &format!("{label} {}", format_duration(end_time - started_at)))
        ));
    }
}

fn now_ms() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs_f64() * 1000.0)
        .unwrap_or(0.0)
}

pub const BASH_TOOL_DESCRIPTION_TEMPLATE: &str =
    "Execute a bash command in the current working directory. Returns stdout and stderr. Output is truncated to last {lines} lines or {kb}KB (whichever is hit first). If truncated, full output is saved to a temp file. Optionally provide a timeout in seconds.";

pub fn bash_tool_description() -> String {
    BASH_TOOL_DESCRIPTION_TEMPLATE
        .replace("{lines}", &DEFAULT_MAX_LINES.to_string())
        .replace("{kb}", &(DEFAULT_MAX_BYTES / 1024).to_string())
}

/// Port of `createBashToolDefinition`'s execute body.
///
/// The TypeScript closure mutates one `OutputAccumulator` from the data handler
/// and from the finishing path; the shared `Arc<Mutex<_>>` keeps that single
/// instance without changing the emission order.
pub async fn execute_bash(
    cwd: &str,
    operations: Arc<dyn BashOperations>,
    command_prefix: Option<&str>,
    spawn_hook: Option<&BashSpawnHook>,
    input: &BashToolInput,
    signal: Option<CancellationToken>,
    on_update: Option<AgentToolUpdateCallback>,
) -> Result<(String, Option<BashToolDetails>), String> {
    let resolved_command = match command_prefix {
        Some(prefix) => format!("{prefix}\n{}", input.command),
        None => input.command.clone(),
    };
    let spawn_context = resolve_spawn_context(&resolved_command, cwd, spawn_hook);
    let output = Arc::new(std::sync::Mutex::new(OutputAccumulator::with_temp_file_prefix(
        OutputAccumulatorOptions::default(),
        "pi-bash",
    )));
    let last_update_at = Arc::new(std::sync::Mutex::new(0.0f64));

    if let Some(on_update) = on_update.as_ref() {
        on_update(AgentToolResult::new(Vec::new(), Value::Null));
    }

    let on_data: Arc<dyn Fn(&[u8]) + Send + Sync> = {
        let output = output.clone();
        let on_update = on_update.clone();
        let last_update_at = last_update_at.clone();
        Arc::new(move |data: &[u8]| {
            let mut accumulator = output.lock().expect("output lock");
            if accumulator.append(data).is_err() {
                return;
            }
            let Some(on_update) = on_update.as_ref() else {
                return;
            };
            let now = now_ms();
            let mut last_update = last_update_at.lock().expect("update lock");
            if now - *last_update < BASH_UPDATE_THROTTLE_MS as f64 {
                return;
            }
            *last_update = now;
            let snapshot = accumulator.snapshot();
            on_update(update_result(&snapshot));
        })
    };

    let exec_options = BashExecOptions {
        on_data,
        signal: signal.clone(),
        timeout: input.timeout,
        env: Some(spawn_context.env.clone()),
    };

    let result = operations
        .exec(&spawn_context.command, &spawn_context.cwd, exec_options)
        .await;

    let snapshot = finish_output(&output, on_update.as_ref());
    let (text, details) = format_output(&snapshot, &output);

    match result {
        Ok(exec_result) => {
            if let Some(exit_code) = exec_result.exit_code {
                if exit_code != 0 {
                    return Err(append_status(&text, &format!("Command exited with code {exit_code}")));
                }
            }
            Ok((text, details))
        }
        Err(error) => {
            if error == "aborted" {
                return Err(append_status(&text, "Command aborted"));
            }
            if let Some(timeout_secs) = error.strip_prefix("timeout:") {
                return Err(append_status(
                    &text,
                    &format!("Command timed out after {timeout_secs} seconds"),
                ));
            }
            Err(error)
        }
    }
}

fn update_result(snapshot: &OutputSnapshot) -> AgentToolResult {
    AgentToolResult::new(
        vec![AgentContentBlock::text(snapshot.content.clone())],
        serde_json::json!({
            "truncation": if snapshot.truncation.truncated {
                serde_json::to_value(&snapshot.truncation).unwrap_or(Value::Null)
            } else {
                Value::Null
            },
            "fullOutputPath": snapshot.full_output_path,
        }),
    )
}

fn finish_output(
    output: &Arc<std::sync::Mutex<OutputAccumulator>>,
    on_update: Option<&AgentToolUpdateCallback>,
) -> OutputSnapshot {
    let mut accumulator = output.lock().expect("output lock");
    accumulator.finish();
    if let Some(on_update) = on_update {
        on_update(update_result(&accumulator.snapshot()));
    }
    // Snapshot only after the spill settled: the advertised path is terminal.
    accumulator.close_temp_file();
    accumulator.snapshot()
}

fn format_output(snapshot: &OutputSnapshot, output: &Arc<std::sync::Mutex<OutputAccumulator>>) -> (String, Option<BashToolDetails>) {
    let truncation = snapshot.truncation.clone();
    let mut text = if snapshot.content.is_empty() {
        "(no output)".to_string()
    } else {
        snapshot.content.clone()
    };
    let mut details: Option<BashToolDetails> = None;
    if truncation.truncated {
        details = Some(BashToolDetails {
            truncation: Some(truncation.clone()),
            full_output_path: snapshot.full_output_path.clone(),
        });
        let start_line = truncation.total_lines - truncation.output_lines + 1;
        let end_line = truncation.total_lines;
        // A degraded spill has no path; never advertise "Full output: undefined".
        let location = match snapshot.full_output_path.as_ref() {
            Some(path) => format!(". Full output: {path}"),
            None => String::new(),
        };
        if truncation.last_line_partial {
            // The partial line is the first SHOWN line; trailing blanks can follow it.
            let last_line_bytes = output.lock().expect("output lock").get_last_line_bytes();
            let line_size = if last_line_bytes > 0 {
                format!(" (line is {})", format_size(last_line_bytes))
            } else {
                String::new()
            };
            text += &format!(
                "\n\n[Showing last {} of line {start_line}{line_size}{location}]",
                format_size(truncation.output_bytes)
            );
        } else if truncation.truncated_by == Some(super::truncate::TruncatedBy::Lines) {
            text += &format!(
                "\n\n[Showing lines {start_line}-{end_line} of {}{location}]",
                truncation.total_lines
            );
        } else {
            text += &format!(
                "\n\n[Showing lines {start_line}-{end_line} of {} ({} limit){location}]",
                truncation.total_lines,
                format_size(DEFAULT_MAX_BYTES)
            );
        }
    }
    (text, details)
}

fn append_status(text: &str, status: &str) -> String {
    if text.is_empty() {
        status.to_string()
    } else {
        format!("{text}\n\n{status}")
    }
}

/// Port of `createBashToolDefinition`.
pub fn create_bash_tool_definition(cwd: &str, options: Option<&BashToolOptions>) -> ToolDefinition<BashToolDetails> {
    let operations = options
        .and_then(|options| options.operations.clone())
        .unwrap_or_else(|| create_local_bash_operations(Some(LocalBashOperationsOptions {
            shell_path: options.and_then(|options| options.shell_path.clone()),
        })));
    let command_prefix = options.and_then(|options| options.command_prefix.clone());
    let spawn_hook = options.and_then(|options| options.spawn_hook.clone());
    let cwd = cwd.to_string();

    let execute: ToolExecuteFn<BashToolDetails> = Arc::new(
        move |_tool_call_id: String,
              params: Value,
              signal: Option<CancellationToken>,
              on_update: Option<AgentToolUpdateCallback>,
              _ctx: ExtensionContext| {
            let cwd = cwd.clone();
            let operations = operations.clone();
            let command_prefix = command_prefix.clone();
            let spawn_hook = spawn_hook.clone();
            Box::pin(async move {
                let input = BashToolInput {
                    command: params
                        .get("command")
                        .and_then(|command| command.as_str())
                        .unwrap_or_default()
                        .to_string(),
                    timeout: params.get("timeout").and_then(|timeout| timeout.as_f64()),
                };
                let (text, details) =
                    execute_bash(&cwd, operations, command_prefix.as_deref(), spawn_hook.as_ref(), &input, signal, on_update)
                        .await
                        .map_err(anyhow::Error::msg)?;
                Ok(AgentToolResult::new(
                    vec![AgentContentBlock::text(text)],
                    serde_json::to_value(details).unwrap_or(Value::Null),
                ))
            })
        },
    );

    ToolDefinition {
        name: "bash".to_string(),
        label: "bash".to_string(),
        description: bash_tool_description(),
        prompt_snippet: Some("Execute bash commands (ls, grep, find, etc.)".to_string()),
        parameters: serde_json::from_str(BASH_SCHEMA).expect("valid bash schema"),
        replay_built_in_tool_name: Some("bash".to_string()),
        execute,
        ..ToolDefinition::default()
    }
}

pub fn create_bash_tool(cwd: &str, options: Option<&BashToolOptions>) -> AgentTool {
    wrap_tool_definition(&create_bash_tool_definition(cwd, options), None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct RecordingOperations {
        calls: Arc<AtomicUsize>,
    }

    impl BashOperations for RecordingOperations {
        fn exec(
            &self,
            command: &str,
            _cwd: &str,
            options: BashExecOptions,
        ) -> futures::future::BoxFuture<'static, Result<BashExecResult, String>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let command = command.to_string();
            let on_data = options.on_data.clone();
            let signal = options.signal.clone();
            Box::pin(async move {
                if command.contains("fail") {
                    on_data(b"boom");
                    return Ok(BashExecResult { exit_code: Some(1) });
                }
                if command.contains("abort") {
                    return Err("aborted".to_string());
                }
                if command.contains("timeout") {
                    return Err("timeout:2".to_string());
                }
                on_data(b"hello\n");
                if let Some(token) = signal {
                    token.cancelled().await;
                }
                Ok(BashExecResult { exit_code: Some(0) })
            })
        }
    }

    fn recording(calls: Arc<AtomicUsize>) -> Arc<dyn BashOperations> {
        Arc::new(RecordingOperations { calls })
    }

    #[test]
    fn description_uses_default_limits() {
        let description = bash_tool_description();
        assert!(description.contains("last 2000 lines"));
        assert!(description.contains("50KB"));
    }

    #[test]
    fn format_bash_call_previews_runner_commands() {
        let args = BashToolInput {
            command: "npx tsx ../../node_modules/vitest/dist/cli.js --run test/a.test.ts".to_string(),
            timeout: Some(30.0),
        };
        let text = format_bash_call(Some(&args), &super::super::render_utils::PlainTheme);
        assert!(text.contains("$ vitest --run test/a.test.ts"));
        assert!(text.contains("(timeout 30s)"));
    }

    #[test]
    fn format_bash_call_marks_invalid_and_empty_arguments() {
        let empty = BashToolInput {
            command: String::new(),
            timeout: None,
        };
        let text = format_bash_call(Some(&empty), &super::super::render_utils::PlainTheme);
        assert!(text.contains("$ ..."));
    }

    #[test]
    fn format_duration_uses_one_decimal() {
        assert_eq!(format_duration(1500.0), "1.5s");
    }

    #[tokio::test]
    async fn execute_bash_streams_output_and_returns_details() {
        let calls = Arc::new(AtomicUsize::new(0));
        let input = BashToolInput {
            command: "echo hello".to_string(),
            timeout: None,
        };
        let (text, details) = execute_bash("/", recording(calls.clone()), None, None, &input, None, None)
            .await
            .expect("executed");
        assert_eq!(text, "hello\n");
        assert!(details.is_none());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn execute_bash_reports_non_zero_exit_code() {
        let calls = Arc::new(AtomicUsize::new(0));
        let input = BashToolInput {
            command: "fail".to_string(),
            timeout: None,
        };
        let error = execute_bash("/", recording(calls), None, None, &input, None, None)
            .await
            .expect_err("must fail");
        assert_eq!(error, "boom\n\nCommand exited with code 1");
    }

    #[tokio::test]
    async fn execute_bash_reports_abort_and_timeout_statuses() {
        let calls = Arc::new(AtomicUsize::new(0));
        let abort_input = BashToolInput {
            command: "abort".to_string(),
            timeout: None,
        };
        let abort_error = execute_bash("/", recording(calls.clone()), None, None, &abort_input, None, None)
            .await
            .expect_err("aborted");
        assert_eq!(abort_error, "Command aborted");

        let timeout_input = BashToolInput {
            command: "timeout".to_string(),
            timeout: Some(2.0),
        };
        let timeout_error = execute_bash("/", recording(calls), None, None, &timeout_input, None, None)
            .await
            .expect_err("timed out");
        assert_eq!(timeout_error, "Command timed out after 2 seconds");
    }

    #[tokio::test]
    async fn execute_bash_applies_command_prefix() {
        let calls = Arc::new(AtomicUsize::new(0));
        let input = BashToolInput {
            command: "echo hello".to_string(),
            timeout: None,
        };
        let (text, _) = execute_bash(
            "/",
            recording(calls),
            Some("set -e"),
            None,
            &input,
            None,
            None,
        )
        .await
        .expect("executed");
        assert_eq!(text, "hello\n");
    }

    #[tokio::test]
    async fn execute_bash_rejects_missing_working_directory() {
        let calls = Arc::new(AtomicUsize::new(0));
        let input = BashToolInput {
            command: "echo hello".to_string(),
            timeout: None,
        };
        let error = execute_bash(
            "/definitely/missing/dir",
            create_local_bash_operations(None),
            None,
            None,
            &input,
            None,
            None,
        )
        .await
        .expect_err("must reject");
        assert!(error.starts_with("Working directory does not exist: /definitely/missing/dir\nCannot execute bash commands."));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn get_shell_config_uses_explicit_shell_path() {
        let (shell, args) = get_shell_config(Some("/bin/zsh"));
        assert_eq!(shell, "/bin/zsh");
        assert_eq!(args, vec!["-c".to_string()]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::tools::render_utils::PlainTheme;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct RecordingOperations {
        calls: Arc<AtomicUsize>,
    }

    impl BashOperations for RecordingOperations {
        fn exec(
            &self,
            command: &str,
            _cwd: &str,
            options: BashExecOptions,
        ) -> futures::future::BoxFuture<'static, Result<BashExecResult, String>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let command = command.to_string();
            let on_data = options.on_data.clone();
            let signal = options.signal.clone();
            Box::pin(async move {
                if command.contains("fail") {
                    on_data(b"boom");
                    return Ok(BashExecResult { exit_code: Some(1) });
                }
                if command.contains("abort") {
                    return Err("aborted".to_string());
                }
                if command.contains("timeout") {
                    return Err("timeout:2".to_string());
                }
                on_data(b"hello\n");
                if let Some(token) = signal {
                    token.cancelled().await;
                }
                Ok(BashExecResult { exit_code: Some(0) })
            })
        }
    }

    fn recording(calls: Arc<AtomicUsize>) -> Arc<dyn BashOperations> {
        Arc::new(RecordingOperations { calls })
    }

    #[test]
    fn description_uses_default_limits() {
        let description = bash_tool_description();
        assert!(description.contains("last 2000 lines"));
        assert!(description.contains("50KB"));
    }

    #[test]
    fn format_bash_call_previews_runner_commands() {
        let args = BashToolInput {
            command: "npx tsx ../../node_modules/vitest/dist/cli.js --run test/a.test.ts".to_string(),
            timeout: Some(30.0),
        };
        let text = format_bash_call(Some(&args), &PlainTheme);
        assert!(text.contains("$ vitest --run test/a.test.ts"));
        assert!(text.contains("(timeout 30s)"));
    }

    #[test]
    fn format_bash_call_marks_empty_command() {
        let empty = BashToolInput {
            command: String::new(),
            timeout: None,
        };
        let text = format_bash_call(Some(&empty), &PlainTheme);
        assert!(text.contains("$ ..."));
    }

    #[test]
    fn format_duration_uses_one_decimal() {
        assert_eq!(format_duration(1500.0), "1.5s");
    }

    #[tokio::test]
    async fn execute_bash_streams_output_and_returns_details() {
        let calls = Arc::new(AtomicUsize::new(0));
        let input = BashToolInput {
            command: "echo hello".to_string(),
            timeout: None,
        };
        let (text, details) = execute_bash("/", recording(calls.clone()), None, None, &input, None, None)
            .await
            .expect("executed");
        assert_eq!(text, "hello\n");
        assert!(details.is_none());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn execute_bash_reports_non_zero_exit_code() {
        let calls = Arc::new(AtomicUsize::new(0));
        let input = BashToolInput {
            command: "fail".to_string(),
            timeout: None,
        };
        let error = execute_bash("/", recording(calls), None, None, &input, None, None)
            .await
            .expect_err("must fail");
        assert_eq!(error, "boom\n\nCommand exited with code 1");
    }

    #[tokio::test]
    async fn execute_bash_reports_abort_and_timeout_statuses() {
        let calls = Arc::new(AtomicUsize::new(0));
        let abort_input = BashToolInput {
            command: "abort".to_string(),
            timeout: None,
        };
        let abort_error = execute_bash("/", recording(calls.clone()), None, None, &abort_input, None, None)
            .await
            .expect_err("aborted");
        assert_eq!(abort_error, "Command aborted");

        let timeout_input = BashToolInput {
            command: "timeout".to_string(),
            timeout: Some(2.0),
        };
        let timeout_error = execute_bash("/", recording(calls), None, None, &timeout_input, None, None)
            .await
            .expect_err("timed out");
        assert_eq!(timeout_error, "Command timed out after 2 seconds");
    }

    #[tokio::test]
    async fn execute_bash_applies_command_prefix() {
        let calls = Arc::new(AtomicUsize::new(0));
        let input = BashToolInput {
            command: "echo hello".to_string(),
            timeout: None,
        };
        let (text, _) = execute_bash("/", recording(calls), Some("set -e"), None, &input, None, None)
            .await
            .expect("executed");
        assert_eq!(text, "hello\n");
    }

    #[tokio::test]
    async fn execute_bash_rejects_missing_working_directory() {
        let input = BashToolInput {
            command: "echo hello".to_string(),
            timeout: None,
        };
        let error = execute_bash(
            "/definitely/missing/dir",
            create_local_bash_operations(None),
            None,
            None,
            &input,
            None,
            None,
        )
        .await
        .expect_err("must reject");
        assert!(error.starts_with(
            "Working directory does not exist: /definitely/missing/dir\nCannot execute bash commands."
        ));
    }

    #[test]
    fn get_shell_config_uses_explicit_shell_path() {
        let (shell, args) = get_shell_config(Some("/bin/zsh"));
        assert_eq!(shell, "/bin/zsh");
        assert_eq!(args, vec!["-c".to_string()]);
    }

    #[test]
    fn render_component_shows_preview_hint_and_duration() {
        let result = RenderResultLike {
            content: vec![RenderContentBlock::from_text("l1\nl2\nl3\nl4\nl5\nl6\nl7")],
        };
        let mut component = BashResultRenderComponent::default();
        rebuild_bash_result_render_component(
            &mut component,
            Some(&result),
            None,
            super::super::ToolRenderResultOptions::default(),
            true,
            true,
            true,
            Some(0.0),
            Some(1000.0),
            &PlainTheme,
        );
        assert_eq!(component.state.cached_skipped, Some(2));
        assert!(component.children.iter().any(|child| child.contains("... 2 earlier lines")));
        assert!(component.children.iter().any(|child| child.contains("Took 1.0s")));
    }

    #[test]
    fn render_component_reports_truncation_warnings() {
        let result = RenderResultLike {
            content: vec![RenderContentBlock::from_text("out")],
        };
        let details = BashToolDetails {
            truncation: Some(super::super::truncate::TruncationResult {
                content: "out".to_string(),
                truncated: true,
                truncated_by: Some(super::super::truncate::TruncatedBy::Lines),
                total_lines: 10,
                total_bytes: 100,
                output_lines: 2,
                output_bytes: 8,
                last_line_partial: false,
                first_line_exceeds_limit: false,
                max_lines: 2,
                max_bytes: 1024,
            }),
            full_output_path: Some("/tmp/full.log".to_string()),
        };
        let mut component = BashResultRenderComponent::default();
        rebuild_bash_result_render_component(
            &mut component,
            Some(&result),
            Some(&details),
            super::super::ToolRenderResultOptions::default(),
            true,
            true,
            true,
            None,
            None,
            &PlainTheme,
        );
        let warning = component
            .children
            .iter()
            .find(|child| child.contains("Full output: /tmp/full.log"))
            .expect("warning row");
        assert!(warning.contains("Truncated: showing 2 of 10 lines"));
    }
}
