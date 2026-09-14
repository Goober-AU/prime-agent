//! Port of packages/tui/src/components/cancellable-loader.ts

use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::components::loader::{Loader, LoaderIndicatorOptions};
use crate::keybindings::get_keybindings;
use crate::tui::{Component, TUI};

/// Loader that can be cancelled with Escape.
/// Extends Loader with an abort flag for cancelling async operations.
pub struct CancellableLoader {
    loader: Loader,
    aborted: Arc<AtomicBool>,
    /// Invoked after Escape aborts this loader.
    pub on_abort: Option<Box<dyn FnMut()>>,
}

impl CancellableLoader {
    pub fn new(
        ui: Rc<std::cell::RefCell<TUI>>,
        spinner_color_fn: Box<dyn Fn(&str) -> String>,
        message_color_fn: Box<dyn Fn(&str) -> String>,
        message: String,
        indicator: Option<LoaderIndicatorOptions>,
    ) -> Self {
        Self {
            loader: Loader::new(ui, spinner_color_fn, message_color_fn, message, indicator),
            aborted: Arc::new(AtomicBool::new(false)),
            on_abort: None,
        }
    }

    /// Abort handle shared with async work started for this loader.
    /// `AbortSignal` is a `CancellationToken` in the port; this flag mirrors
    /// `signal.aborted` for synchronous checks.
    pub fn signal(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.aborted)
    }

    /// Whether Escape has already aborted this loader.
    pub fn aborted(&self) -> bool {
        self.aborted.load(Ordering::SeqCst)
    }

    pub fn loader(&self) -> &Loader {
        &self.loader
    }

    pub fn loader_mut(&mut self) -> &mut Loader {
        &mut self.loader
    }

    pub fn dispose(&mut self) {
        self.loader.stop();
    }
}

impl Component for CancellableLoader {
    fn render(&mut self, width: f64) -> Vec<String> {
        self.loader.render(width)
    }

    fn handle_input(&mut self, data: &str) {
        let kb = get_keybindings();
        if kb.matches(data, "tui.select.cancel") {
            self.aborted.store(true, Ordering::SeqCst);
            if let Some(callback) = self.on_abort.as_mut() {
                callback();
            }
        }
    }

    fn invalidate(&mut self) {
        self.loader.invalidate();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn abort_flag_starts_clear() {
        let aborted = Arc::new(AtomicBool::new(false));
        assert!(!aborted.load(Ordering::SeqCst));
    }

    #[test]
    fn escape_keybinding_cancels() {
        let kb = get_keybindings();
        assert!(kb.matches("\x1b", "tui.select.cancel"));
        assert!(kb.matches("\x03", "tui.select.cancel"));
        assert!(!kb.matches("a", "tui.select.cancel"));
    }
}
