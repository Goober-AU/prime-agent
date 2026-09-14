//! Native host wiring for TypeScript's serialized queued-message editing flow.

use super::{wire, CustomEditor, InteractiveMode, QueueSelectionItem};
use serde_json::{json, Value};
use std::{
    cell::RefCell,
    collections::VecDeque,
    rc::Rc,
    sync::{mpsc, Arc},
};

enum Mutation {
    Move(i64),
    Edit(String, bool),
}
struct Reply {
    generation: u64,
    revision: u64,
    session: String,
    selected: QueueSelectionItem,
    mutation: Mutation,
    editor_before: String,
    result: Result<(String, wire::AgentConnectionQueueState), String>,
}

pub(super) struct QueueRuntime {
    generation: u64,
    revision: u64,
    mode: Rc<RefCell<InteractiveMode>>,
    editor: Rc<RefCell<CustomEditor>>,
    connection: Arc<dyn wire::AgentConnection>,
    send: mpsc::Sender<Reply>,
    receive: mpsc::Receiver<Reply>,
    session: String,
    pending: bool,
    queued: VecDeque<Mutation>,
}

impl QueueRuntime {
    pub(super) fn new(
        mode: Rc<RefCell<InteractiveMode>>,
        editor: Rc<RefCell<CustomEditor>>,
        connection: Arc<dyn wire::AgentConnection>,
    ) -> Self {
        let header = mode.clone();
        editor.borrow_mut().get_header_line = Some(Box::new(move || {
            header.borrow().get_queue_selection_header()
        }));
        let (send, receive) = mpsc::channel();
        let session = mode
            .borrow()
            .connection_state
            .as_ref()
            .map(|s| s.session_id.clone())
            .unwrap_or_default();
        Self {
            generation: 0,
            revision: 0,
            mode,
            editor,
            connection,
            send,
            receive,
            session,
            pending: false,
            queued: VecDeque::new(),
        }
    }

    pub(super) fn observe_queue_change(&mut self) {
        self.revision = self.revision.wrapping_add(1);
    }

    pub(super) fn reset_session(&mut self, session: String) {
        self.generation = self.generation.wrapping_add(1);
        self.session = session;
        self.pending = false;
        self.queued.clear();
        self.mode.borrow_mut().queue_selection.reset();
    }

    pub(super) fn handle_input(&mut self, data: &str) -> bool {
        let keys = pi_tui::keybindings::get_keybindings();
        for (key, direction) in [
            ("app.message.navigateOlder", -1),
            ("app.message.navigateNewer", 1),
        ] {
            if keys.matches(data, key) {
                if !self.pending {
                    let text = self.editor.borrow().editor().get_text();
                    let next = self
                        .mode
                        .borrow_mut()
                        .browse_queue_selection(&text, direction);
                    if let Some(next) = next {
                        self.editor.borrow_mut().editor_mut().set_text(&next);
                    }
                }
                return true;
            }
        }
        if !self.mode.borrow().queue_selection.is_browsing() {
            return false;
        }
        for (key, direction) in [
            ("app.message.moveEarlier", -1),
            ("app.message.moveLater", 1),
        ] {
            if keys.matches(data, key) {
                self.queued.push_back(Mutation::Move(direction));
                self.start_next();
                return true;
            }
        }
        if self.pending
            && (keys.matches(data, "tui.input.submit")
                || keys.matches(data, "app.message.followUp"))
        {
            return true;
        }
        if keys.matches(data, "app.input.clear") && !self.mode.borrow().has_interruptible_work() {
            let draft = self.mode.borrow_mut().queue_selection.reset();
            self.editor.borrow_mut().editor_mut().set_text(&draft);
            return true;
        }
        false
    }

    pub(super) fn submit(&mut self, text: &str, follow_up: bool) -> bool {
        if !self.mode.borrow().queue_selection.is_browsing() {
            return false;
        }
        if !self.pending {
            self.queued
                .push_back(Mutation::Edit(text.to_string(), follow_up));
            self.editor.borrow_mut().editor_mut().set_text("");
            self.start_next();
        }
        true
    }

    fn start_next(&mut self) {
        if self.pending {
            return;
        }
        let Some(mutation) = self.queued.pop_front() else {
            return;
        };
        let Some(selected) = self.mode.borrow().queue_selection.selected().cloned() else {
            self.queued.clear();
            return;
        };
        self.pending = true;
        let generation = self.generation;
        let revision = self.revision;
        let editor_before = self.editor.borrow().editor().get_text();
        let session = self.session.clone();
        let connection = self.connection.clone();
        let send = self.send.clone();
        tokio::spawn(async move {
            let result = async {
                if connection.get_state().await?.session_id != session {
                    return Err("Session changed".into());
                }
                let payload = mutation_payload(&mutation);
                let status = connection
                    .mutate_queued_message(
                        selected.lane.as_str(),
                        selected.index as i64,
                        &selected.text,
                        payload,
                    )
                    .await?;
                let queue = connection.get_queue().await?;
                Ok((status, queue))
            }
            .await;
            let _ = send.send(Reply {
                generation,
                revision,
                session,
                selected,
                mutation,
                editor_before,
                result,
            });
        });
    }

    pub(super) fn poll(&mut self) {
        let session = self
            .mode
            .borrow()
            .connection_state
            .as_ref()
            .map(|s| s.session_id.clone())
            .unwrap_or_default();
        if session != self.session {
            self.reset_session(session);
        }
        while let Ok(reply) = self.receive.try_recv() {
            if reply.generation != self.generation || reply.session != self.session {
                continue;
            }
            self.pending = false;
            let untouched = self.editor.borrow().editor().get_text() == reply.editor_before;
            let mut mode = self.mode.borrow_mut();
            let status = match reply.result {
                Ok((status, queue)) => {
                    if reply.revision == self.revision {
                        mode.patch_connection_state(|state| {
                            state.session_actions.queued_count =
                                queue.steering.len() + queue.follow_up.len();
                            state.session_actions.steering = queue.steering;
                            state.session_actions.follow_ups = queue.follow_up;
                        });
                    }
                    status
                }
                Err(error) => {
                    mode.show_error(&error);
                    "error".into()
                }
            };
            match reply.mutation {
                Mutation::Move(direction) => {
                    let queue = mode.get_connection_queue();
                    let index = if status == "applied" {
                        reply
                            .selected
                            .index
                            .saturating_add_signed(direction as isize)
                    } else {
                        reply.selected.index
                    };
                    let dropped = mode.queue_selection.refresh_at(
                        &queue,
                        reply.selected.lane,
                        index,
                        &reply.selected.text,
                    );
                    if untouched {
                        if let Some(draft) = dropped {
                            self.editor.borrow_mut().editor_mut().set_text(&draft);
                        }
                    }
                    if status != "applied" {
                        mode.show_status(
                            if status == "unsupported" {
                                "Queue editing requires a newer daemon"
                            } else {
                                "Queue changed; reorder not applied"
                            },
                            "dim",
                        );
                    }
                }
                Mutation::Edit(text, _) => {
                    if status == "applied" {
                        if !text.trim().is_empty() {
                            self.editor
                                .borrow_mut()
                                .editor_mut()
                                .add_to_history(text.trim());
                        }
                        let draft = mode.queue_selection.reset();
                        if untouched {
                            self.editor.borrow_mut().editor_mut().set_text(&draft);
                        }
                    } else {
                        if untouched {
                            self.editor.borrow_mut().editor_mut().set_text(&text);
                        }
                        mode.show_status(match status.as_str() {
                            "invalid" => "Edited command is not a valid session command; edit kept in the editor",
                            "unsupported" => "Queue editing requires a newer daemon",
                            _ => "Queue changed; edit kept in the editor",
                        }, "dim");
                    }
                }
            }
        }
        if !self.pending {
            let mut mode = self.mode.borrow_mut();
            if let Some(selected) = mode.queue_selection.selected().cloned() {
                let queue = mode.get_connection_queue();
                let draft = mode.queue_selection.refresh_at(
                    &queue,
                    selected.lane,
                    selected.index,
                    &selected.text,
                );
                if let Some(draft) = draft {
                    if self.editor.borrow().editor().get_text() == selected.text {
                        self.editor.borrow_mut().editor_mut().set_text(&draft);
                    }
                }
            }
        }
        self.start_next();
    }
}

fn mutation_payload(mutation: &Mutation) -> Value {
    match mutation {
        Mutation::Move(direction) => json!({"type": "move", "direction": direction}),
        Mutation::Edit(text, follow_up) if !text.trim().is_empty() => {
            let mut value = json!({"type": "replace", "text": text.trim(), "lane": if *follow_up { "followUp" } else { "steering" }});
            // Missing images preserves attachments that this client cannot resolve.
            if crate::modes::interactive::image_markers::image_marker_ids(text).is_empty() {
                value["images"] = json!([]);
            }
            value
        }
        Mutation::Edit(_, _) => json!({"type": "delete"}),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn queue_edits_keep_the_typescript_mutation_contract() {
        assert_eq!(
            mutation_payload(&Mutation::Edit(" \n ".into(), false)),
            json!({"type":"delete"})
        );
        assert_eq!(
            mutation_payload(&Mutation::Edit(" updated ".into(), true)),
            json!({"type":"replace","text":"updated","lane":"followUp","images":[]})
        );
        assert!(
            mutation_payload(&Mutation::Edit("[image #1] inspect".into(), false))
                .get("images")
                .is_none()
        );
        assert_eq!(
            mutation_payload(&Mutation::Move(-1)),
            json!({"type":"move","direction":-1})
        );
    }
}
