//! Port of packages/coding-agent/src/modes/interactive/prompt-stash-state.ts

use pi_ai::types::ImageContent;
use std::sync::{Arc, Mutex, OnceLock};

/// The pi-tui snapshot (`packages/tui/src/editor-component.ts:4-7`, Rust
/// `crates/pi-tui/src/editor_component.rs:10-13`). The stash keeps the real
/// snapshot so a restore can rebuild the editor's paste table.
pub use pi_tui::editor_component::EditorPasteSnapshot;

/// `PromptStash`
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PromptStash {
    pub text: String,
    pub expanded_text: Option<String>,
    pub paste_snapshot: Option<EditorPasteSnapshot>,
    /// `images?: readonly (readonly [number, ImageContent])[]` - insertion ordered.
    pub images: Option<Vec<(i64, ImageContent)>>,
    pub restore_on_open: Option<bool>,
}

/// `PromptStashState`
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PromptStashState {
    /// `undefined` and "absent" are indistinguishable in the TypeScript object;
    /// the field is `None` for both, which is why this stays a plain `Option`.
    pub stash: Option<PromptStash>,
    pub queued_stashes: Option<Vec<PromptStash>>,
}

/// Port of `ClientPromptStashStore`.
///
/// `forSession` returns a stable key; callers resolve the state through
/// [`ClientPromptStashStore::state`]. The TypeScript identity check
/// (`this.states.get(sessionId) === state`) is preserved with a generation stamp
/// because Rust cannot hand out stable references into a map.
#[derive(Debug, Default)]
pub struct ClientPromptStashStore {
    states: Vec<(String, PromptStashState, u64)>,
    next_generation: u64,
}

/// A handle to one session's stash state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PromptStashHandle {
    pub generation: u64,
}

impl ClientPromptStashStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Port of `forSession`.
    pub fn for_session(&mut self, session_id: &str) -> PromptStashHandle {
        if let Some((_, _, generation)) = self.states.iter().find(|(id, _, _)| id == session_id) {
            return PromptStashHandle { generation: *generation };
        }
        self.next_generation += 1;
        let generation = self.next_generation;
        self.states.push((session_id.to_string(), PromptStashState::default(), generation));
        PromptStashHandle { generation }
    }

    /// Reads the state for a handle created by [`Self::for_session`].
    pub fn state(&self, handle: PromptStashHandle) -> Option<&PromptStashState> {
        self.states
            .iter()
            .find(|(_, _, generation)| *generation == handle.generation)
            .map(|(_, state, _)| state)
    }

    /// Mutable variant of [`Self::state`].
    pub fn state_mut(&mut self, handle: PromptStashHandle) -> Option<&mut PromptStashState> {
        self.states
            .iter_mut()
            .find(|(_, _, generation)| *generation == handle.generation)
            .map(|(_, state, _)| state)
    }

    /// Port of `release`.
    pub fn release(&mut self, session_id: &str, handle: PromptStashHandle) {
        let Some(index) = self.states.iter().position(|(_, _, generation)| *generation == handle.generation) else {
            return;
        };
        let (_, state, _) = &self.states[index];
        let is_empty = state.stash.is_none() && state.queued_stashes.as_ref().map(|q| q.len()).unwrap_or(0) == 0;
        if is_empty && self.states[index].0 == session_id {
            self.states.remove(index);
        }
    }
}

/// The one stash store for this TUI process.
///
/// `main.ts:1448` creates a single `ClientPromptStashStore` and hands it to every
/// chat view, so a stash survives leaving to the agents view and reopening a
/// session. The port keeps the same lifetime with a process-wide store; the
/// `InteractiveModeOptions` seam is outside this slice.
pub fn shared_prompt_stash_store() -> Arc<Mutex<ClientPromptStashStore>> {
    static STORE: OnceLock<Arc<Mutex<ClientPromptStashStore>>> = OnceLock::new();
    STORE
        .get_or_init(|| Arc::new(Mutex::new(ClientPromptStashStore::new())))
        .clone()
}

/// The editor inputs of `snapshotPromptStashFrom` (interactive-mode.ts:4343-4352).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PromptStashCapture {
    /// `editor.getText()` - kept collapsed (`[paste #1 +12 lines]` stays a marker).
    pub text: String,
    /// `editor.getExpandedText()` - the fallback when a paste snapshot cannot be restored.
    pub expanded_text: String,
    /// `editor.getPasteSnapshot?.()`.
    pub paste_snapshot: Option<EditorPasteSnapshot>,
    /// `getPromptStashImages(text)` - insertion ordered marker/image pairs.
    pub images: Vec<(i64, ImageContent)>,
}

impl PromptStashCapture {
    /// Port of `snapshotPromptStashFrom` (interactive-mode.ts:4343-4352).
    pub fn into_stash(self) -> PromptStash {
        PromptStash {
            expanded_text: if self.paste_snapshot.is_some() {
                Some(self.expanded_text)
            } else {
                None
            },
            text: self.text,
            paste_snapshot: self.paste_snapshot,
            images: if self.images.is_empty() {
                None
            } else {
                Some(self.images)
            },
            restore_on_open: None,
        }
    }
}

/// Port of `getPromptStashImages` (interactive-mode.ts:4492-4502): the images whose
/// `[image #N]` marker appears in `text`, in marker appearance order.
pub fn stash_images(
    images: &std::collections::HashMap<i64, ImageContent>,
    text: &str,
) -> Vec<(i64, ImageContent)> {
    let mut collected = Vec::new();
    for marker_id in super::image_markers::image_marker_ids(text) {
        if let Some(image) = images.get(&(marker_id as i64)) {
            collected.push((marker_id as i64, image.clone()));
        }
    }
    collected
}

/// The editor change that completes a stash action. The host owns the editor, so
/// the state layer returns the change instead of performing it.
#[derive(Debug, Clone, PartialEq)]
pub enum PromptStashEditorEffect {
    None,
    Clear,
    SetText {
        text: String,
        paste_snapshot: Option<EditorPasteSnapshot>,
    },
}

/// Result of one stash action: the editor change plus the status notice, if any.
#[derive(Debug, Clone, PartialEq)]
pub struct PromptStashOutcome {
    pub editor: PromptStashEditorEffect,
    pub status: Option<&'static str>,
}

impl PromptStashOutcome {
    fn with_status(status: &'static str) -> Self {
        Self {
            editor: PromptStashEditorEffect::None,
            status: Some(status),
        }
    }
}

/// One session's stash state inside a shared store.
///
/// TypeScript keeps a live reference (`this.promptStashState`) that several views
/// mutate; the port resolves the state through the store on every call so a
/// mutation is visible to the next view that opens the same session.
pub struct PromptStashSession {
    store: Arc<Mutex<ClientPromptStashStore>>,
    handle: PromptStashHandle,
}

impl PromptStashSession {
    pub fn open(store: Arc<Mutex<ClientPromptStashStore>>, session_id: &str) -> Self {
        let handle = store
            .lock()
            .expect("prompt stash store poisoned")
            .for_session(session_id);
        Self { store, handle }
    }

    fn with_state<T>(&self, body: impl FnOnce(&mut PromptStashState) -> T) -> Option<T> {
        let mut store = self.store.lock().expect("prompt stash store poisoned");
        store.state_mut(self.handle).map(body)
    }

    /// Reads a clone of the session state (tests and diagnostics).
    pub fn state(&self) -> PromptStashState {
        self.store
            .lock()
            .expect("prompt stash store poisoned")
            .state(self.handle)
            .cloned()
            .unwrap_or_default()
    }

    /// Port of `handlePromptStash` (interactive-mode.ts:4379-4394).
    pub fn handle_prompt_stash(
        &self,
        capture: &PromptStashCapture,
        can_restore_paste_snapshot: bool,
    ) -> PromptStashOutcome {
        let text = capture.text.clone();
        if text.trim().is_empty() {
            return self
                .restore_prompt_stash_if_editor_empty(None, &text, can_restore_paste_snapshot)
                .unwrap_or_else(|| PromptStashOutcome::with_status("No prompt to stash"));
        }
        let existing = self
            .with_state(|state| state.stash.is_some())
            .unwrap_or(false);
        if existing {
            return PromptStashOutcome::with_status("Prompt stash already has a draft");
        }
        let stash = capture.clone().into_stash();
        let stored = self
            .with_state(|state| {
                if state.stash.is_some() {
                    return false;
                }
                state.stash = Some(stash);
                true
            })
            .unwrap_or(false);
        if !stored {
            return PromptStashOutcome::with_status("Prompt stash already has a draft");
        }
        PromptStashOutcome {
            editor: PromptStashEditorEffect::Clear,
            status: Some("Stashed prompt"),
        }
    }

    /// Port of `restorePromptStashIfEditorEmpty` (interactive-mode.ts:4396-4414).
    ///
    /// `captured` is the stash a caller captured earlier (TypeScript passes it for
    /// the deferred submit restore). The identity guard `this.promptStash !== stash`
    /// becomes a value comparison: the port stores owned stashes, so the only
    /// observable difference is between two stashes with identical content.
    pub fn restore_prompt_stash_if_editor_empty(
        &self,
        captured: Option<&PromptStash>,
        editor_text: &str,
        can_restore_paste_snapshot: bool,
    ) -> Option<PromptStashOutcome> {
        if !editor_text.trim().is_empty() {
            return None;
        }
        self.with_state(|state| {
            let live = state.stash.clone()?;
            if let Some(captured) = captured {
                if &live != captured {
                    return None;
                }
            }
            state.stash = match state.queued_stashes.as_mut() {
                Some(queue) if !queue.is_empty() => Some(queue.remove(0)),
                _ => None,
            };
            if state
                .queued_stashes
                .as_ref()
                .map(|queue| queue.is_empty())
                .unwrap_or(false)
            {
                state.queued_stashes = None;
            }
            let text = if live.paste_snapshot.is_none() || can_restore_paste_snapshot {
                live.text.clone()
            } else {
                live.expanded_text.clone().unwrap_or_else(|| live.text.clone())
            };
            let paste_snapshot = if can_restore_paste_snapshot {
                live.paste_snapshot.clone()
            } else {
                None
            };
            Some(PromptStashOutcome {
                editor: PromptStashEditorEffect::SetText {
                    text,
                    paste_snapshot,
                },
                status: Some("Restored stashed prompt"),
            })
        })
        .flatten()
    }

    /// Port of `stashDraftForAgentsView` (interactive-mode.ts:4367-4377).
    ///
    /// The auto-stash goes to the head of the durable queue and keeps
    /// `restoreOnOpen`, so an existing manual stash stays queued behind it.
    /// Returns whether a draft was stashed; whitespace-only drafts are skipped.
    pub fn stash_draft_for_agents_view(&self, capture: &PromptStashCapture) -> bool {
        if capture.text.trim().is_empty() {
            return false;
        }
        let mut stash = capture.clone().into_stash();
        stash.restore_on_open = Some(true);
        self.with_state(|state| {
            let mut existing = Vec::new();
            existing.extend(state.stash.take());
            if let Some(mut queued) = state.queued_stashes.take() {
                existing.append(&mut queued);
            }
            state.stash = Some(stash);
            state.queued_stashes = if existing.is_empty() {
                None
            } else {
                Some(existing)
            };
        })
        .is_some()
    }

    /// The `restorePromptStashOnOpen` guard (interactive-mode.ts:4358-4365).
    pub fn restore_on_open_pending(&self) -> bool {
        self.with_state(|state| {
            state
                .stash
                .as_ref()
                .and_then(|stash| stash.restore_on_open)
                .unwrap_or(false)
        })
        .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn for_session_is_stable_and_release_drops_empty_state() {
        let mut store = ClientPromptStashStore::new();
        let first = store.for_session("s1");
        let second = store.for_session("s1");
        assert_eq!(first, second);

        store.release("s1", first);
        assert!(store.state(first).is_none(), "empty state is released");

        let third = store.for_session("s1");
        assert!(store.state(third).is_some());
    }

    #[test]
    fn release_keeps_non_empty_state() {
        let mut store = ClientPromptStashStore::new();
        let handle = store.for_session("s1");
        store.state_mut(handle).expect("state").stash = Some(PromptStash { text: "draft".into(), ..Default::default() });
        store.release("s1", handle);
        assert!(store.state(handle).is_some());
    }

    #[test]
    fn release_with_wrong_session_keeps_state() {
        let mut store = ClientPromptStashStore::new();
        let handle = store.for_session("s1");
        store.release("other", handle);
        assert!(store.state(handle).is_some());
    }

    fn image(data: &str) -> ImageContent {
        ImageContent::new(data, "image/png")
    }

    fn capture(text: &str) -> PromptStashCapture {
        PromptStashCapture {
            text: text.to_string(),
            expanded_text: text.to_string(),
            paste_snapshot: None,
            images: Vec::new(),
        }
    }

    fn session() -> (Arc<Mutex<ClientPromptStashStore>>, PromptStashSession) {
        let store = Arc::new(Mutex::new(ClientPromptStashStore::new()));
        let session = PromptStashSession::open(store.clone(), "session-a");
        (store, session)
    }

    fn editor_text(outcome: &PromptStashOutcome) -> Option<&str> {
        match &outcome.editor {
            PromptStashEditorEffect::SetText { text, .. } => Some(text.as_str()),
            PromptStashEditorEffect::Clear => Some(""),
            PromptStashEditorEffect::None => None,
        }
    }

    #[test]
    fn ctrl_s_stashes_and_restores_the_draft() {
        let (_store, session) = session();
        let stashed = session.handle_prompt_stash(&capture("half-written draft"), true);
        assert_eq!(stashed.editor, PromptStashEditorEffect::Clear);
        assert_eq!(stashed.status, Some("Stashed prompt"));

        // Whitespace-only editor text counts as empty, so Ctrl+S restores.
        let restored = session.handle_prompt_stash(&capture("   "), true);
        assert_eq!(restored.status, Some("Restored stashed prompt"));
        assert_eq!(editor_text(&restored), Some("half-written draft"));

        // The stash is consumed: a third Ctrl+S has nothing left to restore.
        let exhausted = session.handle_prompt_stash(&capture(""), true);
        assert_eq!(exhausted.status, Some("No prompt to stash"));
    }

    #[test]
    fn empty_editor_without_a_stash_reports_no_prompt_to_stash() {
        let (_store, session) = session();
        let outcome = session.handle_prompt_stash(&capture(""), true);
        assert_eq!(outcome.status, Some("No prompt to stash"));
        assert_eq!(outcome.editor, PromptStashEditorEffect::None);
        assert!(session
            .restore_prompt_stash_if_editor_empty(None, "", true)
            .is_none());
    }

    #[test]
    fn an_existing_stash_is_never_overwritten() {
        let (_store, session) = session();
        session.handle_prompt_stash(&capture("first draft"), true);
        let outcome = session.handle_prompt_stash(&capture("second draft"), true);
        assert_eq!(outcome.status, Some("Prompt stash already has a draft"));
        assert_eq!(outcome.editor, PromptStashEditorEffect::None);
        assert_eq!(session.state().stash.map(|stash| stash.text), Some("first draft".to_string()));
    }

    #[test]
    fn whitespace_only_drafts_are_not_stashed_for_the_agents_view() {
        let (_store, session) = session();
        assert!(!session.stash_draft_for_agents_view(&capture("   \n\t")));
        assert!(session.state().stash.is_none());
    }

    #[test]
    fn agents_view_auto_stash_goes_to_the_head_and_keeps_the_manual_stash() {
        let (_store, session) = session();
        session.handle_prompt_stash(&capture("manual draft"), true);
        assert!(session.stash_draft_for_agents_view(&capture("typed draft")));

        let queued = session.state();
        assert_eq!(
            queued
                .stash
                .as_ref()
                .map(|stash| (stash.text.as_str(), stash.restore_on_open)),
            Some(("typed draft", Some(true)))
        );
        assert_eq!(
            queued
                .queued_stashes
                .as_ref()
                .map(|queue| queue.iter().map(|stash| stash.text.clone()).collect::<Vec<_>>()),
            Some(vec!["manual draft".to_string()])
        );

        // The auto-stash restores first; the manual stash then moves to the head and
        // the queue is dropped once empty.
        let first = session
            .restore_prompt_stash_if_editor_empty(None, "", true)
            .expect("auto-stash restores");
        assert_eq!(editor_text(&first), Some("typed draft"));
        let manual = session.state();
        assert_eq!(manual.stash.as_ref().map(|stash| stash.text.as_str()), Some("manual draft"));
        assert!(manual.queued_stashes.is_none());

        let second = session
            .restore_prompt_stash_if_editor_empty(None, "", true)
            .expect("manual stash restores");
        assert_eq!(editor_text(&second), Some("manual draft"));
        let drained = session.state();
        assert!(drained.stash.is_none());
        assert!(drained.queued_stashes.is_none());
    }

    #[test]
    fn only_a_restore_on_open_stash_auto_restores() {
        let (_store, manual) = session();
        manual.handle_prompt_stash(&capture("manual draft"), true);
        assert!(!manual.restore_on_open_pending());

        let (_other_store, auto) = session();
        auto.stash_draft_for_agents_view(&capture("typed draft"));
        assert!(auto.restore_on_open_pending());
    }

    #[test]
    fn a_restore_keeps_the_paste_snapshot_and_the_collapsed_marker_text() {
        let (_store, session) = session();
        let snapshot = EditorPasteSnapshot {
            pastes: vec![(1, "line one\nline two".to_string())],
            paste_counter: 1,
        };
        let mut captured = capture("draft [paste #1 +12 lines]");
        captured.expanded_text = "draft line one\nline two".to_string();
        captured.paste_snapshot = Some(snapshot.clone());
        session.handle_prompt_stash(&captured, true);

        let restored = session
            .restore_prompt_stash_if_editor_empty(None, "", true)
            .expect("restore");
        match &restored.editor {
            PromptStashEditorEffect::SetText {
                text,
                paste_snapshot,
            } => {
                assert_eq!(text, "draft [paste #1 +12 lines]");
                assert_eq!(paste_snapshot.as_ref(), Some(&snapshot));
            }
            other => panic!("unexpected editor effect: {other:?}"),
        }
    }

    #[test]
    fn a_restore_falls_back_to_the_expanded_text_without_paste_support() {
        let (_store, session) = session();
        let mut captured = capture("draft [paste #1 +12 lines]");
        captured.expanded_text = "draft line one\nline two".to_string();
        captured.paste_snapshot = Some(EditorPasteSnapshot {
            pastes: vec![(1, "line one\nline two".to_string())],
            paste_counter: 1,
        });
        session.handle_prompt_stash(&captured, true);

        let restored = session
            .restore_prompt_stash_if_editor_empty(None, "", false)
            .expect("restore");
        assert_eq!(editor_text(&restored), Some("draft line one\nline two"));
        match restored.editor {
            PromptStashEditorEffect::SetText { paste_snapshot, .. } => assert!(paste_snapshot.is_none()),
            other => panic!("unexpected editor effect: {other:?}"),
        }
    }

    #[test]
    fn an_older_captured_stash_is_not_restored_over_a_newer_one() {
        let (_store, session) = session();
        session.handle_prompt_stash(&capture("older draft"), true);
        let older = session.state().stash.expect("older stash");
        session.handle_prompt_stash(&capture(""), true); // drain the older stash
        session.handle_prompt_stash(&capture("newer draft"), true);

        let restored = session.restore_prompt_stash_if_editor_empty(Some(&older), "", true);
        assert!(restored.is_none(), "the stale capture must not restore");
        assert_eq!(
            session.state().stash.map(|stash| stash.text),
            Some("newer draft".to_string())
        );
    }

    #[test]
    fn a_non_empty_editor_is_never_overwritten() {
        let (_store, session) = session();
        session.handle_prompt_stash(&capture("draft"), true);
        assert!(session
            .restore_prompt_stash_if_editor_empty(None, "newer typing", true)
            .is_none());
        assert_eq!(session.state().stash.map(|stash| stash.text), Some("draft".to_string()));
    }

    #[test]
    fn a_stash_keeps_the_images_of_its_markers() {
        let (_store, session) = session();
        let mut images = std::collections::HashMap::new();
        images.insert(7, image("aW1hZ2U="));
        let mut captured = capture("draft [image #7]");
        captured.images = stash_images(&images, &captured.text);
        session.handle_prompt_stash(&captured, true);

        let stored = session.state().stash.expect("stash");
        assert_eq!(
            stored.images.as_ref().map(|entries| entries.iter().map(|(id, _)| *id).collect::<Vec<_>>()),
            Some(vec![7])
        );
        assert!(stash_images(&std::collections::HashMap::new(), "no markers").is_empty());
    }
}
