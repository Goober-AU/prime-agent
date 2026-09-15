//! Background state refreshes must not hold up session events or repainting.
use super::*;
use pi_agent_core::types::AgentEvent;

struct RefreshResult {
    generation: u64,
    revision: u64,
    session_id: String,
    result: Result<wire::AgentConnectionState, String>,
}

pub(super) struct StateRefresh {
    generation: u64,
    revision: u64,
    send: mpsc::Sender<RefreshResult>,
    receive: mpsc::Receiver<RefreshResult>,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl StateRefresh {
    pub(super) fn new() -> Self {
        let (send, receive) = mpsc::channel();
        Self {
            generation: 0,
            revision: 0,
            send,
            receive,
            task: None,
        }
    }

    pub(super) fn invalidate(&mut self) {
        self.revision = self.revision.wrapping_add(1);
    }

    pub(super) fn request(
        &mut self,
        connection: Arc<dyn wire::AgentConnection>,
        session_id: String,
    ) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;
        let revision = self.revision;
        let send = self.send.clone();
        self.task = Some(tokio::spawn(async move {
            let result = tokio::time::timeout(Duration::from_secs(30), connection.get_state())
                .await
                .unwrap_or_else(|_| Err("Session state refresh timed out".into()));
            let _ = send.send(RefreshResult {
                generation,
                revision,
                session_id,
                result,
            });
        }));
    }

    pub(super) fn poll(&mut self, mode: &Rc<RefCell<InteractiveMode>>, session_id: &str) -> bool {
        let mut changed = false;
        while let Ok(reply) = self.receive.try_recv() {
            if reply.generation != self.generation
                || reply.revision != self.revision
                || reply.session_id != session_id
            {
                continue;
            }
            match reply.result {
                Ok(state) if state.session_id == session_id => {
                    apply_refresh(&mut mode.borrow_mut(), state);
                    changed = true;
                }
                Ok(_) => {}
                Err(error) => {
                    mode.borrow_mut().show_error(&error);
                    changed = true;
                }
            }
        }
        changed
    }
}

impl Drop for StateRefresh {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

/// Like the TypeScript's post-turn stats refresh, metadata must not overwrite
/// completed lifecycle flags with a still-settling run. A fresh idle result can
/// clear activity; it cannot restart it without a start event. Full attachment
/// and resync snapshots still replace the entire state in the owner loop.
pub(super) fn apply_refresh(mode: &mut InteractiveMode, state: wire::AgentConnectionState) {
    let mut state = project_state(state);
    if let Some(current) = mode
        .connection_state
        .as_ref()
        .filter(|s| s.session_id == state.session_id)
    {
        state.is_streaming &= current.is_streaming;
        state.is_compacting &= current.is_compacting;
        state.is_bash_running &= current.is_bash_running;
        state.retry_attempt = current.retry_attempt;
    }
    mode.apply_connection_state_snapshot(state);
    mode.sync_working_loader();
}

/// Adapt both the local AgentEvent and daemon message variants to the existing
/// activity reducer (interactive-mode.ts:5501). Message end is not agent end:
/// tools, retries, or another assistant turn can still follow it.
pub(super) fn track_activity(
    mode: &mut InteractiveMode,
    event: &wire::AgentConnectionSessionEvent,
) {
    use local::AgentConnectionSessionEvent as Local;
    use wire::AgentConnectionSessionEvent as Wire;
    let event = match event {
        Wire::Agent(AgentEvent::AgentStart) => Local::AgentStart,
        Wire::Agent(AgentEvent::MessageStart { message }) | Wire::MessageStart { message } => {
            if message.role() == "user" {
                mode.context_usage_token_baseline = 0.0;
            }
            Local::MessageStart {
                message: message.clone(),
            }
        }
        Wire::Agent(AgentEvent::MessageUpdate {
            message,
            assistant_message_event,
        })
        | Wire::MessageUpdate {
            message,
            assistant_message_event,
        } => Local::MessageUpdate {
            message: message.clone(),
            assistant_message_event: assistant_message_event.clone(),
        },
        Wire::Agent(AgentEvent::MessageEnd { message }) | Wire::MessageEnd { message } => {
            Local::MessageEnd {
                message: message.clone(),
            }
        }
        Wire::Agent(AgentEvent::ToolExecutionStart {
            tool_call_id,
            tool_name,
            args,
        })
        | Wire::ToolExecutionStart {
            tool_call_id,
            tool_name,
            args,
        } => Local::ToolExecutionStart {
            tool_call_id: tool_call_id.clone(),
            tool_name: tool_name.clone(),
            args: args.clone(),
        },
        Wire::Agent(AgentEvent::ToolExecutionEnd {
            tool_call_id,
            result,
            is_error,
            ..
        }) => {
            let Ok(result) = serde_json::to_value(result) else {
                return;
            };
            Local::ToolExecutionEnd {
                tool_call_id: tool_call_id.clone(),
                result,
                is_error: *is_error,
            }
        }
        Wire::ToolExecutionEnd {
            tool_call_id,
            result,
            is_error,
            ..
        } => Local::ToolExecutionEnd {
            tool_call_id: tool_call_id.clone(),
            result: result.clone(),
            is_error: *is_error,
        },
        _ => return,
    };
    mode.activity_tracker.handle_event(&event);
}
