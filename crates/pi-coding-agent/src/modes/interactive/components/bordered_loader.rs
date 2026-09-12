//! Port of packages/coding-agent/src/modes/interactive/components/bordered-loader.ts

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use pi_tui::components::cancellable_loader::CancellableLoader;
use pi_tui::components::loader::Loader;
use pi_tui::components::spacer::Spacer;
use pi_tui::components::text::Text;
use pi_tui::tui::{Component, TUI};

use crate::modes::interactive::theme::theme::Theme;

use super::keybinding_hints::key_hint;

/// `DynamicBorder` (components/dynamic-border.ts) belongs to another slice, so
/// this module keeps a private copy of the one-line border it renders; see
/// evidence/status/ca-interactive-components-3.json -> blocked_on.
struct BorderLine {
    color: Box<dyn Fn(&str) -> String>,
}

impl BorderLine {
    fn new(color: Box<dyn Fn(&str) -> String>) -> Self {
        Self { color }
    }
}

impl Component for BorderLine {
    fn render(&mut self, width: f64) -> Vec<String> {
        let width = (width.max(0.0).floor() as usize).max(1);
        vec![(self.color)("\u{2500}".repeat(width).as_str())]
    }

    fn invalidate(&mut self) {}
}

/// Port of `options?: { cancellable?: boolean }`.
#[derive(Debug, Clone, Copy, Default)]
pub struct BorderedLoaderOptions {
    pub cancellable: Option<bool>,
}

enum LoaderKind {
    Cancellable(CancellableLoader),
    Plain(Loader),
}

/// Port of `BorderedLoader`.
///
/// The TypeScript pushes the parts into the `Container` child list; the port
/// keeps the same parts as fields and renders them in the same order, because a
/// Rust component cannot be owned by a child list and read through a field at
/// the same time.
pub struct BorderedLoader {
    top_border: BorderLine,
    loader: LoaderKind,
    spacer_before_hint: Option<Spacer>,
    hint: Option<Text>,
    spacer_bottom: Spacer,
    bottom_border: BorderLine,
    cancellable: bool,
    /// `new AbortController().signal` for the non-cancellable case.
    signal_controller: Option<Arc<AtomicBool>>,
}

impl BorderedLoader {
    pub fn new(
        tui: Rc<RefCell<TUI>>,
        theme: Arc<Theme>,
        message: String,
        options: BorderedLoaderOptions,
    ) -> Self {
        let cancellable = options.cancellable.unwrap_or(true);

        let muted_spinner = {
            let theme = Arc::clone(&theme);
            Box::new(move |s: &str| theme.fg("muted", s)) as Box<dyn Fn(&str) -> String>
        };
        let muted_message = {
            let theme = Arc::clone(&theme);
            Box::new(move |s: &str| theme.fg("muted", s)) as Box<dyn Fn(&str) -> String>
        };

        let loader = if cancellable {
            LoaderKind::Cancellable(CancellableLoader::new(
                tui,
                muted_spinner,
                muted_message,
                message,
                None,
            ))
        } else {
            LoaderKind::Plain(Loader::new(
                tui,
                muted_spinner,
                muted_message,
                message,
                None,
            ))
        };

        let (spacer_before_hint, hint) = if cancellable {
            (
                Some(Spacer::new(1)),
                Some(Text::new(
                    key_hint("tui.select.cancel", "cancel", &Default::default()),
                    1,
                    0,
                    None,
                )),
            )
        } else {
            (None, None)
        };

        Self {
            top_border: BorderLine::new(border_color(Arc::clone(&theme))),
            loader,
            spacer_before_hint,
            hint,
            spacer_bottom: Spacer::new(1),
            bottom_border: BorderLine::new(border_color(theme)),
            cancellable,
            signal_controller: if cancellable {
                None
            } else {
                Some(Arc::new(AtomicBool::new(false)))
            },
        }
    }

    /// Port of the `signal` getter. `AbortSignal` is the loader's abort flag
    /// (`CancellableLoader.signal()`), or an always-clear flag when the loader is
    /// not cancellable.
    pub fn signal(&self) -> Arc<AtomicBool> {
        match &self.loader {
            LoaderKind::Cancellable(loader) => loader.signal(),
            LoaderKind::Plain(_) => self
                .signal_controller
                .clone()
                .unwrap_or_else(|| Arc::new(AtomicBool::new(false))),
        }
    }

    /// Whether this loader's `AbortSignal` is aborted (`signal.aborted`).
    pub fn signal_aborted(&self) -> bool {
        self.signal().load(Ordering::SeqCst)
    }

    /// Port of the `onAbort` setter. Only the cancellable loader receives it.
    pub fn set_on_abort(&mut self, callback: Option<Box<dyn FnMut()>>) {
        if let LoaderKind::Cancellable(loader) = &mut self.loader {
            loader.on_abort = callback;
        }
    }

    pub fn cancellable(&self) -> bool {
        self.cancellable
    }

    /// Port of `dispose`: `dispose()` when present, otherwise `stop()`.
    pub fn dispose(&mut self) {
        match &mut self.loader {
            LoaderKind::Cancellable(loader) => loader.dispose(),
            LoaderKind::Plain(loader) => loader.stop(),
        }
    }
}

fn border_color(theme: Arc<Theme>) -> Box<dyn Fn(&str) -> String> {
    Box::new(move |s: &str| theme.fg("border", s))
}

impl Component for BorderedLoader {
    fn render(&mut self, width: f64) -> Vec<String> {
        let mut lines = self.top_border.render(width);
        lines.extend(match &mut self.loader {
            LoaderKind::Cancellable(loader) => loader.render(width),
            LoaderKind::Plain(loader) => loader.render(width),
        });
        if let Some(spacer) = self.spacer_before_hint.as_mut() {
            lines.extend(spacer.render(width));
        }
        if let Some(hint) = self.hint.as_mut() {
            lines.extend(hint.render(width));
        }
        lines.extend(self.spacer_bottom.render(width));
        lines.extend(self.bottom_border.render(width));
        lines
    }

    fn handle_input(&mut self, data: &str) {
        if self.cancellable {
            if let LoaderKind::Cancellable(loader) = &mut self.loader {
                loader.handle_input(data);
            }
        }
    }

    fn invalidate(&mut self) {
        self.top_border.invalidate();
        match &mut self.loader {
            LoaderKind::Cancellable(loader) => loader.invalidate(),
            LoaderKind::Plain(loader) => loader.invalidate(),
        }
        if let Some(spacer) = self.spacer_before_hint.as_mut() {
            spacer.invalidate();
        }
        if let Some(hint) = self.hint.as_mut() {
            hint.invalidate();
        }
        self.spacer_bottom.invalidate();
        self.bottom_border.invalidate();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn theme() -> Arc<Theme> {
        crate::modes::interactive::theme::theme::theme()
    }

    fn loader(cancellable: bool) -> BorderedLoader {
        let terminal = pi_tui::terminal::ProcessTerminal::new();
        let ui = Rc::new(RefCell::new(TUI::new(Box::new(terminal), Some(false))));
        BorderedLoader::new(
            ui,
            theme(),
            "working".to_string(),
            BorderedLoaderOptions {
                cancellable: Some(cancellable),
            },
        )
    }

    #[test]
    fn renders_borders_loader_hint_and_spacers() {
        let mut bordered = loader(true);
        let lines = bordered.render(6.0);
        assert_eq!(lines[0], "\u{2500}".repeat(6));
        assert_eq!(lines[lines.len() - 1], "\u{2500}".repeat(6));
        assert!(lines.len() >= 6);
    }

    #[test]
    fn non_cancellable_loaders_omit_the_hint_block() {
        let mut cancellable = loader(true);
        let mut plain = loader(false);
        assert!(cancellable.render(6.0).len() > plain.render(6.0).len());
        assert_eq!(plain.signal_aborted(), false);
        assert_eq!(cancellable.signal_aborted(), false);
    }

    #[test]
    fn escape_aborts_only_the_cancellable_loader() {
        let mut cancellable = loader(true);
        cancellable.handle_input("\u{1b}");
        assert!(cancellable.signal_aborted());
        let mut plain = loader(false);
        plain.handle_input("\u{1b}");
        assert!(!plain.signal_aborted());
    }

    #[test]
    fn on_abort_fires_for_the_cancellable_loader() {
        let fired = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&fired);
        let mut cancellable = loader(true);
        cancellable.set_on_abort(Some(Box::new(move || flag.store(true, Ordering::SeqCst))));
        cancellable.handle_input("\u{1b}");
        assert!(fired.load(Ordering::SeqCst));
    }

    #[test]
    fn dispose_stops_the_loader() {
        let mut cancellable = loader(true);
        cancellable.dispose();
        cancellable.dispose();
    }
}
