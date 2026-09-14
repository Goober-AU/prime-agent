//! Recent-first history paging on the native terminal owner thread.
use super::*;

pub(super) struct HistoryRuntime {
    connection: Arc<dyn wire::AgentConnection>,
    loaded: Option<LoadedAgentConnectionHistory>,
    generation: u64,
    task: Option<tokio::task::JoinHandle<()>>,
    send: mpsc::Sender<(u64, Result<wire::AgentConnectionHistoryRange, String>)>,
    receive: mpsc::Receiver<(u64, Result<wire::AgentConnectionHistoryRange, String>)>,
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
            generation: 0,
            task: None,
            send,
            receive,
        }
    }

    pub(super) fn reset(
        &mut self,
        history: Option<wire::AgentConnectionHistoryWindow>,
        messages: Vec<AgentMessage>,
        transcript: &Rc<RefCell<Transcript>>,
        editor: &Rc<RefCell<CustomEditor>>,
    ) -> Result<(), String> {
        self.generation = self.generation.wrapping_add(1);
        if let Some(task) = self.task.take() {
            task.abort();
        }
        self.loaded = None;
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
        if let Some(history) = history {
            validate(&history, messages.len())?;
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
            transcript.borrow_mut().replace(messages);
        }
        Ok(())
    }

    pub(super) fn request(&mut self, mode: &InteractiveMode) {
        let Some(loaded) = &self.loaded else {
            return;
        };
        if self.task.is_some()
            || !loaded.window.has_older
            || mode
                .connection_state
                .as_ref()
                .is_some_and(|s| s.is_streaming || s.is_compacting || s.is_bash_running)
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

fn validate(history: &wire::AgentConnectionHistoryWindow, messages: usize) -> Result<(), String> {
    if history.version != 1.0
        || history.order != "chronological"
        || history.representation.is_empty()
        || history.entry_ids.len() != messages
        || !history.start_index.is_finite()
        || history.start_index < 0.0
        || !history.total_message_count.is_finite()
        || history.start_index + messages as f64 > history.total_message_count
    {
        return Err("Received an invalid recent-first session history window".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
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
}
