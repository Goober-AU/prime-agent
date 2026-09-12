//! Port of packages/coding-agent/src/modes/interactive/components/extension-input.ts
//!
//! Simple text input component for extensions.

use pi_tui::components::input::Input;
use pi_tui::components::spacer::Spacer;
use pi_tui::components::text::Text;
use pi_tui::keybindings::get_keybindings;
use pi_tui::tui::{Component, Container, Focusable, TUI};
use std::cell::RefCell;
use std::rc::Rc;

use crate::modes::interactive::components::countdown_timer::CountdownTimer;
use crate::modes::interactive::components::dynamic_border::DynamicBorder;
use crate::modes::interactive::components::keybinding_hints::key_hint;
use crate::modes::interactive::theme::theme::theme;

/// `ExtensionInputOptions`
pub struct ExtensionInputOptions {
    pub tui: Option<Rc<RefCell<TUI>>>,
    pub timeout: Option<f64>,
}

impl Default for ExtensionInputOptions {
    fn default() -> Self {
        Self {
            tui: None,
            timeout: None,
        }
    }
}

/// Port of `ExtensionInputComponent`.
pub struct ExtensionInputComponent {
    input: Rc<RefCell<Input>>,
    on_submit_callback: Box<dyn FnMut(&str)>,
    on_cancel_callback: Rc<RefCell<Box<dyn FnMut()>>>,
    title_text: Text,
    base_title: String,
    countdown: Option<CountdownTimer>,
    /// Focusable implementation - propagated to the input for IME cursor positioning.
    focused: bool,
    container: Container,
    title_slot: Rc<RefCell<String>>,
}

impl ExtensionInputComponent {
    /// Port of the `ExtensionInputComponent` constructor.
    pub fn new(
        title: &str,
        _placeholder: Option<String>,
        on_submit: Box<dyn FnMut(&str)>,
        on_cancel: Box<dyn FnMut()>,
        opts: ExtensionInputOptions,
    ) -> Self {
        let mut container = Container::new();
        let base_title = title.to_string();

        container.add_child(
            Rc::new(RefCell::new(DynamicBorder::default())) as Rc<RefCell<dyn Component>>
        );
        container.add_child(Rc::new(RefCell::new(Spacer::new(1))) as Rc<RefCell<dyn Component>>);

        let title_text = Text::new(theme().fg("accent", title), 1, 0, None);
        container.add_child(Rc::new(RefCell::new(title_text)) as Rc<RefCell<dyn Component>>);
        container.add_child(Rc::new(RefCell::new(Spacer::new(1))) as Rc<RefCell<dyn Component>>);

        let title_slot: Rc<RefCell<String>> = Rc::new(RefCell::new(theme().fg("accent", title)));
        // Shared with the timer's expire callback so the countdown and the cancel
        // key call the same `onCancelCallback`.
        let on_cancel_cell: Rc<RefCell<Box<dyn FnMut()>>> = Rc::new(RefCell::new(on_cancel));

        let mut countdown = None;
        if let Some(timeout) = opts.timeout {
            if timeout > 0.0 {
                if let Some(tui) = opts.tui {
                    let base_title_for_tick = base_title.clone();
                    let slot = Rc::clone(&title_slot);
                    // `() => this.onCancelCallback()` - the timer calls the same
                    // callback the cancel key does, so both share the box.
                    let on_expire = Rc::clone(&on_cancel_cell);
                    countdown = Some(CountdownTimer::new(
                        timeout,
                        Some(tui),
                        Box::new(move |seconds| {
                            *slot.borrow_mut() = theme()
                                .fg("accent", &format!("{base_title_for_tick} ({seconds}s)"));
                        }),
                        Box::new(move || (on_expire.borrow_mut())()),
                    ));
                }
            }
        }

        // The input is shared with the container so `handleInput` and `render`
        // reach the same instance, matching `this.input` in the TypeScript.
        let input_cell = Rc::new(RefCell::new(Input::new()));
        container.add_child(Rc::clone(&input_cell) as Rc<RefCell<dyn Component>>);
        container.add_child(Rc::new(RefCell::new(Spacer::new(1))) as Rc<RefCell<dyn Component>>);
        container.add_child(Rc::new(RefCell::new(Text::new(
            format!(
                "{}  {}",
                key_hint("tui.select.confirm", "submit", &Default::default()),
                key_hint("tui.select.cancel", "cancel", &Default::default())
            ),
            1,
            0,
            None,
        ))) as Rc<RefCell<dyn Component>>);
        container.add_child(Rc::new(RefCell::new(Spacer::new(1))) as Rc<RefCell<dyn Component>>);
        container.add_child(
            Rc::new(RefCell::new(DynamicBorder::default())) as Rc<RefCell<dyn Component>>
        );

        // Read the slot's value into an owned string before the struct literal:
        // the temporary `Ref` borrow must end before the slot is moved.
        let initial_title = title_slot.borrow().clone();

        Self {
            input: Rc::clone(&input_cell),
            on_submit_callback: on_submit,
            on_cancel_callback: Rc::clone(&on_cancel_cell),
            title_text: Text::new(initial_title, 1, 0, None),
            base_title,
            countdown,
            focused: false,
            container,
            title_slot,
        }
    }
}

impl Component for ExtensionInputComponent {
    fn render(&mut self, width: f64) -> Vec<String> {
        // The timer posts its ticks to the owner (see `CountdownTimer`), so the
        // render pass drains them before producing output.
        if let Some(countdown) = self.countdown.as_mut() {
            countdown.poll();
        }
        self.container.render(width)
    }

    fn get_selection_regions(&self) -> Vec<pi_tui::selection_metadata::TableCellSelectionRegion> {
        self.container.get_selection_regions()
    }

    fn handle_input(&mut self, key_data: &str) {
        let kb = get_keybindings();
        if kb.matches(key_data, "tui.select.confirm") || key_data == "\n" {
            let value = self.input.borrow().get_value().to_string();
            (self.on_submit_callback)(&value);
        } else if kb.matches(key_data, "tui.select.cancel") {
            (self.on_cancel_callback.borrow_mut())();
        } else {
            self.input.borrow_mut().handle_input(key_data);
        }
    }

    fn invalidate(&mut self) {
        self.container.invalidate();
    }

    fn as_focusable(&mut self) -> Option<&mut dyn Focusable> {
        Some(self)
    }
}

impl Focusable for ExtensionInputComponent {
    fn focused(&self) -> bool {
        self.focused
    }

    fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
        self.input.borrow_mut().set_focused(focused);
    }
}

impl ExtensionInputComponent {
    /// Port of `dispose`.
    pub fn dispose(&mut self) {
        if let Some(countdown) = self.countdown.as_mut() {
            countdown.dispose();
        }
    }

    /// Test access to the input value.
    pub fn value(&self) -> String {
        self.input.borrow().get_value().to_string()
    }

    /// Test access to the countdown title slot.
    pub fn title_slot(&self) -> Rc<RefCell<String>> {
        Rc::clone(&self.title_slot)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    fn init() {
        crate::modes::interactive::theme::theme::init_theme(Some("prime"), false);
    }

    #[test]
    fn submits_the_input_value_on_enter() {
        init();
        let submitted = Rc::new(RefCell::new(String::new()));
        let submitted_for_callback = Rc::clone(&submitted);
        let mut component = ExtensionInputComponent::new(
            "Title",
            None,
            Box::new(move |value| *submitted_for_callback.borrow_mut() = value.to_string()),
            Box::new(|| {}),
            ExtensionInputOptions::default(),
        );
        component.handle_input("h");
        component.handle_input("i");
        component.handle_input("\n");
        assert_eq!(*submitted.borrow(), "hi");
    }

    #[test]
    fn cancel_invokes_the_cancel_callback() {
        init();
        let cancelled = Rc::new(RefCell::new(false));
        let cancelled_for_callback = Rc::clone(&cancelled);
        let mut component = ExtensionInputComponent::new(
            "Title",
            None,
            Box::new(|_| {}),
            Box::new(move || *cancelled_for_callback.borrow_mut() = true),
            ExtensionInputOptions::default(),
        );
        component.handle_input("\u{1b}");
        assert!(*cancelled.borrow());
    }

    #[test]
    fn focus_propagates_to_the_input() {
        init();
        let mut component = ExtensionInputComponent::new(
            "Title",
            None,
            Box::new(|_| {}),
            Box::new(|| {}),
            ExtensionInputOptions::default(),
        );
        assert!(!Focusable::focused(&component));
        Focusable::set_focused(&mut component, true);
        assert!(Focusable::focused(&component));
        assert!(Focusable::focused(&*component.input.borrow()));
    }
}
