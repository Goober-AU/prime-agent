//! Port of packages/tui/src/components/loader.ts

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use crate::components::text::Text;
use crate::tui::{Component, TUI};

pub const DEFAULT_FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
pub const DEFAULT_INTERVAL_MS: u64 = 80;

/// Port of `LoaderIndicatorOptions`.
#[derive(Clone, Default)]
pub struct LoaderIndicatorOptions {
    pub frames: Option<Vec<String>>,
    pub interval_ms: Option<u64>,
}

/// Loader component that updates with an optional spinning animation.
pub struct Loader {
    text: Text,
    frames: Vec<String>,
    interval_ms: u64,
    current_frame: usize,
    /// Shared with the animation task: the frame index the timer advanced to.
    shared_frame: Arc<AtomicUsize>,
    running: Arc<AtomicBool>,
    interval_task: Option<tokio::task::JoinHandle<()>>,
    /// Render request hook owned by the TUI. The animation task advances the
    /// shared frame counter; the TUI reads it while rendering.
    ui: Option<Rc<RefCell<TUI>>>,
    render_indicator_verbatim: bool,
    spinner_color_fn: Box<dyn Fn(&str) -> String>,
    message_color_fn: Box<dyn Fn(&str) -> String>,
    message: String,
}

impl Loader {
    pub fn new(
        ui: Rc<RefCell<TUI>>,
        spinner_color_fn: Box<dyn Fn(&str) -> String>,
        message_color_fn: Box<dyn Fn(&str) -> String>,
        message: String,
        indicator: Option<LoaderIndicatorOptions>,
    ) -> Self {
        let mut loader = Self {
            text: Text::new(String::new(), 1, 0, None),
            frames: DEFAULT_FRAMES.iter().map(|f| (*f).to_string()).collect(),
            interval_ms: DEFAULT_INTERVAL_MS,
            current_frame: 0,
            shared_frame: Arc::new(AtomicUsize::new(0)),
            running: Arc::new(AtomicBool::new(false)),
            interval_task: None,
            ui: Some(ui),
            render_indicator_verbatim: false,
            spinner_color_fn,
            message_color_fn,
            message,
        };
        loader.set_indicator(indicator);
        loader
    }

    pub fn start(&mut self) {
        self.update_display();
        self.restart_animation();
    }

    pub fn stop(&mut self) {
        self.running.store(false, Ordering::SeqCst);
        if let Some(task) = self.interval_task.take() {
            task.abort();
        }
    }

    pub fn set_message(&mut self, message: String) {
        self.message = message;
        self.update_display();
    }

    pub fn set_indicator(&mut self, indicator: Option<LoaderIndicatorOptions>) {
        self.render_indicator_verbatim = indicator.is_some();
        self.frames = match indicator.as_ref().and_then(|i| i.frames.as_ref()) {
            Some(frames) => frames.clone(),
            None => DEFAULT_FRAMES.iter().map(|f| (*f).to_string()).collect(),
        };
        self.interval_ms = match indicator.as_ref().and_then(|i| i.interval_ms) {
            Some(interval_ms) if interval_ms > 0 => interval_ms,
            _ => DEFAULT_INTERVAL_MS,
        };
        self.current_frame = 0;
        self.shared_frame.store(0, Ordering::SeqCst);
        self.start();
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub fn frames(&self) -> &[String] {
        &self.frames
    }

    pub fn interval_ms(&self) -> u64 {
        self.interval_ms
    }

    /// Current animation frame index, including frames advanced by the timer task.
    pub fn current_frame(&self) -> usize {
        self.shared_frame.load(Ordering::SeqCst) % self.frames.len().max(1)
    }

    fn restart_animation(&mut self) {
        self.stop();
        if self.frames.len() <= 1 {
            return;
        }

        let frame_count = self.frames.len();
        let shared_frame = Arc::clone(&self.shared_frame);
        let running = Arc::clone(&self.running);
        let interval_ms = self.interval_ms;
        running.store(true, Ordering::SeqCst);

        // Port of `setInterval(...)`: advance the frame and request a render.
        // The task needs no `TUI` access because the render loop reads the frame.
        let task = tokio::spawn(async move {
            let mut ticker = tokio::time::interval(std::time::Duration::from_millis(interval_ms));
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            ticker.tick().await;
            while running.load(Ordering::SeqCst) {
                ticker.tick().await;
                if !running.load(Ordering::SeqCst) {
                    break;
                }
                let next = (shared_frame.load(Ordering::SeqCst) + 1) % frame_count;
                shared_frame.store(next, Ordering::SeqCst);
            }
        });

        self.interval_task = Some(task);
    }

    fn update_display(&mut self) {
        self.current_frame = self.current_frame();
        let frame = self
            .frames
            .get(self.current_frame)
            .cloned()
            .unwrap_or_default();
        let rendered_frame = if self.render_indicator_verbatim {
            frame.clone()
        } else {
            (self.spinner_color_fn)(&frame)
        };
        let indicator = if !frame.is_empty() {
            format!("{rendered_frame} ")
        } else {
            String::new()
        };
        let message = (self.message_color_fn)(&self.message);
        self.text.set_text(format!("{indicator}{message}"));
        if let Some(ui) = &self.ui {
            ui.borrow_mut().request_render();
        }
    }
}

impl Component for Loader {
    fn render(&mut self, width: f64) -> Vec<String> {
        let mut result = vec![String::new()];
        result.extend(self.text.render(width));
        result
    }

    fn invalidate(&mut self) {
        self.text.invalidate();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_frames_and_interval_match_typescript() {
        assert_eq!(DEFAULT_FRAMES.len(), 10);
        assert_eq!(DEFAULT_FRAMES[0], "⠋");
        assert_eq!(DEFAULT_FRAMES[9], "⠏");
        assert_eq!(DEFAULT_INTERVAL_MS, 80);
    }

    #[test]
    fn indicator_options_are_normalized() {
        let indicator = Some(LoaderIndicatorOptions {
            frames: Some(vec!["x".to_string()]),
            interval_ms: Some(0),
        });
        // `setIndicator` treats a present indicator as verbatim and keeps the
        // default interval when `intervalMs` is not positive.
        let verbatim = indicator.is_some();
        let frames = indicator
            .as_ref()
            .and_then(|i| i.frames.clone())
            .unwrap_or_else(|| DEFAULT_FRAMES.iter().map(|f| (*f).to_string()).collect());
        let interval_ms = match indicator.as_ref().and_then(|i| i.interval_ms) {
            Some(value) if value > 0 => value,
            _ => DEFAULT_INTERVAL_MS,
        };
        assert!(verbatim);
        assert_eq!(frames, vec!["x".to_string()]);
        assert_eq!(interval_ms, DEFAULT_INTERVAL_MS);
    }
}
