//! Recent-first history paging on the native terminal owner thread.
use super::*;

const INITIAL_DISPLAY_MESSAGES: usize = 40;
const INITIAL_DISPLAY_BYTES: usize = 8 * 1024;

pub(super) struct HistoryRuntime {
    connection: Arc<dyn wire::AgentConnection>,
    loaded: Option<LoadedAgentConnectionHistory>,
    full_history: Option<FullHistory>,
    full_history_requested: bool,
    session_id: Option<String>,
    generation: u64,
    task: Option<tokio::task::JoinHandle<()>>,
    send: mpsc::Sender<(u64, Result<wire::AgentConnectionHistoryRange, String>)>,
    receive: mpsc::Receiver<(u64, Result<wire::AgentConnectionHistoryRange, String>)>,
}

struct FullHistory {
    messages: Vec<AgentMessage>,
    start: usize,
}

fn window(window: wire::AgentConnectionHistoryWindow) -> local::AgentConnectionHistoryWindow {
    local::AgentConnectionHistoryWindow {
        version: window.version,
        generation: window.generation,
        representation: window.representation,
        tip_entry_id: window.tip_entry_id,
        total_message_count: window.total_message_count,
        start_index: window.start_index,
        entry_ids: window.entry_ids,
        has_older: window.has_older,
        order: window.order,
    }
}

impl HistoryRuntime {
    pub(super) fn new(connection: Arc<dyn wire::AgentConnection>) -> Self {
        let (send, receive) = mpsc::channel();
        Self {
            connection,
            loaded: None,
            full_history: None,
            full_history_requested: false,
            session_id: None,
            generation: 0,
            task: None,
            send,
            receive,
        }
    }

    /// Replaces the transcript with the attached session's messages.
    ///
    /// The recent-first window is optional metadata. TypeScript renders the plain
    /// transcript when `snapshot.history` is absent (interactive-mode.ts:6759-6767)
    /// and reports an unusable window through `showError` without failing the
    /// attachment (:5302-5304); the daemon likewise degrades to the full legacy
    /// snapshot instead of dropping or misidentifying it
    /// (modes/daemon/daemon-mode.ts:5507-5509). The unusable window therefore
    /// degrades to local paging of the full transcript here and its message is returned
    /// for the caller to report, instead of propagating out of the host and killing
    /// startup/resync (handoff defect 5).
    pub(super) fn reset(
        &mut self,
        history: Option<wire::AgentConnectionHistoryWindow>,
        messages: Vec<AgentMessage>,
        transcript: &Rc<RefCell<Transcript>>,
        editor: &Rc<RefCell<CustomEditor>>,
    ) -> Option<String> {
        self.generation = self.generation.wrapping_add(1);
        if let Some(task) = self.task.take() {
            task.abort();
        }
        let previous = self.loaded.take();
        let previous_full = self.full_history.take();
        self.full_history_requested = false;
        let session_id = transcript.borrow().mode.borrow().connection_state.as_ref()
            .map(|state| state.session_id.clone());
        let same_session = self.session_id == session_id;
        self.session_id = session_id;
        for message in &messages {
            if let AgentMessage::Message(pi_ai::types::Message::User(user)) = message {
                let text = transcript
                    .borrow()
                    .mode
                    .borrow()
                    .get_user_message_text(user);
                if !text.trim().is_empty() {
                    editor.borrow_mut().editor_mut().add_to_history(&text);
                }
            }
        }
        let mut warning = None;
        let history = match history {
            Some(history) => match validate(&history, messages.len()) {
                Ok(()) => Some(history),
                Err(error) => {
                    warning = Some(error);
                    None
                }
            },
            None => None,
        };
        if let Some(mut history) = history {
            let mut messages = messages;
            let display_count = previous.as_ref().filter(|_| same_session)
                .map(|loaded| loaded.messages.len());
            if let Some(previous) = previous.as_ref().filter(|_| same_session) {
                retain_loaded_prefix(&mut history, &mut messages, previous);
            }
            let start = display_count.map(|count| display_start(&messages, count))
                .unwrap_or_else(|| initial_display_start(&messages));
            history.start_index += start as f64;
            history.entry_ids.drain(..start);
            history.has_older = history.start_index > 0.0;
            messages.drain(..start);
            self.loaded = Some(LoadedAgentConnectionHistory {
                window: window(history),
                messages: messages.clone(),
            });
            transcript.borrow_mut().replace(Vec::new());
            transcript.borrow_mut().replace_history(
                messages,
                self.loaded.as_ref().unwrap().window.total_message_count,
            );
        } else {
            // Legacy/supervisor attachments do not supply wire history ranges.
            // Keep their complete snapshot locally, but apply the same small
            // first paint. Older rows remain available without a daemon request.
            let start = previous_full.as_ref()
                .filter(|previous| same_session && messages.starts_with(&previous.messages))
                .map(|previous| previous.start)
                .unwrap_or_else(|| initial_display_start(&messages));
            transcript.borrow_mut().replace(Vec::new());
            transcript.borrow_mut().replace_history(messages[start..].to_vec(), messages.len() as f64);
            self.full_history = Some(FullHistory { messages, start });
        }
        warning
    }

    pub(super) fn request(&mut self, _mode: &InteractiveMode) {
        // Local snapshots and remote ranges both retain an immutable history pin.
        // Live rows are separate; poll rejects a replaced generation before merging.
        if let Some(full) = &self.full_history {
            self.full_history_requested = full.start > 0;
            return;
        }
        let Some(loaded) = &self.loaded else {
            return;
        };
        if self.task.is_some()
            || !loaded.window.has_older
        {
            return;
        }
        let Some(boundary) = loaded.window.entry_ids.first() else {
            return;
        };
        let request = wire::AgentConnectionHistoryRangeRequest {
            generation: loaded.window.generation.clone(),
            representation: loaded.window.representation.clone(),
            tip_entry_id: loaded.window.tip_entry_id.clone(),
            before_entry_id: Some(boundary.clone()),
            limit: None,
        };
        let connection = self.connection.clone();
        let send = self.send.clone();
        let generation = self.generation;
        self.task = Some(tokio::spawn(async move {
            let _ = send.send((generation, connection.get_history_range(request).await));
        }));
    }

    pub(super) fn poll(
        &mut self,
        mode: &Rc<RefCell<InteractiveMode>>,
        transcript: &Rc<RefCell<Transcript>>,
        ui: &Rc<RefCell<TUI>>,
    ) {
        if std::mem::take(&mut self.full_history_requested) {
            if let Some(full) = &mut self.full_history {
                full.start = display_start(&full.messages, full.messages.len() - full.start + INITIAL_DISPLAY_MESSAGES);
                transcript.borrow_mut().replace_history(full.messages[full.start..].to_vec(), full.messages.len() as f64);
                ui.borrow_mut().request_render_preserving_viewport();
            }
        }
        while let Ok((generation, result)) = self.receive.try_recv() {
            if generation != self.generation {
                continue;
            }
            self.task = None;
            let Some(current) = &self.loaded else {
                continue;
            };
            let result = result.and_then(|range| {
                merge_older_agent_connection_history(
                    current,
                    &local::AgentConnectionHistoryRange {
                        window: window(range.window),
                        messages: range.messages,
                    },
                )
            });
            match result {
                Ok(merged) => {
                    transcript.borrow_mut().replace_history(
                        merged.messages.clone(),
                        merged.window.total_message_count,
                    );
                    self.loaded = Some(merged);
                    ui.borrow_mut().request_render_preserving_viewport();
                }
                Err(error) => mode.borrow_mut().show_status(
                    &format!("Could not load earlier history: {error}"),
                    "warning",
                ),
            }
        }
    }
}

impl Drop for HistoryRuntime {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

// A resync of the same pinned snapshot must not discard already-paged rows.
fn retain_loaded_prefix(
    history: &mut wire::AgentConnectionHistoryWindow,
    messages: &mut Vec<AgentMessage>,
    previous: &LoadedAgentConnectionHistory,
) {
    let old = &previous.window;
    if old.generation != history.generation
        || old.representation != history.representation
        || old.tip_entry_id != history.tip_entry_id
        || old.total_message_count != history.total_message_count
        || old.start_index >= history.start_index
    {
        return;
    }
    let prefix = (history.start_index - old.start_index) as usize;
    if previous.messages.len() != old.entry_ids.len()
        || old.entry_ids.get(prefix..) != Some(history.entry_ids.as_slice())
    {
        return;
    }
    let mut combined = previous.messages[..prefix].to_vec();
    combined.append(messages);
    *messages = combined;
    let mut ids = old.entry_ids[..prefix].to_vec();
    ids.append(&mut history.entry_ids);
    history.entry_ids = ids;
    history.start_index = old.start_index;
    history.has_older = old.has_older;
}

fn initial_display_start(messages: &[AgentMessage]) -> usize {
    // A count alone still renders megabytes from a few long messages before the
    // input loop starts. PageUp retains the full snapshot and uses normal pages.
    // Always show the last message; tool-call linkage may exceed this soft budget.
    let mut bytes = 0usize;
    let mut count = 0usize;
    for message in messages.iter().rev().take(INITIAL_DISPLAY_MESSAGES) {
        let size = serde_json::to_vec(message).map(|value| value.len()).unwrap_or(0);
        if count > 0 && bytes.saturating_add(size) > INITIAL_DISPLAY_BYTES {
            break;
        }
        bytes = bytes.saturating_add(size);
        count += 1;
    }
    display_start(messages, count)
}

fn display_start(messages: &[AgentMessage], count: usize) -> usize {
    let mut start = messages.len().saturating_sub(count);
    let mut calls = HashMap::new();
    for (index, message) in messages.iter().enumerate() {
        if let AgentMessage::Message(pi_ai::types::Message::Assistant(assistant)) = message {
            for block in &assistant.content {
                if let pi_ai::types::ContentBlock::ToolCall(call) = block {
                    calls.entry(call.id.as_str()).or_insert(index);
                }
            }
        }
    }
    // Walk backwards so newly included results also retain their call rows.
    for (index, message) in messages.iter().enumerate().rev() {
        if index < start { break; }
        if let AgentMessage::Message(pi_ai::types::Message::ToolResult(result)) = message {
            if let Some(call) = calls.get(result.tool_call_id.as_str()) {
                start = start.min(*call);
            }
        }
    }
    start
}

fn validate(history: &wire::AgentConnectionHistoryWindow, messages: usize) -> Result<(), String> {
    if history.version != 1.0
        || history.order != "chronological"
        || history.representation.is_empty()
        || history.entry_ids.len() != messages
        || !history.start_index.is_finite()
        || history.start_index < 0.0
        || history.start_index.fract() != 0.0
        || !history.total_message_count.is_finite()
        || history.start_index + messages as f64 != history.total_message_count
        || history.has_older != (history.start_index > 0.0)
        || history.entry_ids.iter().collect::<std::collections::HashSet<_>>().len() != messages
    {
        return Err("Received an invalid recent-first session history window".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_ai::types::{
        AssistantMessage, ContentBlock, Message as AiMessage, TextContent, UserContent, UserMessage,
    };

    /// A connection that a test never calls: `reset` is pure transcript work.
    fn unused_connection() -> Arc<dyn wire::AgentConnection> {
        let client =
            crate::modes::daemon::daemon_client::DaemonClient::create("unused-test-socket");
        Arc::new(
            crate::modes::agent_connection::daemon_agent_connection::DaemonAgentConnection::new(
                Arc::new(crate::main_entry::MainEntryDaemonTransport::new(client)),
                "active".to_string(),
                Default::default(),
            ),
        )
    }

    fn user_message(text: &str) -> AgentMessage {
        AgentMessage::Message(AiMessage::User(UserMessage::new(
            UserContent::Text(text.to_string()),
            0,
        )))
    }

    fn streaming_assistant_message(text: &str) -> AgentMessage {
        AgentMessage::Message(AiMessage::Assistant(AssistantMessage {
            content: vec![ContentBlock::Text(TextContent::new(text))],
            ..Default::default()
        }))
    }

    fn transcript_text(transcript: &Rc<RefCell<Transcript>>) -> String {
        transcript
            .borrow_mut()
            .render(80.0)
            .iter()
            .map(|line| pi_tui::utils::strip_ansi(line))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn fixture(
        session_id: &str,
    ) -> (
        Rc<RefCell<Transcript>>,
        Rc<RefCell<CustomEditor>>,
        HistoryRuntime,
    ) {
        let mode = Rc::new(RefCell::new(super::super::tests::stash_mode(session_id)));
        let transcript = Rc::new(RefCell::new(Transcript::new(mode)));
        let tui = Rc::new(RefCell::new(TUI::new(
            Box::new(pi_tui::terminal::ProcessTerminal::new()),
            None,
        )));
        let editor = Rc::new(RefCell::new(CustomEditor::new(
            tui,
            editor_theme(),
            CustomEditorOptions::default(),
        )));
        (transcript, editor, HistoryRuntime::new(unused_connection()))
    }

    fn valid_window(entry_ids: &[&str]) -> wire::AgentConnectionHistoryWindow {
        wire::AgentConnectionHistoryWindow {
            version: 1.0,
            generation: "generation-1".into(),
            representation: "model".into(),
            tip_entry_id: Some("tip".into()),
            total_message_count: entry_ids.len() as f64,
            start_index: 0.0,
            entry_ids: entry_ids.iter().map(|id| (*id).to_string()).collect(),
            has_older: false,
            order: "chronological".into(),
        }
    }

    fn large_snapshot(count: usize) -> (wire::AgentConnectionHistoryWindow, Vec<AgentMessage>) {
        let ids: Vec<String> = (0..count).map(|i| format!("entry-{i}")).collect();
        let history = valid_window(&ids.iter().map(String::as_str).collect::<Vec<_>>());
        let messages = (0..count).map(|i| user_message(&format!("MESSAGE_{i:03}"))).collect();
        (history, messages)
    }

    #[test]
    fn busy_local_backfill_preserves_live_rows_and_draft() {
        for activity in ["stream", "compact", "bash"] {
            let (transcript, editor, mut runtime) = fixture(activity);
            let mode = transcript.borrow().mode.clone();
            mode.borrow_mut().apply_connection_state_snapshot(local::AgentConnectionState {
                session_id: activity.into(), is_streaming: activity == "stream",
                is_compacting: activity == "compact", is_bash_running: activity == "bash",
                ..Default::default()
            });
            let ui = Rc::new(RefCell::new(TUI::new(Box::new(pi_tui::terminal::ProcessTerminal::new()), None)));
            let (_, messages) = large_snapshot(100);
            runtime.reset(None, messages, &transcript, &editor);
            editor.borrow_mut().editor_mut().set_text("keep draft");
            transcript.borrow_mut().message(user_message("LIVE_TURN"), false);
            runtime.request(&mode.borrow());
            runtime.poll(&mode, &transcript, &ui);
            assert_eq!(runtime.full_history.as_ref().unwrap().start, 20, "{activity}");
            let text = transcript_text(&transcript);
            assert_eq!(text.matches("LIVE_TURN").count(), 1);
            assert!(text.contains("MESSAGE_020"));
            assert_eq!(editor.borrow().editor().get_text(), "keep draft");
        }
    }

    #[tokio::test]
    async fn busy_remote_backfill_ignores_old_generation_and_preserves_live_rows() {
        let (transcript, editor, mut runtime) = fixture("busy-remote");
        let mode = transcript.borrow().mode.clone();
        mode.borrow_mut().apply_connection_state_snapshot(local::AgentConnectionState {
            session_id: "busy-remote".into(), is_streaming: true, is_compacting: true,
            ..Default::default()
        });
        let ui = Rc::new(RefCell::new(TUI::new(Box::new(pi_tui::terminal::ProcessTerminal::new()), None)));
        let (history, messages) = large_snapshot(100);
        runtime.reset(Some(history.clone()), messages.clone(), &transcript, &editor);
        transcript.borrow_mut().message(user_message("LIVE_TURN"), false);
        runtime.request(&mode.borrow());
        let request = runtime.task.take().expect("busy state must still schedule the pinned remote history request");
        // Current-thread runtime: abort before yielding so the unused fixture
        // transport never connects. Response merging is supplied deterministically.
        request.abort();
        assert!(request.await.unwrap_err().is_cancelled());
        let range = wire::AgentConnectionHistoryRange {
            window: wire::AgentConnectionHistoryWindow {
                entry_ids: history.entry_ids[..60].to_vec(), ..history
            }, messages: messages[..60].to_vec(),
        };
        runtime.send.send((runtime.generation - 1, Ok(range.clone()))).unwrap();
        runtime.poll(&mode, &transcript, &ui);
        assert_eq!(runtime.loaded.as_ref().unwrap().messages.len(), 40);
        runtime.send.send((runtime.generation, Ok(range))).unwrap();
        runtime.poll(&mode, &transcript, &ui);
        assert_eq!(runtime.loaded.as_ref().unwrap().messages.len(), 100);
        assert_eq!(transcript_text(&transcript).matches("LIVE_TURN").count(), 1);
    }

    #[test]
    fn oversized_first_paint_is_bounded_and_pageup_retains_every_message() {
        let (transcript, editor, mut runtime) = fixture("large-payload-first");
        let (_, mut messages) = large_snapshot(6);
        messages[4] = user_message(&format!("LARGE_{}", "x".repeat(INITIAL_DISPLAY_BYTES)));
        runtime.reset(None, messages.clone(), &transcript, &editor);
        assert_eq!(runtime.full_history.as_ref().unwrap().messages, messages);
        assert_eq!(runtime.full_history.as_ref().unwrap().start, 5);
        assert!(!transcript_text(&transcript).contains("LARGE_"));
        assert!(transcript_text(&transcript).contains("MESSAGE_005"));
        // Same-session refresh must retain the budgeted slice, not expand to 40.
        runtime.reset(None, messages.clone(), &transcript, &editor);
        assert_eq!(runtime.full_history.as_ref().unwrap().start, 5);
        let mode = transcript.borrow().mode.clone();
        let ui = Rc::new(RefCell::new(TUI::new(Box::new(pi_tui::terminal::ProcessTerminal::new()), None)));
        runtime.request(&mode.borrow());
        runtime.poll(&mode, &transcript, &ui);
        assert_eq!(runtime.full_history.as_ref().unwrap().start, 0);
        let rendered = transcript_text(&transcript);
        assert_eq!(rendered.matches("LARGE_").count(), 1);
        assert_eq!(rendered.matches("MESSAGE_005").count(), 1);
        assert!(runtime.task.is_none());
        runtime.reset(None, messages, &transcript, &editor);
        assert_eq!(runtime.full_history.as_ref().unwrap().start, 0);
    }

    #[test]
    fn remote_oversized_first_paint_keeps_boundary_and_resync_slice() {
        let (transcript, editor, mut runtime) = fixture("large-payload-remote");
        let (history, mut messages) = large_snapshot(6);
        messages[4] = user_message(&"x".repeat(INITIAL_DISPLAY_BYTES));
        runtime.reset(Some(history.clone()), messages.clone(), &transcript, &editor);
        let loaded = runtime.loaded.as_ref().unwrap();
        assert_eq!(loaded.messages, messages[5..]);
        assert_eq!(loaded.window.entry_ids, history.entry_ids[5..]);
        assert_eq!(loaded.window.start_index, 5.0);
        assert!(loaded.window.has_older);
        runtime.reset(Some(history), messages.clone(), &transcript, &editor);
        assert_eq!(runtime.loaded.as_ref().unwrap().messages, messages[5..]);
        assert_eq!(initial_display_start(&[user_message(&"x".repeat(INITIAL_DISPLAY_BYTES * 2))]), 0,
            "the latest message is never hidden even when it exceeds the soft budget");
    }

    #[test]
    fn full_snapshot_first_paint_pages_locally_without_losing_history_or_stream() {
        let (transcript, editor, mut runtime) = fixture("full-first");
        let (_, messages) = large_snapshot(100);
        assert_eq!(apply_history_snapshot(None, messages.clone(),
            Some(streaming_assistant_message("LIVE_REPLY")), &transcript, &editor, &mut runtime), None);
        assert_eq!(runtime.full_history.as_ref().unwrap().messages, messages);
        assert_eq!(runtime.full_history.as_ref().unwrap().start, 60);
        assert!(runtime.loaded.is_none());
        let recent = transcript_text(&transcript);
        assert!(!recent.contains("MESSAGE_000"));
        assert!(recent.contains("MESSAGE_099"));
        assert_eq!(recent.matches("LIVE_REPLY").count(), 1);
        transcript.borrow_mut().message(user_message("AFTER_ATTACH_TURN"), false);
        let mode = transcript.borrow().mode.clone();
        let ui = Rc::new(RefCell::new(TUI::new(Box::new(pi_tui::terminal::ProcessTerminal::new()), None)));
        for expected in [20, 0, 0] {
            runtime.request(&mode.borrow());
            runtime.poll(&mode, &transcript, &ui);
            assert_eq!(runtime.full_history.as_ref().unwrap().start, expected);
            assert!(runtime.task.is_none(), "full snapshots must not request a remote range");
        }
        let complete = transcript_text(&transcript);
        for i in 0..100 { assert_eq!(complete.matches(&format!("MESSAGE_{i:03}")).count(), 1); }
        assert_eq!(complete.matches("LIVE_REPLY").count(), 1);
        assert_eq!(complete.matches("AFTER_ATTACH_TURN").count(), 1);
    }

    #[test]
    fn full_snapshot_resync_keeps_loaded_prefix_but_never_mixes_replaced_history() {
        let (transcript, editor, mut runtime) = fixture("full-resync");
        let mode = transcript.borrow().mode.clone();
        mode.borrow_mut().apply_connection_state_snapshot(local::AgentConnectionState {
            session_id: "same".into(), ..Default::default()
        });
        let ui = Rc::new(RefCell::new(TUI::new(Box::new(pi_tui::terminal::ProcessTerminal::new()), None)));
        let (_, mut messages) = large_snapshot(100);
        runtime.reset(None, messages.clone(), &transcript, &editor);
        runtime.request(&mode.borrow());
        runtime.poll(&mode, &transcript, &ui);
        assert_eq!(runtime.full_history.as_ref().unwrap().start, 20);
        messages.push(user_message("NEW_TURN"));
        runtime.reset(None, messages, &transcript, &editor);
        assert_eq!(runtime.full_history.as_ref().unwrap().start, 20);
        assert!(transcript_text(&transcript).contains("MESSAGE_020"));
        runtime.request(&mode.borrow());
        runtime.reset(None, vec![user_message("COMPACTED_HISTORY")], &transcript, &editor);
        runtime.poll(&mode, &transcript, &ui);
        assert_eq!(runtime.full_history.as_ref().unwrap().start, 0);
        let compacted = transcript_text(&transcript);
        assert!(compacted.contains("COMPACTED_HISTORY"));
        assert!(!compacted.contains("MESSAGE_020"));
        assert!(!compacted.contains("NEW_TURN"));
        let (_, messages) = large_snapshot(100);
        runtime.reset(None, messages.clone(), &transcript, &editor);
        runtime.request(&mode.borrow());
        runtime.poll(&mode, &transcript, &ui);
        mode.borrow_mut().apply_connection_state_snapshot(local::AgentConnectionState {
            session_id: "different".into(), ..Default::default()
        });
        runtime.reset(None, messages, &transcript, &editor);
        assert_eq!(runtime.full_history.as_ref().unwrap().start, 60);
    }

    #[test]
    fn first_paint_uses_recent_40_and_backfill_restores_every_message_once() {
        let (transcript, editor, mut runtime) = fixture("recent-first");
        let (history, messages) = large_snapshot(100);
        assert_eq!(apply_history_snapshot(Some(history.clone()), messages.clone(),
            Some(streaming_assistant_message("LIVE_REPLY")), &transcript, &editor, &mut runtime), None);
        let loaded = runtime.loaded.as_ref().unwrap();
        assert_eq!(loaded.messages, messages[60..]);
        assert_eq!(loaded.window.start_index, 60.0);
        assert_eq!(loaded.window.entry_ids, history.entry_ids[60..]);
        assert!(loaded.window.has_older);
        let recent = transcript_text(&transcript);
        assert!(!recent.contains("MESSAGE_000"));
        assert!(recent.contains("MESSAGE_099"));
        assert_eq!(recent.matches("LIVE_REPLY").count(), 1);

        let mode = transcript.borrow().mode.clone();
        let ui = Rc::new(RefCell::new(TUI::new(Box::new(pi_tui::terminal::ProcessTerminal::new()), None)));
        let range = wire::AgentConnectionHistoryRange {
            window: wire::AgentConnectionHistoryWindow {
                entry_ids: history.entry_ids[..60].to_vec(), ..history.clone()
            },
            messages: messages[..60].to_vec(),
        };
        runtime.send.send((runtime.generation, Ok(range))).unwrap();
        runtime.poll(&mode, &transcript, &ui);
        let loaded = runtime.loaded.as_ref().unwrap();
        assert_eq!(loaded.messages, messages);
        assert_eq!(loaded.window.entry_ids, history.entry_ids);
        assert!(!loaded.window.has_older);
        let complete = transcript_text(&transcript);
        assert_eq!(complete.matches("MESSAGE_").count(), 100);
        assert_eq!(complete.matches("LIVE_REPLY").count(), 1);
    }

    #[test]
    fn legacy_or_malformed_metadata_keeps_every_message_available_through_local_paging() {
        let (transcript, editor, mut runtime) = fixture("recent-first-legacy");
        let (mut history, messages) = large_snapshot(100);
        history.total_message_count += 1.0;
        let mode = transcript.borrow().mode.clone();
        let ui = Rc::new(RefCell::new(TUI::new(Box::new(pi_tui::terminal::ProcessTerminal::new()), None)));
        for metadata in [None, Some(history)] {
            let malformed = metadata.is_some();
            assert_eq!(runtime.reset(metadata, messages.clone(), &transcript, &editor).is_some(), malformed);
            assert!(runtime.loaded.is_none());
            assert_eq!(runtime.full_history.as_ref().unwrap().messages, messages);
            for _ in 0..3 {
                runtime.request(&mode.borrow());
                runtime.poll(&mode, &transcript, &ui);
            }
            let complete = transcript_text(&transcript);
            for i in 0..100 { assert_eq!(complete.matches(&format!("MESSAGE_{i:03}")).count(), 1); }
        }
    }

    #[test]
    fn first_paint_retains_tool_calls_for_results_crossing_the_display_boundary() {
        use pi_ai::types::{ToolCall, ToolResultMessage};
        let (_, mut messages) = large_snapshot(100);
        let call = |id: &str| AgentMessage::Message(AiMessage::Assistant(AssistantMessage {
            content: vec![ContentBlock::ToolCall(ToolCall::new(id, "bash", Default::default()))],
            ..Default::default()
        }));
        let result = |id: &str| AgentMessage::Message(AiMessage::ToolResult(
            ToolResultMessage::new(id, "bash", Vec::new(), false, 0),
        ));
        messages[55] = call("earlier-call");
        messages[58] = call("boundary-call");
        messages[59] = result("earlier-call");
        messages[60] = result("boundary-call");
        assert_eq!(display_start(&messages, 40), 55);
        let oversized_linked = vec![
            user_message("earlier"),
            call("large-call"),
            user_message(&"x".repeat(INITIAL_DISPLAY_BYTES)),
            result("large-call"),
        ];
        assert_eq!(initial_display_start(&oversized_linked), 1,
            "a visible result keeps its call even across an oversized intervening message");
        let (transcript, editor, mut runtime) = fixture("recent-first-tools");
        let (history, _) = large_snapshot(100);
        assert_eq!(runtime.reset(Some(history), messages.clone(), &transcript, &editor), None);
        let loaded = runtime.loaded.as_ref().unwrap();
        assert_eq!(loaded.window.start_index, 55.0);
        assert_eq!(loaded.messages, messages[55..]);
        let history = transcript.borrow();
        assert_eq!(history.history.as_ref().unwrap().tools.len(), 2);
    }

    #[test]
    fn first_paint_refresh_retains_loaded_scrollback_and_never_reuses_another_pin() {
        let (transcript, editor, mut runtime) = fixture("recent-first-refresh");
        let (history, messages) = large_snapshot(100);
        assert_eq!(runtime.reset(Some(history.clone()), messages.clone(), &transcript, &editor), None);
        // Represent a user who has paged back to entry 20.
        runtime.loaded = Some(LoadedAgentConnectionHistory {
            window: window(wire::AgentConnectionHistoryWindow {
                start_index: 20.0, has_older: true, entry_ids: history.entry_ids[20..].to_vec(),
                ..history.clone()
            }), messages: messages[20..].to_vec(),
        });
        let shorter = wire::AgentConnectionHistoryWindow {
            start_index: 60.0, has_older: true, entry_ids: history.entry_ids[60..].to_vec(),
            ..history.clone()
        };
        assert_eq!(runtime.reset(Some(shorter.clone()), messages[60..].to_vec(), &transcript, &editor), None);
        assert_eq!(runtime.loaded.as_ref().unwrap().messages, messages[20..]);
        assert_eq!(runtime.reset(Some(history), messages.clone(), &transcript, &editor), None);
        assert_eq!(runtime.loaded.as_ref().unwrap().messages, messages[20..]);
        let another_pin = wire::AgentConnectionHistoryWindow {
            generation: "different-generation".into(), ..shorter
        };
        assert_eq!(runtime.reset(Some(another_pin), messages[60..].to_vec(), &transcript, &editor), None);
        assert_eq!(runtime.loaded.as_ref().unwrap().messages, messages[60..]);
    }

    /// DEFECT 5: an unusable optional history window degrades to the full-transcript
    /// render instead of aborting the attachment (interactive-mode.ts:6759-6767
    /// renders the plain transcript when `snapshot.history` is unusable).
    #[test]
    fn malformed_optional_history_keeps_a_usable_transcript() {
        let (transcript, editor, mut runtime) = fixture("history-malformed");
        let messages = vec![user_message("KEEP_ME_VISIBLE")];
        // Entry ids disagree with the messages, which is exactly what
        // `validate` rejects.
        let mut history = valid_window(&["entry-1"]);
        history.entry_ids = vec!["entry-1".into(), "entry-2".into()];

        let error = apply_history_snapshot(
            Some(history),
            messages,
            None,
            &transcript,
            &editor,
            &mut runtime,
        );

        assert_eq!(
            error.as_deref(),
            Some("Received an invalid recent-first session history window"),
            "the unusable window must be reported, not propagated as a fatal error"
        );
        let rendered = transcript_text(&transcript);
        assert!(
            rendered.contains("KEEP_ME_VISIBLE"),
            "the transcript must stay usable after malformed optional history: {rendered:?}"
        );
        assert!(
            runtime.loaded.is_none(),
            "an unusable window must not become the pinned paging state"
        );
    }

    /// DEFECT 1: the in-flight response survives the initial attach, with
    /// recent-first metadata (interactive-mode.ts:6865-6871 renders the transcript
    /// first and restores the streaming message afterwards).
    #[test]
    fn streaming_response_survives_the_initial_attach_with_history_metadata() {
        let (transcript, editor, mut runtime) = fixture("history-streaming-window");
        let messages = vec![user_message("EARLIER_TURN")];
        let error = apply_history_snapshot(
            Some(valid_window(&["entry-1"])),
            messages,
            Some(streaming_assistant_message("IN_FLIGHT_REPLY")),
            &transcript,
            &editor,
            &mut runtime,
        );
        assert_eq!(error, None, "the window is valid");
        let rendered = transcript_text(&transcript);
        assert!(
            rendered.contains("IN_FLIGHT_REPLY"),
            "the paged history reset must not discard the streaming row: {rendered:?}"
        );
    }

    /// DEFECT 1 without recent-first metadata: the same ordering holds on the
    /// legacy full-snapshot attach path (interactive-mode.ts:6759-6766).
    #[test]
    fn streaming_response_survives_the_initial_attach_without_history_metadata() {
        let (transcript, editor, mut runtime) = fixture("history-streaming-plain");
        let messages = vec![user_message("EARLIER_TURN")];
        let error = apply_history_snapshot(
            None,
            messages,
            Some(streaming_assistant_message("IN_FLIGHT_REPLY")),
            &transcript,
            &editor,
            &mut runtime,
        );
        assert_eq!(error, None, "no window means nothing to validate");
        let rendered = transcript_text(&transcript);
        assert!(
            rendered.contains("IN_FLIGHT_REPLY"),
            "the full-transcript reset must not discard the streaming row: {rendered:?}"
        );
        assert!(
            rendered.contains("EARLIER_TURN"),
            "the reset must still render the snapshot messages: {rendered:?}"
        );
    }

    #[test]
    fn snapshot_validation_rejects_unknown_order_and_misaligned_ids() {
        let mut history = wire::AgentConnectionHistoryWindow {
            version: 1.0,
            representation: "model".into(),
            order: "chronological".into(),
            total_message_count: 2.0,
            entry_ids: vec!["a".into(), "b".into()],
            ..Default::default()
        };
        assert!(validate(&history, 2).is_ok());
        assert!(validate(&history, 1).is_err());
        history.order = "newest-first".into();
        assert!(validate(&history, 2).is_err());
        history.order = "chronological".into();
        history.start_index = f64::NAN;
        assert!(validate(&history, 2).is_err());
    }

    #[test]
    fn streaming_history_refresh_keeps_slash_draft_and_middle_cursor() {
        crate::core::keybindings::KeybindingsManager::new(Default::default(), None).install();
        let (transcript, editor, mut runtime) = fixture("same-session-refresh");
        editor.borrow_mut().editor_mut().set_text("/telegram pairing");
        editor.borrow_mut().handle_input("\x1b[D");
        editor.borrow_mut().handle_input("\x1b[D");
        let before = editor.borrow().editor().get_cursor();
        for _ in 0..10 {
            assert_eq!(apply_history_snapshot(None, vec![user_message("previous prompt")],
                Some(streaming_assistant_message("stream update")), &transcript, &editor, &mut runtime), None);
            assert_eq!(editor.borrow().editor().get_cursor(), before);
            assert_eq!(editor.borrow().editor().get_text(), "/telegram pairing");
        }
        assert!(transcript_text(&transcript).contains("stream update"));
    }
}
