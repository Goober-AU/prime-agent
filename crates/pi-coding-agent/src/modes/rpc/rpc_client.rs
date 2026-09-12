//! Port of packages/coding-agent/src/modes/rpc/rpc-client.ts
//!
//! RPC client for programmatic access to the coding agent. Spawns the agent in
//! RPC mode and provides a typed API for all operations.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::AsyncReadExt;
use tokio::process::ChildStdin;
use tokio::sync::{oneshot, Mutex as AsyncMutex};

use crate::core::agent_messages::{AgentSessionMessageReceipt, AgentSessionMessageSafetyStatus};
use crate::core::refinement::refinement::RefinementResult;
use crate::core::session_stats::SessionStats;
use crate::modes::agent_connection::types::AgentConnectionHeartbeat;
use crate::modes::rpc::jsonl::{serialize_json_line, JsonlLineReader, JsonlLineReaderOptions, StringDecoder};
use crate::modes::rpc::rpc_types::{
    RpcCancelledPayload, RpcCommand, RpcExtensionUiRequest, RpcForkMessage, RpcForkMessagesPayload,
    RpcForkPayload, RpcHeartbeatPayload, RpcHeartbeatsPayload, RpcModelsPayload, RpcObservedSessionEvent,
    RpcPathPayload, RpcResponse, RpcSessionState, RpcSlashCommand, RpcTextPayload, RpcThinkingLevelPayload,
};
use crate::utils::child_process::{signal_process_group_or_process, spawn_hidden, Signal, SpawnOptions};

// ============================================================================
// Types
// ============================================================================

/// Extended response timeout for refine requests, which run an LLM pass.
pub const REFINE_REQUEST_TIMEOUT_MS: u64 = 10 * 60 * 1000;

/// Default response timeout for every other command.
pub const DEFAULT_REQUEST_TIMEOUT_MS: u64 = 30_000;

/// `RpcClientOptions`.
#[derive(Debug, Clone, Default)]
pub struct RpcClientOptions {
    /// Path to the CLI entry point (default: `dist/cli.js`).
    pub cli_path: Option<String>,
    /// Working directory for the agent.
    pub cwd: Option<String>,
    /// Environment variables.
    pub env: Option<Vec<(String, String)>>,
    /// Provider to use.
    pub provider: Option<String>,
    /// Model ID to use.
    pub model: Option<String>,
    /// Additional CLI arguments.
    pub args: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelInfo {
    pub provider: String,
    pub id: String,
    pub context_window: f64,
    pub reasoning: bool,
}

/// `{ provider: string; id: string }`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RpcModelIdentity {
    pub provider: String,
    pub id: String,
}

/// `{ model; thinkingLevel; isScoped }` returned by `cycle_model`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcCycleModelResult {
    pub model: RpcModelIdentity,
    pub thinking_level: pi_agent_core::types::ThinkingLevel,
    pub is_scoped: bool,
}

/// `RpcEventListener`.
///
/// The TypeScript declares `(event: AgentEvent) => void`. `pi-agent-core`'s
/// `AgentEvent` has a `to_json` projection but no JSON parser, so the port hands
/// listeners the same JSON object the wire carries.
pub type RpcEventListener = Arc<dyn Fn(&Value) + Send + Sync>;

/// `RpcObservedSessionListener`.
pub type RpcObservedSessionListener = Arc<dyn Fn(RpcObservedSessionEvent) + Send + Sync>;

// ============================================================================
// RPC Client
// ============================================================================

/// `class RpcClient`.
pub struct RpcClient {
    options: RpcClientOptions,
    inner: Arc<RpcClientInner>,
}

struct RpcClientInner {
    stderr: Mutex<String>,
    event_listeners: Mutex<Vec<RpcEventListener>>,
    observed_session_listeners: Mutex<Vec<RpcObservedSessionListener>>,
    pending_requests: Mutex<HashMap<String, oneshot::Sender<RpcResponse>>>,
    request_id: AtomicU64,
    stdin: AsyncMutex<Option<ChildStdin>>,
}

struct RpcProcess {
    child: crate::utils::child_process::ChildProcessHandle,
    stop_reading_stdout: Option<tokio::task::JoinHandle<()>>,
    stop_reading_stderr: Option<tokio::task::JoinHandle<()>>,
}

impl RpcClient {
    pub fn new(options: RpcClientOptions) -> Self {
        Self {
            options,
            inner: Arc::new(RpcClientInner {
                stderr: Mutex::new(String::new()),
                event_listeners: Mutex::new(Vec::new()),
                observed_session_listeners: Mutex::new(Vec::new()),
                pending_requests: Mutex::new(HashMap::new()),
                request_id: AtomicU64::new(0),
                stdin: AsyncMutex::new(None),
            }),
        }
    }

    pub fn options(&self) -> &RpcClientOptions {
        &self.options
    }

    fn process_slot() -> &'static AsyncMutex<HashMap<usize, RpcProcess>> {
        // The child process is owned per client instance; the slot keeps the
        // handle reachable from `&self` methods without an extra lock layer on
        // the public type.
        static PROCESSES: once_cell::sync::Lazy<AsyncMutex<HashMap<usize, RpcProcess>>> =
            once_cell::sync::Lazy::new(|| AsyncMutex::new(HashMap::new()));
        &PROCESSES
    }

    fn key(&self) -> usize {
        Arc::as_ptr(&self.inner) as usize
    }

    /// Start the RPC agent process.
    pub async fn start(&self) -> Result<(), String> {
        if Self::process_slot().lock().await.contains_key(&self.key()) {
            return Err("Client already started".to_string());
        }

        let cli_path = self
            .options
            .cli_path
            .clone()
            .unwrap_or_else(|| "dist/cli.js".to_string());
        let mut args = vec!["--mode".to_string(), "rpc".to_string()];
        if let Some(provider) = &self.options.provider {
            args.push("--provider".to_string());
            args.push(provider.clone());
        }
        if let Some(model) = &self.options.model {
            args.push("--model".to_string());
            args.push(model.clone());
        }
        if let Some(extra) = &self.options.args {
            args.extend(extra.iter().cloned());
        }

        let mut env: Vec<(String, String)> = std::env::vars().collect();
        if let Some(overrides) = &self.options.env {
            for (key, value) in overrides {
                env.retain(|(existing, _)| existing != key);
                env.push((key.clone(), value.clone()));
            }
        }

        let mut command_args = vec![cli_path];
        command_args.extend(args);

        let mut handle = spawn_hidden(
            "node",
            &command_args,
            SpawnOptions {
                cwd: self.options.cwd.clone(),
                env: Some(env),
                detached: false,
                shell: false,
                capture_stdout: true,
                capture_stderr: true,
                stdin_piped: true,
            },
        )
        .map_err(|error| format!("Failed to start the RPC agent process: {error}"))?;

        let stdin = handle.child.stdin.take();
        *self.inner.stdin.lock().await = stdin;

        // Collect stderr for debugging.
        let stderr_task = match handle.child.stderr.take() {
            Some(mut stderr) => {
                let inner = self.inner.clone();
                Some(tokio::spawn(async move {
                    let mut buffer = vec![0u8; 8192];
                    loop {
                        match stderr.read(&mut buffer).await {
                            Ok(0) => break,
                            Ok(read) => {
                                let text = String::from_utf8_lossy(&buffer[..read]).to_string();
                                inner.stderr.lock().expect("stderr poisoned").push_str(&text);
                                let _ = std::io::Write::write_all(&mut std::io::stderr(), &buffer[..read]);
                            }
                            Err(_) => break,
                        }
                    }
                }))
            }
            None => None,
        };

        // Set up the strict JSONL reader for stdout.
        let stdout_task = match handle.child.stdout.take() {
            Some(mut stdout) => {
                let inner = self.inner.clone();
                Some(tokio::spawn(async move {
                    let mut reader = JsonlLineReader::new(
                        Arc::new(move |line: String| {
                            handle_line(&inner, &line);
                        }),
                        JsonlLineReaderOptions::default(),
                    );
                    let mut decoder = StringDecoder::new();
                    let mut buffer = vec![0u8; 8192];
                    loop {
                        match stdout.read(&mut buffer).await {
                            Ok(0) => break,
                            Ok(read) => reader.push(&decoder.write(&buffer[..read])),
                            Err(_) => break,
                        }
                    }
                    let tail = decoder.end();
                    if !tail.is_empty() {
                        reader.push(&tail);
                    }
                    reader.end();
                }))
            }
            None => None,
        };

        Self::process_slot().lock().await.insert(
            self.key(),
            RpcProcess {
                child: handle,
                stop_reading_stdout: stdout_task,
                stop_reading_stderr: stderr_task,
            },
        );

        // Wait a moment for the process to initialize.
        tokio::time::sleep(Duration::from_millis(100)).await;

        let exited = {
            let mut slot = Self::process_slot().lock().await;
            match slot.get_mut(&self.key()) {
                Some(process) => match process.child.child.try_wait() {
                    Ok(Some(status)) => Some(status.code()),
                    Ok(None) => None,
                    Err(_) => None,
                },
                None => None,
            }
        };
        if let Some(code) = exited {
            let stderr = self.get_stderr();
            return Err(format!(
                "Agent process exited immediately with code {}. Stderr: {}",
                code.map(|code| code.to_string()).unwrap_or_else(|| "null".to_string()),
                stderr
            ));
        }

        Ok(())
    }

    /// Stop the RPC agent process.
    pub async fn stop(&self) -> Result<(), String> {
        let process = Self::process_slot().lock().await.remove(&self.key());
        let Some(mut process) = process else {
            return Ok(());
        };

        if let Some(task) = process.stop_reading_stdout.take() {
            task.abort();
        }
        if let Some(task) = process.stop_reading_stderr.take() {
            task.abort();
        }
        *self.inner.stdin.lock().await = None;

        if let Some(pid) = process.child.child.id() {
            signal_process_group_or_process(pid as i32, Signal::Term);
        }

        // Wait for the process to exit, then escalate.
        let waited = tokio::time::timeout(Duration::from_millis(1000), process.child.child.wait()).await;
        if waited.is_err() {
            let _ = process.child.child.start_kill();
            let _ = process.child.child.wait().await;
        }

        self.inner
            .pending_requests
            .lock()
            .expect("pending requests poisoned")
            .clear();
        Ok(())
    }

    /// Subscribe to agent events.
    pub fn on_event(&self, listener: RpcEventListener) -> Arc<dyn Fn() + Send + Sync> {
        self.inner
            .event_listeners
            .lock()
            .expect("event listeners poisoned")
            .push(listener.clone());
        let listeners = self.inner.event_listeners.clone();
        Arc::new(move || {
            let mut guard = listeners.lock().expect("event listeners poisoned");
            if let Some(index) = guard.iter().position(|entry| Arc::ptr_eq(entry, &listener)) {
                guard.remove(index);
            }
        })
    }

    pub fn on_observed_session_event(
        &self,
        listener: RpcObservedSessionListener,
    ) -> Arc<dyn Fn() + Send + Sync> {
        self.inner
            .observed_session_listeners
            .lock()
            .expect("observed session listeners poisoned")
            .push(listener.clone());
        let listeners = self.inner.observed_session_listeners.clone();
        Arc::new(move || {
            let mut guard = listeners.lock().expect("observed session listeners poisoned");
            if let Some(index) = guard.iter().position(|entry| Arc::ptr_eq(entry, &listener)) {
                guard.remove(index);
            }
        })
    }

    /// Get collected stderr output (useful for debugging).
    pub fn get_stderr(&self) -> String {
        self.inner.stderr.lock().expect("stderr poisoned").clone()
    }

    // =========================================================================
    // Command Methods
    // =========================================================================

    /// Send a prompt to the agent.
    ///
    /// Returns immediately after sending; use `on_event` to receive streaming
    /// events. Use `wait_for_idle` to wait for completion.
    pub async fn prompt(&self, message: &str, images: Option<Vec<Value>>) -> Result<(), String> {
        self.send(
            RpcCommand::Prompt {
                id: None,
                message: message.to_string(),
                images,
                streaming_behavior: None,
            },
            DEFAULT_REQUEST_TIMEOUT_MS,
        )
        .await
        .map(|_| ())
    }

    /// Queue a steering message to interrupt the agent mid-run.
    pub async fn steer(&self, message: &str, images: Option<Vec<Value>>) -> Result<(), String> {
        self.send(
            RpcCommand::Steer {
                id: None,
                message: message.to_string(),
                images,
            },
            DEFAULT_REQUEST_TIMEOUT_MS,
        )
        .await
        .map(|_| ())
    }

    /// Queue a follow-up message to be processed after the agent finishes.
    pub async fn follow_up(&self, message: &str, images: Option<Vec<Value>>) -> Result<(), String> {
        self.send(
            RpcCommand::FollowUp {
                id: None,
                message: message.to_string(),
                images,
            },
            DEFAULT_REQUEST_TIMEOUT_MS,
        )
        .await
        .map(|_| ())
    }

    /// Abort current operation.
    pub async fn abort(&self) -> Result<(), String> {
        self.send(RpcCommand::Abort { id: None }, DEFAULT_REQUEST_TIMEOUT_MS)
            .await
            .map(|_| ())
    }

    /// Start a new session, optionally with parent tracking.
    ///
    /// Returns `cancelled: true` when an extension cancelled the new session.
    pub async fn new_session(&self, parent_session: Option<&str>) -> Result<RpcCancelledPayload, String> {
        let response = self
            .send(
                RpcCommand::NewSession {
                    id: None,
                    parent_session: parent_session.map(str::to_string),
                },
                DEFAULT_REQUEST_TIMEOUT_MS,
            )
            .await?;
        self.get_data(response)
    }

    /// Get current session state.
    pub async fn get_state(&self) -> Result<RpcSessionState, String> {
        let response = self
            .send(RpcCommand::GetState { id: None }, DEFAULT_REQUEST_TIMEOUT_MS)
            .await?;
        self.get_data(response)
    }

    /// Set model by provider and ID.
    pub async fn set_model(&self, provider: &str, model_id: &str) -> Result<RpcModelIdentity, String> {
        let response = self
            .send(
                RpcCommand::SetModel {
                    id: None,
                    provider: provider.to_string(),
                    model_id: model_id.to_string(),
                },
                DEFAULT_REQUEST_TIMEOUT_MS,
            )
            .await?;
        self.get_data(response)
    }

    /// Cycle to next model.
    pub async fn cycle_model(&self) -> Result<Option<RpcCycleModelResult>, String> {
        let response = self
            .send(RpcCommand::CycleModel { id: None }, DEFAULT_REQUEST_TIMEOUT_MS)
            .await?;
        self.get_data(response)
    }

    /// Get list of available models.
    pub async fn get_available_models(&self) -> Result<Vec<ModelInfo>, String> {
        let response = self
            .send(RpcCommand::GetAvailableModels { id: None }, DEFAULT_REQUEST_TIMEOUT_MS)
            .await?;
        let payload: RpcModelsPayload = self.get_data(response)?;
        Ok(payload.models)
    }

    /// Set thinking level.
    pub async fn set_thinking_level(&self, level: pi_agent_core::types::ThinkingLevel) -> Result<(), String> {
        self.send(
            RpcCommand::SetThinkingLevel { id: None, level },
            DEFAULT_REQUEST_TIMEOUT_MS,
        )
        .await
        .map(|_| ())
    }

    /// Cycle thinking level.
    pub async fn cycle_thinking_level(&self) -> Result<Option<RpcThinkingLevelPayload>, String> {
        let response = self
            .send(RpcCommand::CycleThinkingLevel { id: None }, DEFAULT_REQUEST_TIMEOUT_MS)
            .await?;
        self.get_data(response)
    }

    /// Set steering mode.
    pub async fn set_steering_mode(&self, mode: &str) -> Result<(), String> {
        self.send(
            RpcCommand::SetSteeringMode {
                id: None,
                mode: mode.to_string(),
            },
            DEFAULT_REQUEST_TIMEOUT_MS,
        )
        .await
        .map(|_| ())
    }

    /// Set follow-up mode.
    pub async fn set_follow_up_mode(&self, mode: &str) -> Result<(), String> {
        self.send(
            RpcCommand::SetFollowUpMode {
                id: None,
                mode: mode.to_string(),
            },
            DEFAULT_REQUEST_TIMEOUT_MS,
        )
        .await
        .map(|_| ())
    }

    /// Compact session context.
    ///
    /// blocked_on: `core/compaction`'s `CompactionResult` has no serde impls, so
    /// the port returns the raw JSON payload of the response.
    pub async fn compact(&self, custom_instructions: Option<&str>) -> Result<Value, String> {
        let response = self
            .send(
                RpcCommand::Compact {
                    id: None,
                    custom_instructions: custom_instructions.map(str::to_string),
                },
                DEFAULT_REQUEST_TIMEOUT_MS,
            )
            .await?;
        self.get_data(response)
    }

    /// Refine editable continual harness state.
    pub async fn refine(
        &self,
        options: RefineOptions,
    ) -> Result<RefinementResult, String> {
        // Refinement runs an LLM pass that routinely exceeds the default 30s
        // response timeout, so use the same extended window as the daemon refine
        // path.
        let response = self
            .send(
                RpcCommand::Refine {
                    id: None,
                    instructions: options.instructions,
                    rollback_id: options.rollback_id,
                    global: options.global,
                },
                REFINE_REQUEST_TIMEOUT_MS,
            )
            .await?;
        self.get_data(response)
    }

    /// Set auto-compaction enabled/disabled.
    pub async fn set_auto_compaction(&self, enabled: bool) -> Result<(), String> {
        self.send(
            RpcCommand::SetAutoCompaction { id: None, enabled },
            DEFAULT_REQUEST_TIMEOUT_MS,
        )
        .await
        .map(|_| ())
    }

    /// Set auto-retry enabled/disabled.
    pub async fn set_auto_retry(&self, enabled: bool) -> Result<(), String> {
        self.send(RpcCommand::SetAutoRetry { id: None, enabled }, DEFAULT_REQUEST_TIMEOUT_MS)
            .await
            .map(|_| ())
    }

    /// Abort in-progress retry.
    pub async fn abort_retry(&self) -> Result<(), String> {
        self.send(RpcCommand::AbortRetry { id: None }, DEFAULT_REQUEST_TIMEOUT_MS)
            .await
            .map(|_| ())
    }

    /// Execute a bash command.
    ///
    /// blocked_on: `core/bash-executor` has not landed, so the port returns the
    /// raw JSON payload of the response.
    pub async fn bash(&self, command: &str) -> Result<Value, String> {
        let response = self
            .send(
                RpcCommand::Bash {
                    id: None,
                    command: command.to_string(),
                },
                DEFAULT_REQUEST_TIMEOUT_MS,
            )
            .await?;
        self.get_data(response)
    }

    /// Abort running bash command.
    pub async fn abort_bash(&self) -> Result<(), String> {
        self.send(RpcCommand::AbortBash { id: None }, DEFAULT_REQUEST_TIMEOUT_MS)
            .await
            .map(|_| ())
    }

    /// Get session statistics.
    pub async fn get_session_stats(&self) -> Result<SessionStats, String> {
        let response = self
            .send(RpcCommand::GetSessionStats { id: None }, DEFAULT_REQUEST_TIMEOUT_MS)
            .await?;
        self.get_data(response)
    }

    /// Export session to HTML.
    pub async fn export_html(&self, output_path: Option<&str>) -> Result<RpcPathPayload, String> {
        let response = self
            .send(
                RpcCommand::ExportHtml {
                    id: None,
                    output_path: output_path.map(str::to_string),
                },
                DEFAULT_REQUEST_TIMEOUT_MS,
            )
            .await?;
        self.get_data(response)
    }

    /// Switch to a different session file.
    ///
    /// Returns `cancelled: true` when an extension cancelled the switch.
    pub async fn switch_session(&self, session_path: &str) -> Result<RpcCancelledPayload, String> {
        let response = self
            .send(
                RpcCommand::SwitchSession {
                    id: None,
                    session_path: session_path.to_string(),
                },
                DEFAULT_REQUEST_TIMEOUT_MS,
            )
            .await?;
        self.get_data(response)
    }

    /// Fork from a specific message.
    pub async fn fork(&self, entry_id: &str) -> Result<RpcForkPayload, String> {
        let response = self
            .send(
                RpcCommand::Fork {
                    id: None,
                    entry_id: entry_id.to_string(),
                },
                DEFAULT_REQUEST_TIMEOUT_MS,
            )
            .await?;
        self.get_data(response)
    }

    /// Clone the current active branch into a new session.
    pub async fn clone(&self) -> Result<RpcCancelledPayload, String> {
        let response = self
            .send(RpcCommand::Clone { id: None }, DEFAULT_REQUEST_TIMEOUT_MS)
            .await?;
        self.get_data(response)
    }

    /// Get messages available for forking.
    pub async fn get_fork_messages(&self) -> Result<Vec<RpcForkMessage>, String> {
        let response = self
            .send(RpcCommand::GetForkMessages { id: None }, DEFAULT_REQUEST_TIMEOUT_MS)
            .await?;
        let payload: RpcForkMessagesPayload = self.get_data(response)?;
        Ok(payload.messages)
    }

    /// Get text of last assistant message.
    pub async fn get_last_assistant_text(&self) -> Result<Option<String>, String> {
        let response = self
            .send(RpcCommand::GetLastAssistantText { id: None }, DEFAULT_REQUEST_TIMEOUT_MS)
            .await?;
        let payload: RpcTextPayload = self.get_data(response)?;
        Ok(payload.text)
    }

    /// Set the session display name.
    pub async fn set_session_name(&self, name: &str) -> Result<(), String> {
        self.send(
            RpcCommand::SetSessionName {
                id: None,
                name: name.to_string(),
            },
            DEFAULT_REQUEST_TIMEOUT_MS,
        )
        .await
        .map(|_| ())
    }

    /// Get all messages in the session.
    pub async fn get_messages(&self) -> Result<Vec<pi_agent_core::types::AgentMessage>, String> {
        let response = self
            .send(RpcCommand::GetMessages { id: None }, DEFAULT_REQUEST_TIMEOUT_MS)
            .await?;
        let payload: crate::modes::rpc::rpc_types::RpcMessagesPayload = self.get_data(response)?;
        Ok(payload.messages)
    }

    pub async fn send_agent_message(
        &self,
        target_active_session_id: &str,
        message: &str,
    ) -> Result<AgentSessionMessageReceipt, String> {
        let response = self
            .send(
                RpcCommand::SendMessage {
                    id: None,
                    target_active_session_id: target_active_session_id.to_string(),
                    message: message.to_string(),
                },
                DEFAULT_REQUEST_TIMEOUT_MS,
            )
            .await?;
        self.get_data(response)
    }

    pub async fn get_agent_message_status(&self) -> Result<AgentSessionMessageSafetyStatus, String> {
        let response = self
            .send(RpcCommand::AgentMessagesStatus { id: None }, DEFAULT_REQUEST_TIMEOUT_MS)
            .await?;
        self.get_data(response)
    }

    pub async fn pause_agent_messages(&self) -> Result<AgentSessionMessageSafetyStatus, String> {
        let response = self
            .send(RpcCommand::AgentMessagesPause { id: None }, DEFAULT_REQUEST_TIMEOUT_MS)
            .await?;
        self.get_data(response)
    }

    pub async fn resume_agent_messages(&self) -> Result<AgentSessionMessageSafetyStatus, String> {
        let response = self
            .send(RpcCommand::AgentMessagesResume { id: None }, DEFAULT_REQUEST_TIMEOUT_MS)
            .await?;
        self.get_data(response)
    }

    pub async fn clear_agent_messages(&self) -> Result<f64, String> {
        let response = self
            .send(RpcCommand::AgentMessagesClear { id: None }, DEFAULT_REQUEST_TIMEOUT_MS)
            .await?;
        let payload: crate::modes::rpc::rpc_types::RpcClearedPayload = self.get_data(response)?;
        Ok(payload.cleared)
    }

    /// blocked_on: `core/cron-jobs` has not landed; jobs stay raw JSON.
    pub async fn list_schedules(&self, include_inactive: Option<bool>) -> Result<Vec<Value>, String> {
        let response = self
            .send(
                RpcCommand::ListSchedules {
                    id: None,
                    include_inactive,
                },
                DEFAULT_REQUEST_TIMEOUT_MS,
            )
            .await?;
        let payload: crate::modes::rpc::rpc_types::RpcJobsPayload = self.get_data(response)?;
        Ok(payload.jobs)
    }

    pub async fn add_schedule(&self, schedule: &str, prompt: &str) -> Result<Value, String> {
        let response = self
            .send(
                RpcCommand::AddSchedule {
                    id: None,
                    schedule: schedule.to_string(),
                    prompt: prompt.to_string(),
                },
                DEFAULT_REQUEST_TIMEOUT_MS,
            )
            .await?;
        let payload: crate::modes::rpc::rpc_types::RpcJobPayload = self.get_data(response)?;
        Ok(payload.job)
    }

    pub async fn cancel_schedule(&self, job_id: &str) -> Result<Value, String> {
        let response = self
            .send(
                RpcCommand::CancelSchedule {
                    id: None,
                    job_id: job_id.to_string(),
                },
                DEFAULT_REQUEST_TIMEOUT_MS,
            )
            .await?;
        let payload: crate::modes::rpc::rpc_types::RpcJobPayload = self.get_data(response)?;
        Ok(payload.job)
    }

    pub async fn list_heartbeats(&self) -> Result<Vec<AgentConnectionHeartbeat>, String> {
        let response = self
            .send(RpcCommand::ListHeartbeats { id: None }, DEFAULT_REQUEST_TIMEOUT_MS)
            .await?;
        let payload: RpcHeartbeatsPayload = self.get_data(response)?;
        Ok(payload.heartbeats)
    }

    pub async fn get_heartbeat(&self) -> Result<Option<Value>, String> {
        let response = self
            .send(RpcCommand::GetHeartbeat { id: None }, DEFAULT_REQUEST_TIMEOUT_MS)
            .await?;
        let payload: RpcHeartbeatPayload = self.get_data(response)?;
        Ok(payload.heartbeat)
    }

    pub async fn set_heartbeat(
        &self,
        schedule: &str,
        prompt: &str,
        delivery_mode: Option<&str>,
    ) -> Result<Value, String> {
        let response = self
            .send(
                RpcCommand::SetHeartbeat {
                    id: None,
                    schedule: schedule.to_string(),
                    prompt: prompt.to_string(),
                    delivery_mode: delivery_mode.map(str::to_string),
                },
                DEFAULT_REQUEST_TIMEOUT_MS,
            )
            .await?;
        let payload: RpcHeartbeatPayload = self.get_data(response)?;
        payload
            .heartbeat
            .ok_or_else(|| "Daemon did not return the created heartbeat".to_string())
    }

    pub async fn update_heartbeat(&self, action: Value) -> Result<Option<Value>, String> {
        let response = self
            .send(
                RpcCommand::UpdateHeartbeat { id: None, action },
                DEFAULT_REQUEST_TIMEOUT_MS,
            )
            .await?;
        let payload: RpcHeartbeatPayload = self.get_data(response)?;
        Ok(payload.heartbeat)
    }

    pub async fn manage_heartbeat(
        &self,
        active_session_id: &str,
        job_id: &str,
        action: Value,
    ) -> Result<Value, String> {
        let response = self
            .send(
                RpcCommand::ManageHeartbeat {
                    id: None,
                    active_session_id: active_session_id.to_string(),
                    job_id: job_id.to_string(),
                    action,
                },
                DEFAULT_REQUEST_TIMEOUT_MS,
            )
            .await?;
        let payload: RpcHeartbeatPayload = self.get_data(response)?;
        payload
            .heartbeat
            .ok_or_else(|| "Daemon did not return the managed heartbeat".to_string())
    }

    pub async fn observe(&self, active_session_id: &str) -> Result<Vec<pi_agent_core::types::AgentMessage>, String> {
        let response = self
            .send(
                RpcCommand::Observe {
                    id: None,
                    active_session_id: active_session_id.to_string(),
                },
                DEFAULT_REQUEST_TIMEOUT_MS,
            )
            .await?;
        let payload: crate::modes::rpc::rpc_types::RpcMessagesPayload = self.get_data(response)?;
        Ok(payload.messages)
    }

    pub async fn unobserve(&self, active_session_id: &str) -> Result<(), String> {
        self.send(
            RpcCommand::Unobserve {
                id: None,
                active_session_id: active_session_id.to_string(),
            },
            DEFAULT_REQUEST_TIMEOUT_MS,
        )
        .await
        .map(|_| ())
    }

    /// Get available commands (extension commands, prompt templates, skills).
    pub async fn get_commands(&self) -> Result<Vec<RpcSlashCommand>, String> {
        let response = self
            .send(RpcCommand::GetCommands { id: None }, DEFAULT_REQUEST_TIMEOUT_MS)
            .await?;
        let payload: crate::modes::rpc::rpc_types::RpcCommandsPayload = self.get_data(response)?;
        Ok(payload.commands)
    }

    // =========================================================================
    // Helpers
    // =========================================================================

    /// Wait for the agent to become idle (no streaming).
    ///
    /// Resolves when an `agent_end` event is received.
    pub async fn wait_for_idle(&self, timeout_ms: u64) -> Result<(), String> {
        let (sender, receiver) = oneshot::channel::<()>();
        let sender = Arc::new(Mutex::new(Some(sender)));
        let sink = sender.clone();
        let unsubscribe = self.on_event(Arc::new(move |event: &Value| {
            if event.get("type").and_then(Value::as_str) == Some("agent_end") {
                if let Some(sender) = sink.lock().expect("idle sender poisoned").take() {
                    let _ = sender.send(());
                }
            }
        }));
        let waited = tokio::time::timeout(Duration::from_millis(timeout_ms), receiver).await;
        unsubscribe();
        match waited {
            Ok(Ok(())) => Ok(()),
            // A settled-by-stop listener never resolves, matching the TypeScript
            // promise that stays pending.
            Ok(Err(_)) => std::future::pending::<Result<(), String>>().await,
            Err(_) => Err(format!(
                "Timeout waiting for agent to become idle. Stderr: {}",
                self.get_stderr()
            )),
        }
    }

    /// Collect events until the agent becomes idle.
    pub async fn collect_events(&self, timeout_ms: u64) -> Result<Vec<Value>, String> {
        let events: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
        let (sender, receiver) = oneshot::channel::<()>();
        let sender = Arc::new(Mutex::new(Some(sender)));
        let sink = sender.clone();
        let collected = events.clone();
        let unsubscribe = self.on_event(Arc::new(move |event: &Value| {
            collected.lock().expect("events poisoned").push(event.clone());
            if event.get("type").and_then(Value::as_str) == Some("agent_end") {
                if let Some(sender) = sink.lock().expect("idle sender poisoned").take() {
                    let _ = sender.send(());
                }
            }
        }));
        let waited = tokio::time::timeout(Duration::from_millis(timeout_ms), receiver).await;
        unsubscribe();
        match waited {
            Ok(Ok(())) => Ok(Arc::try_unwrap(events)
                .map(|events| events.into_inner().expect("events poisoned"))
                .unwrap_or_else(|events| events.lock().expect("events poisoned").clone())),
            Ok(Err(_)) => std::future::pending::<Result<Vec<Value>, String>>().await,
            Err(_) => Err(format!("Timeout collecting events. Stderr: {}", self.get_stderr())),
        }
    }

    /// Send a prompt and wait for completion, returning all events.
    pub async fn prompt_and_wait(
        &self,
        message: &str,
        images: Option<Vec<Value>>,
        timeout_ms: u64,
    ) -> Result<Vec<Value>, String> {
        let events = self.collect_events(timeout_ms);
        let prompt = self.prompt(message, images);
        let (events, prompt) = tokio::join!(events, prompt);
        prompt?;
        events
    }

    // =========================================================================
    // Internal
    // =========================================================================

    async fn send(&self, command: RpcCommand, timeout_ms: u64) -> Result<RpcResponse, String> {
        let request_id = self.inner.request_id.fetch_add(1, Ordering::SeqCst) + 1;
        let id = format!("req_{request_id}");
        let full_command = with_id(&command, &id);
        let line = serialize_json_line(&full_command);

        let (sender, receiver) = oneshot::channel();
        self.inner
            .pending_requests
            .lock()
            .expect("pending requests poisoned")
            .insert(id.clone(), sender);

        {
            let mut guard = self.inner.stdin.lock().await;
            let Some(stdin) = guard.as_mut() else {
                self.inner
                    .pending_requests
                    .lock()
                    .expect("pending requests poisoned")
                    .remove(&id);
                return Err("Client not started".to_string());
            };
            if let Err(error) = tokio::io::AsyncWriteExt::write_all(stdin, line.as_bytes()).await {
                self.inner
                    .pending_requests
                    .lock()
                    .expect("pending requests poisoned")
                    .remove(&id);
                return Err(error.to_string());
            }
        }

        match tokio::time::timeout(Duration::from_millis(timeout_ms), receiver).await {
            Ok(Ok(response)) => Ok(response),
            // A settled-by-stop receiver leaves the caller pending, matching the
            // TypeScript promise that is never rejected.
            Ok(Err(_)) => std::future::pending::<Result<RpcResponse, String>>().await,
            Err(_) => {
                self.inner
                    .pending_requests
                    .lock()
                    .expect("pending requests poisoned")
                    .remove(&id);
                Err(format!(
                    "Timeout waiting for response to {}. Stderr: {}",
                    command.type_name(),
                    self.get_stderr()
                ))
            }
        }
    }

    fn get_data<T: DeserializeOwned>(&self, response: RpcResponse) -> Result<T, String> {
        if !response.success {
            return Err(response.error.unwrap_or_default());
        }
        let data = response.data.unwrap_or(Value::Null);
        serde_json::from_value(data).map_err(|error| error.to_string())
    }
}

/// `RefineOptions` for `refine()`.
#[derive(Debug, Clone, Default)]
pub struct RefineOptions {
    pub instructions: Option<String>,
    pub rollback_id: Option<String>,
    pub global: Option<bool>,
}

/// `{ ...command, id }` from `send()`.
fn with_id(command: &RpcCommand, id: &str) -> Value {
    let mut value = serde_json::to_value(command).unwrap_or(Value::Null);
    if let Some(object) = value.as_object_mut() {
        object.insert("id".to_string(), Value::String(id.to_string()));
    }
    value
}

/// `handleLine(line)`.
fn handle_line(inner: &RpcClientInner, line: &str) {
    let Ok(data) = serde_json::from_str::<Value>(line) else {
        // Ignore non-JSON lines.
        return;
    };

    // Check whether it is a response to a pending request.
    if data.get("type").and_then(Value::as_str) == Some("response") {
        if let Some(id) = data.get("id").and_then(Value::as_str) {
            if !id.is_empty() {
                let pending = inner
                    .pending_requests
                    .lock()
                    .expect("pending requests poisoned")
                    .remove(id);
                if let Some(pending) = pending {
                    if let Ok(response) = serde_json::from_value::<RpcResponse>(data.clone()) {
                        let _ = pending.send(response);
                        return;
                    }
                }
            }
        }
    }

    let type_name = data.get("type").and_then(Value::as_str);
    if type_name == Some("observed_session_event") || type_name == Some("observed_session_closed") {
        let listeners: Vec<RpcObservedSessionListener> = inner
            .observed_session_listeners
            .lock()
            .expect("observed session listeners poisoned")
            .clone();
        if let Ok(event) = serde_json::from_value::<RpcObservedSessionEvent>(data) {
            for listener in listeners {
                // Listener failures must not block other RPC subscribers.
                listener(event.clone());
            }
        }
        return;
    }

    let listeners: Vec<RpcEventListener> = inner
        .event_listeners
        .lock()
        .expect("event listeners poisoned")
        .clone();
    for listener in listeners {
        // Listener failures must not block other RPC subscribers.
        listener(&data);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_are_sent_with_a_generated_id() {
        let command = RpcCommand::GetState { id: None };
        let value = with_id(&command, "req_1");
        assert_eq!(value, serde_json::json!({"type": "get_state", "id": "req_1"}));
    }

    #[test]
    fn get_data_throws_the_error_message() {
        let client = RpcClient::new(RpcClientOptions::default());
        let response = RpcResponse::error(None, "prompt", "boom");
        assert_eq!(client.get_data::<Value>(response).unwrap_err(), "boom");
    }

    #[test]
    fn get_data_deserializes_success_payloads() {
        let client = RpcClient::new(RpcClientOptions::default());
        let response = RpcResponse::success(None, "get_last_assistant_text", Some(serde_json::json!({"text": "hi"})));
        let payload: RpcTextPayload = client.get_data(response).unwrap();
        assert_eq!(payload.text.as_deref(), Some("hi"));
    }

    #[test]
    fn lines_resolve_pending_requests_before_events() {
        let inner = RpcClientInner {
            stderr: Mutex::new(String::new()),
            event_listeners: Mutex::new(Vec::new()),
            observed_session_listeners: Mutex::new(Vec::new()),
            pending_requests: Mutex::new(HashMap::new()),
            request_id: AtomicU64::new(0),
            stdin: AsyncMutex::new(None),
        };
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = seen.clone();
        inner
            .event_listeners
            .lock()
            .unwrap()
            .push(Arc::new(move |event: &Value| {
                sink.lock().unwrap().push(event.clone());
            }));

        let (sender, mut receiver) = oneshot::channel();
        inner.pending_requests.lock().unwrap().insert("req_1".to_string(), sender);
        handle_line(&inner, "{\"type\":\"response\",\"id\":\"req_1\",\"success\":true}");
        assert!(receiver.try_recv().is_ok());
        assert!(seen.lock().unwrap().is_empty());

        handle_line(&inner, "{\"type\":\"agent_end\",\"messages\":[]}");
        handle_line(&inner, "not json");
        let events = seen.lock().unwrap().clone();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["type"], Value::String("agent_end".to_string()));
    }

    #[test]
    fn observed_session_events_reach_their_own_listeners() {
        let inner = RpcClientInner {
            stderr: Mutex::new(String::new()),
            event_listeners: Mutex::new(Vec::new()),
            observed_session_listeners: Mutex::new(Vec::new()),
            pending_requests: Mutex::new(HashMap::new()),
            request_id: AtomicU64::new(0),
            stdin: AsyncMutex::new(None),
        };
        let observed = Arc::new(Mutex::new(Vec::new()));
        let sink = observed.clone();
        inner
            .observed_session_listeners
            .lock()
            .unwrap()
            .push(Arc::new(move |event: RpcObservedSessionEvent| {
                sink.lock().unwrap().push(event);
            }));
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = events.clone();
        inner
            .event_listeners
            .lock()
            .unwrap()
            .push(Arc::new(move |event: &Value| {
                sink.lock().unwrap().push(event.clone());
            }));

        handle_line(
            &inner,
            "{\"type\":\"observed_session_closed\",\"activeSessionId\":\"s1\"}",
        );
        assert_eq!(observed.lock().unwrap().len(), 1);
        assert!(events.lock().unwrap().is_empty());
    }

    #[test]
    fn model_info_keeps_the_wire_names() {
        let info = ModelInfo {
            provider: "anthropic".to_string(),
            id: "claude".to_string(),
            context_window: 200000.0,
            reasoning: true,
        };
        assert_eq!(
            serde_json::to_value(&info).unwrap(),
            serde_json::json!({"provider": "anthropic", "id": "claude", "contextWindow": 200000, "reasoning": true})
        );
    }

    #[test]
    fn the_refine_timeout_is_ten_minutes() {
        assert_eq!(REFINE_REQUEST_TIMEOUT_MS, 600_000);
    }
}
