//! Port of packages/coding-agent/src/modes/interactive/components/centered-overlay.ts

use std::cell::RefCell;
use std::rc::Rc;

use pi_tui::tui::{Component, Focusable, OverlayHandle, OverlayOptions, SizeValue, TUI};
use pi_tui::utils::{truncate_to_width, visible_width};

/// Port of `CenteredOverlayOptions`.
pub struct CenteredOverlayOptions {
    pub get_rows: Rc<dyn Fn() -> f64>,
    pub max_content_width: Option<f64>,
    pub vertical_offset: Option<f64>,
}

/// Port of `FullPaneOverlayOptions`.
#[derive(Debug, Clone, Copy, Default)]
pub struct FullPaneOverlayOptions {
    pub max_content_width: Option<f64>,
    pub full_width: Option<bool>,
    pub suspend_fullscreen_mouse: Option<bool>,
}

/// Port of the `number | FullPaneOverlayOptions` parameter of
/// `showFullPaneOverlay` (default `80`).
#[derive(Debug, Clone, Copy)]
pub enum OverlayShowOptions {
    MaxContentWidth(f64),
    Full(FullPaneOverlayOptions),
}

impl Default for OverlayShowOptions {
    fn default() -> Self {
        OverlayShowOptions::MaxContentWidth(80.0)
    }
}

/// Port of `hasInputHandler`. Every Rust `Component` has `handle_input`, whose
/// default implementation is the TypeScript "no handleInput" case, so the guard
/// is always true here.
fn has_input_handler(_component: &Rc<RefCell<dyn Component>>) -> bool {
    true
}

/// Shows a component as a full-pane centered overlay on the given TUI.
pub fn show_full_pane_overlay(
    ui: &mut TUI,
    component: Rc<RefCell<dyn Component>>,
    options: OverlayShowOptions,
) -> OverlayHandle {
    let (max_content_width, suspend_fullscreen_mouse) = match options {
        OverlayShowOptions::MaxContentWidth(value) => (Some(value), None),
        OverlayShowOptions::Full(options) => (
            if options.full_width.unwrap_or(false) {
                None
            } else {
                Some(options.max_content_width.unwrap_or(80.0))
            },
            options.suspend_fullscreen_mouse,
        ),
    };
    let mut overlay_options = OverlayOptions {
        width: Some(SizeValue::Percent("100%".to_string())),
        max_height: Some(SizeValue::Percent("100%".to_string())),
        row: Some(SizeValue::Number(0.0)),
        col: Some(SizeValue::Number(0.0)),
        ..Default::default()
    };
    if suspend_fullscreen_mouse.unwrap_or(false) {
        overlay_options.suspend_fullscreen_mouse = true;
    }

    let rows = ui.terminal.rows() as f64;
    let get_rows: Rc<dyn Fn() -> f64> = Rc::new(move || rows);
    let overlay: Rc<RefCell<dyn Component>> = Rc::new(RefCell::new(CenteredOverlayComponent::new(
        component,
        CenteredOverlayOptions {
            get_rows,
            max_content_width,
            vertical_offset: None,
        },
    )));

    ui.show_overlay(overlay, overlay_options)
}

/// A component whose rows come from the owning TUI. The TypeScript closure
/// `() => ui.terminal.rows` reads the live terminal; `show_full_pane_overlay`
/// captures the value at call time because the port cannot alias the TUI.
pub struct CenteredOverlayComponent {
    component: Rc<RefCell<dyn Component>>,
    options: CenteredOverlayOptions,
    focused: bool,
}

impl CenteredOverlayComponent {
    pub fn new(component: Rc<RefCell<dyn Component>>, options: CenteredOverlayOptions) -> Self {
        Self {
            component,
            options,
            focused: false,
        }
    }

    fn place(&self, text: &str, width: usize, left: usize) -> String {
        let safe_left = left.min(width);
        let content_width = width.saturating_sub(safe_left);
        let content = truncate_to_width(text, content_width as f64, "", false);
        let right = width
            .saturating_sub(safe_left)
            .saturating_sub(visible_width(&content));
        format!("{}{}{}", " ".repeat(safe_left), content, " ".repeat(right))
    }

    fn blank(&self, width: usize) -> String {
        " ".repeat(width)
    }
}

impl Focusable for CenteredOverlayComponent {
    fn focused(&self) -> bool {
        self.focused
    }

    fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
        if let Some(focusable) = self.component.borrow_mut().as_focusable() {
            focusable.set_focused(focused);
        }
    }
}

impl Component for CenteredOverlayComponent {
    fn render(&mut self, width: f64) -> Vec<String> {
        let safe_width = (width.max(1.0).floor() as usize).max(1);
        let content_width = safe_width.min(
            self.options
                .max_content_width
                .map(|value| value.max(0.0).floor() as usize)
                .unwrap_or(safe_width),
        );
        let left = ((safe_width.saturating_sub(content_width)) / 2).max(0);
        let content_lines: Vec<String> = self
            .component
            .borrow_mut()
            .render(content_width as f64)
            .iter()
            .map(|line| self.place(line, safe_width, left))
            .collect();
        let requested_rows = (self.options.get_rows)();
        let target_rows = if requested_rows.is_finite() && requested_rows > 0.0 {
            content_lines.len().max(requested_rows.floor() as usize)
        } else {
            content_lines.len()
        };
        let centered_top = ((target_rows as f64 - content_lines.len() as f64) / 2.0).floor() as i64
            + self.options.vertical_offset.unwrap_or(0.0) as i64;
        let max_top = target_rows.saturating_sub(content_lines.len()) as i64;
        let top_padding = 0.max(centered_top.min(max_top)) as usize;
        let bottom_padding = target_rows
            .saturating_sub(content_lines.len())
            .saturating_sub(top_padding);

        let mut result: Vec<String> = Vec::new();
        for _ in 0..top_padding {
            result.push(self.blank(safe_width));
        }
        result.extend(content_lines);
        for _ in 0..bottom_padding {
            result.push(self.blank(safe_width));
        }
        result
    }

    fn handle_input(&mut self, data: &str) {
        if has_input_handler(&self.component) {
            self.component.borrow_mut().handle_input(data);
        }
    }

    fn invalidate(&mut self) {
        self.component.borrow_mut().invalidate();
    }

    fn as_focusable(&mut self) -> Option<&mut dyn Focusable> {
        Some(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Lines(Vec<String>);

    impl Component for Lines {
        fn render(&mut self, _width: f64) -> Vec<String> {
            self.0.clone()
        }
        fn invalidate(&mut self) {}
    }

    fn options(
        rows: f64,
        max_content_width: Option<f64>,
        vertical_offset: Option<f64>,
    ) -> CenteredOverlayOptions {
        CenteredOverlayOptions {
            get_rows: Rc::new(move || rows),
            max_content_width,
            vertical_offset,
        }
    }

    fn component(lines: Vec<&str>, options: CenteredOverlayOptions) -> CenteredOverlayComponent {
        CenteredOverlayComponent::new(
            Rc::new(RefCell::new(Lines(
                lines.into_iter().map(|l| l.to_string()).collect(),
            ))),
            options,
        )
    }

    #[test]
    fn centers_content_horizontally() {
        let mut overlay = component(vec!["hi"], options(1.0, Some(4.0), None));
        assert_eq!(overlay.render(10.0), vec!["   hi     ".to_string()]);
    }

    #[test]
    fn vertical_padding_centers_the_content_and_fills_the_requested_rows() {
        let mut overlay = component(vec!["a"], options(5.0, None, None));
        assert_eq!(
            overlay.render(2.0),
            vec![
                "  ".to_string(),
                "  ".to_string(),
                "a ".to_string(),
                "  ".to_string(),
                "  ".to_string()
            ]
        );
    }

    #[test]
    fn a_non_finite_row_count_uses_the_content_height() {
        let mut overlay = component(vec!["a", "b"], options(f64::NAN, None, None));
        assert_eq!(overlay.render(1.0), vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn vertical_offset_shifts_content_within_the_available_space() {
        let mut overlay = component(vec!["a"], options(4.0, None, Some(99.0)));
        assert_eq!(overlay.render(1.0).len(), 4);
        assert_eq!(overlay.render(1.0)[3], "a");
    }

    #[test]
    fn handle_input_and_focus_propagate_to_the_child() {
        struct Recorder {
            inputs: Vec<String>,
            focused: bool,
        }
        impl Component for Recorder {
            fn render(&mut self, _width: f64) -> Vec<String> {
                Vec::new()
            }
            fn handle_input(&mut self, data: &str) {
                self.inputs.push(data.to_string());
            }
            fn invalidate(&mut self) {}
            fn as_focusable(&mut self) -> Option<&mut dyn Focusable> {
                Some(self)
            }
        }
        impl Focusable for Recorder {
            fn focused(&self) -> bool {
                self.focused
            }
            fn set_focused(&mut self, focused: bool) {
                self.focused = focused;
            }
        }

        let child = Rc::new(RefCell::new(Recorder {
            inputs: Vec::new(),
            focused: false,
        }));
        let mut overlay = CenteredOverlayComponent::new(child.clone(), options(1.0, None, None));
        overlay.handle_input("x");
        overlay.set_focused(true);
        assert!(overlay.focused());
        assert!(child.borrow().focused);
        assert_eq!(child.borrow().inputs, vec!["x".to_string()]);
    }

    #[test]
    fn show_options_default_is_eighty_columns() {
        match OverlayShowOptions::default() {
            OverlayShowOptions::MaxContentWidth(value) => assert_eq!(value, 80.0),
            OverlayShowOptions::Full(_) => panic!("default is the numeric form"),
        }
    }
}
