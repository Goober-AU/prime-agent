//! Port of packages/tui/src/undo-stack.ts.

/// Generic undo stack with clone-on-push semantics.
///
/// The TypeScript default `clone` is `structuredClone`; Rust state is `Clone`,
/// which is the equivalent structural copy.
#[derive(Debug, Clone)]
pub struct UndoStack<S> {
    stack: Vec<S>,
}

impl<S: Clone> Default for UndoStack<S> {
    fn default() -> Self {
        Self::new()
    }
}

impl<S: Clone> UndoStack<S> {
    pub fn new() -> Self {
        Self { stack: Vec::new() }
    }

    pub fn push(&mut self, state: &S) {
        self.stack.push(state.clone());
    }

    pub fn pop(&mut self) -> Option<S> {
        self.stack.pop()
    }

    pub fn clear(&mut self) {
        self.stack.clear();
    }

    pub fn len(&self) -> usize {
        self.stack.len()
    }

    pub fn is_empty(&self) -> bool {
        self.stack.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_clones_state() {
        let mut stack: UndoStack<Vec<i32>> = UndoStack::new();
        let mut state = vec![1, 2];
        stack.push(&state);
        state.push(3);
        assert_eq!(stack.pop(), Some(vec![1, 2]));
        assert_eq!(stack.len(), 0);
    }

    #[test]
    fn clear_empties_stack() {
        let mut stack: UndoStack<i32> = UndoStack::new();
        stack.push(&1);
        stack.push(&2);
        stack.clear();
        assert!(stack.is_empty());
        assert_eq!(stack.pop(), None);
    }
}
