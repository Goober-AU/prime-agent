//! Port of packages/coding-agent/src/modes/interactive/queue-selection.ts

use super::interactive_mode_services::AgentConnectionQueueState;

/// `QueueLane`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueueLane {
    Steering,
    FollowUp,
}

impl QueueLane {
    pub fn as_str(self) -> &'static str {
        match self {
            QueueLane::Steering => "steering",
            QueueLane::FollowUp => "followUp",
        }
    }
}

/// `QueueSelectionItem`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueueSelectionItem {
    pub lane: QueueLane,
    pub index: usize,
    pub text: String,
}

/// Tracks which queued message the user is browsing/editing with alt+up/alt+down.
///
/// Items are addressed by (lane, index, text): the text is the authoritative
/// check when a mutation is applied, so no ids or revisions are needed. Browsing
/// order is newest-first: draft -> last followUp -> ... -> first steering.
#[derive(Debug, Default)]
pub struct QueueSelection {
    items: Vec<QueueSelectionItem>,
    /// `-1` = draft. Kept as an `i64` so the boundary arithmetic matches the TypeScript.
    cursor: i64,
    draft: String,
    has_stashed_draft: bool,
}

impl QueueSelection {
    pub fn new() -> Self {
        Self { items: Vec::new(), cursor: -1, draft: String::new(), has_stashed_draft: false }
    }

    /// `selected`
    pub fn selected(&self) -> Option<&QueueSelectionItem> {
        if self.cursor >= 0 {
            self.items.get(self.cursor as usize)
        } else {
            None
        }
    }

    /// `isBrowsing`
    pub fn is_browsing(&self) -> bool {
        self.cursor >= 0
    }

    /// `hasDraft`
    pub fn has_draft(&self) -> bool {
        self.has_stashed_draft
    }

    /// Port of `replaceDraft`.
    pub fn replace_draft(&mut self, draft: &str) {
        self.draft = draft.to_string();
        self.has_stashed_draft = true;
    }

    /// Move the cursor. -1 browses older, +1 newer. Returns the text to show, or
    /// `None` for a boundary noop.
    pub fn move_cursor(&mut self, queue: &AgentConnectionQueueState, draft: &str, direction: i64) -> Option<String> {
        if self.cursor < 0 {
            if direction > 0 {
                return None;
            }
            self.items = flatten(queue);
            if self.items.is_empty() {
                return None;
            }
            if !self.has_stashed_draft {
                self.draft = draft.to_string();
                self.has_stashed_draft = true;
            }
            self.cursor = self.items.len() as i64 - 1;
            return self.items.get(self.cursor as usize).map(|item| item.text.clone());
        }
        let next = self.cursor + direction;
        if next < 0 || next > self.items.len() as i64 {
            return None;
        }
        if next == self.items.len() as i64 {
            return Some(self.reset());
        }
        self.cursor = next;
        self.items.get(next as usize).map(|item| item.text.clone())
    }

    /// Port of `refreshAt`.
    pub fn refresh_at(
        &mut self,
        queue: &AgentConnectionQueueState,
        lane: QueueLane,
        index: usize,
        expected_text: &str,
    ) -> Option<String> {
        self.items = flatten(queue);
        let cursor = if lane == QueueLane::Steering { index } else { queue.steering.len() + index };
        let selected = self.items.get(cursor).cloned();
        match selected {
            Some(selected) if selected.lane == lane && selected.index == index && selected.text == expected_text => {
                self.cursor = cursor as i64;
                None
            }
            _ => Some(self.reset()),
        }
    }

    /// Called after a mutation or submit resolved the selection. Returns the stashed draft.
    pub fn reset(&mut self) -> String {
        self.cursor = -1;
        let draft = std::mem::take(&mut self.draft);
        self.has_stashed_draft = false;
        draft
    }
}

fn flatten(queue: &AgentConnectionQueueState) -> Vec<QueueSelectionItem> {
    let mut items: Vec<QueueSelectionItem> = Vec::new();
    for (index, text) in queue.steering.iter().enumerate() {
        items.push(QueueSelectionItem { lane: QueueLane::Steering, index, text: text.clone() });
    }
    for (index, text) in queue.follow_up.iter().enumerate() {
        items.push(QueueSelectionItem { lane: QueueLane::FollowUp, index, text: text.clone() });
    }
    items
}

#[cfg(test)]
mod tests {
    use super::*;

    fn queue(steering: &[&str], follow_up: &[&str]) -> AgentConnectionQueueState {
        AgentConnectionQueueState {
            steering: steering.iter().map(|value| (*value).to_string()).collect(),
            follow_up: follow_up.iter().map(|value| (*value).to_string()).collect(),
        }
    }

    #[test]
    fn browse_starts_at_newest_and_returns_draft_at_the_end() {
        let mut selection = QueueSelection::new();
        let queue = queue(&["s1"], &["f1", "f2"]);
        assert_eq!(selection.move_cursor(&queue, "draft", -1), Some("f2".to_string()));
        assert_eq!(selection.move_cursor(&queue, "draft", -1), Some("f1".to_string()));
        assert_eq!(selection.move_cursor(&queue, "draft", -1), Some("s1".to_string()));
        assert_eq!(selection.move_cursor(&queue, "draft", -1), None, "boundary noop");
        assert_eq!(selection.move_cursor(&queue, "draft", 1), Some("f1".to_string()));
        assert_eq!(selection.move_cursor(&queue, "draft", 1), Some("f2".to_string()));
        assert_eq!(selection.move_cursor(&queue, "draft", 1), Some("draft".to_string()));
        assert!(!selection.is_browsing());
    }

    #[test]
    fn newer_direction_from_draft_is_a_noop() {
        let mut selection = QueueSelection::new();
        assert_eq!(selection.move_cursor(&queue(&["s1"], &[]), "draft", 1), None);
        assert!(!selection.is_browsing());
    }

    #[test]
    fn empty_queue_never_starts_browsing() {
        let mut selection = QueueSelection::new();
        assert_eq!(selection.move_cursor(&queue(&[], &[]), "draft", -1), None);
        assert!(!selection.is_browsing());
        assert!(!selection.has_draft());
    }

    #[test]
    fn refresh_at_drops_a_stale_selection() {
        let mut selection = QueueSelection::new();
        let queue = queue(&["s1"], &["f1"]);
        assert_eq!(selection.move_cursor(&queue, "draft", -1), Some("f1".to_string()));
        assert_eq!(selection.refresh_at(&queue, QueueLane::FollowUp, 0, "f1"), None);
        assert!(selection.is_browsing());
        assert_eq!(selection.refresh_at(&queue, QueueLane::FollowUp, 0, "changed"), Some("draft".to_string()));
        assert!(!selection.is_browsing());
    }

    #[test]
    fn reset_returns_the_stashed_draft_once() {
        let mut selection = QueueSelection::new();
        let queue = queue(&[], &["f1"]);
        selection.move_cursor(&queue, "typed draft", -1);
        assert!(selection.has_draft());
        assert_eq!(selection.reset(), "typed draft");
        assert!(!selection.has_draft());
        assert_eq!(selection.reset(), "");
    }
}
