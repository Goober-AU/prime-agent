//! Port of packages/coding-agent/src/modes/interactive/prompt-stash-state.ts

use pi_ai::types::ImageContent;

/// Stand-in for `EditorPasteSnapshot` (pi-tui, other slice).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EditorPasteSnapshot {
    pub text: String,
}

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
}
