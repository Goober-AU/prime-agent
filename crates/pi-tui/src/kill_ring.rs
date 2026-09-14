//! Port of packages/tui/src/kill-ring.ts.

/// Ring buffer for Emacs-style kill/yank operations.
#[derive(Debug, Default, Clone)]
pub struct KillRing {
    ring: Vec<String>,
}

impl KillRing {
    pub fn new() -> Self {
        Self { ring: Vec::new() }
    }

    /// Add text to the kill ring.
    ///
    /// `prepend`: if accumulating, prepend (backward deletion) or append (forward deletion).
    /// `accumulate`: merge with the most recent entry instead of creating a new one.
    pub fn push(&mut self, text: &str, prepend: bool, accumulate: bool) {
        if text.is_empty() {
            return;
        }

        if accumulate && !self.ring.is_empty() {
            let last = self.ring.pop().unwrap();
            self.ring.push(if prepend {
                format!("{text}{last}")
            } else {
                format!("{last}{text}")
            });
        } else {
            self.ring.push(text.to_string());
        }
    }

    pub fn peek(&self) -> Option<String> {
        self.ring.last().cloned()
    }

    pub fn rotate(&mut self) {
        if self.ring.len() > 1 {
            let last = self.ring.pop().unwrap();
            self.ring.insert(0, last);
        }
    }

    pub fn len(&self) -> usize {
        self.ring.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ring.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_accumulate_prepends_or_appends() {
        let mut ring = KillRing::new();
        ring.push("b", false, false);
        ring.push("a", true, true);
        assert_eq!(ring.peek().as_deref(), Some("ab"));
        ring.push("c", false, true);
        assert_eq!(ring.peek().as_deref(), Some("abc"));
        assert_eq!(ring.len(), 1);
    }

    #[test]
    fn rotate_cycles_entries() {
        let mut ring = KillRing::new();
        ring.push("one", false, false);
        ring.push("two", false, false);
        ring.rotate();
        assert_eq!(ring.peek().as_deref(), Some("one"));
    }

    #[test]
    fn empty_text_is_ignored() {
        let mut ring = KillRing::new();
        ring.push("", false, false);
        assert_eq!(ring.len(), 0);
        assert_eq!(ring.peek(), None);
    }
}
