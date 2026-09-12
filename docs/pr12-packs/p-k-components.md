# Pack p-k-components - repairK-components - 26 errors in 13 files

Owned files (edit ONLY these):
- crates/pi-coding-agent/src/modes/interactive/components/compaction_summary_message.rs
- crates/pi-coding-agent/src/modes/interactive/components/conversation_components.rs
- crates/pi-coding-agent/src/modes/interactive/components/custom_editor.rs
- crates/pi-coding-agent/src/modes/interactive/components/daxnuts.rs
- crates/pi-coding-agent/src/modes/interactive/components/dynamic_border.rs
- crates/pi-coding-agent/src/modes/interactive/components/extension_input.rs
- crates/pi-coding-agent/src/modes/interactive/components/extension_selector.rs
- crates/pi-coding-agent/src/modes/interactive/components/mod.rs
- crates/pi-coding-agent/src/modes/interactive/components/skill_invocation_message.rs
- crates/pi-coding-agent/src/modes/interactive/components/subagent_summary_line.rs
- crates/pi-coding-agent/src/modes/interactive/components/thinking_selector.rs
- crates/pi-coding-agent/src/modes/interactive/components/user_message.rs
- crates/pi-coding-agent/src/modes/interactive/onboarding.rs

## Errors to fix (file:line: message)
- `modes/interactive/components/compaction_summary_message.rs:70` mismatched types
    - `interactive::theme::theme::MarkdownTheme` and `pi_tui::r#mod::MarkdownTheme` have similar names, but are actually distinct types
- `modes/interactive/components/conversation_components.rs:360` struct `pi_ai::index::ImageContent` has no field named `r#type`
    - a field with a similar name exists
- `modes/interactive/components/conversation_components.rs:396` mismatched types
- `modes/interactive/components/conversation_components.rs:419` mismatched types
- `modes/interactive/components/custom_editor.rs:605` mismatched types
    - `interactive::theme::theme::EditorTheme` and `pi_tui::r#mod::EditorTheme` have similar names, but are actually distinct types
- `modes/interactive/components/daxnuts.rs:114` future cannot be sent between threads safely
    - within `{async block@crates\pi-coding-agent\src\modes\interactive\components\daxnuts.rs:114:33: 114:43}`, the trait `std::marker::Send` is not implemented for `Rc<RefCell<TUI>>`
    - captured value is not `Send`
- `modes/interactive/components/dynamic_border.rs:38` mismatched types
    - implementation defined here
    - consider borrowing here
- `modes/interactive/components/extension_input.rs:213` mismatched types
    - associated function defined here
    - try removing the method call
- `modes/interactive/components/extension_input.rs:222` the method `get` exists for struct `std::rc::Rc<Cell<std::string::String>>`, but its trait bounds were not satisfied
    - the following trait bounds were not satisfied:
`std::string::String: std::marker::Copy`
    - items from traits can only be used if the trait is implemented and in scope
- `modes/interactive/components/extension_input.rs:231` mismatched types
    - associated function defined here
    - try removing the method call
- `modes/interactive/components/extension_input.rs:245` mismatched types
    - associated function defined here
    - try removing the method call
- `modes/interactive/components/extension_selector.rs:326` mismatched types
    - associated function defined here
    - try removing the method call
- `modes/interactive/components/extension_selector.rs:345` mismatched types
    - associated function defined here
    - try removing the method call
- `modes/interactive/components/extension_selector.rs:352` the method `get` exists for struct `std::rc::Rc<Cell<std::string::String>>`, but its trait bounds were not satisfied
    - the following trait bounds were not satisfied:
`std::string::String: std::marker::Copy`
    - items from traits can only be used if the trait is implemented and in scope
- `modes/interactive/components/extension_selector.rs:355` the method `get` exists for struct `std::rc::Rc<Cell<std::string::String>>`, but its trait bounds were not satisfied
    - the following trait bounds were not satisfied:
`std::string::String: std::marker::Copy`
    - items from traits can only be used if the trait is implemented and in scope
- `modes/interactive/components/extension_selector.rs:364` mismatched types
    - associated function defined here
    - try removing the method call
- `modes/interactive/components/mod.rs:133` expected a type, found a trait
    - you can add the `dyn` keyword if you want a trait object
- `modes/interactive/components/mod.rs:134` expected a type, found a trait
    - you can add the `dyn` keyword if you want a trait object
- `modes/interactive/components/mod.rs:136` cannot find value `FILTER_MODES` in this scope
    - consider importing this constant
- `modes/interactive/components/skill_invocation_message.rs:66` mismatched types
    - `interactive::theme::theme::MarkdownTheme` and `pi_tui::r#mod::MarkdownTheme` have similar names, but are actually distinct types
- `modes/interactive/components/subagent_summary_line.rs:79` mismatched types
    - `daemon_session_list::SessionSummary` and `agents_view_state::SessionSummary` have similar names, but are actually distinct types
- `modes/interactive/components/thinking_selector.rs:85` mismatched types
    - `SelectListTheme` and `pi_tui::r#mod::SelectListTheme` have similar names, but are actually distinct types
- `modes/interactive/components/user_message.rs:120` mismatched types
    - `interactive::theme::theme::MarkdownTheme` and `pi_tui::r#mod::MarkdownTheme` have similar names, but are actually distinct types
- `modes/interactive/components/user_message.rs:151` mismatched types
    - `interactive::theme::theme::MarkdownTheme` and `pi_tui::r#mod::MarkdownTheme` have similar names, but are actually distinct types
- `modes/interactive/components/user_message.rs:165` mismatched types
    - `interactive::theme::theme::MarkdownTheme` and `pi_tui::r#mod::MarkdownTheme` have similar names, but are actually distinct types
- `modes/interactive/onboarding.rs:28` mismatched types
    - expected mutable reference `&mut interactive_mode_services::ModelRegistry`
           found reference `&interactive_mode_services::ModelRegistry`
    - method defined here
