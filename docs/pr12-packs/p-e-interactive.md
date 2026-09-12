# Worklist: p-e-interactive

Total errors in this pack: 129  across 11 files

Read docs/PR12-LEAD-DECISIONS.md FIRST (shared root causes + hard rules).
Ownership: these files are YOURS. Do not edit any other file.

### modes/interactive/interactive_mode.rs  (20 errors)
  L374     [E0599] no method named `get_model_id` found for reference `&BrandSplashHeader` in the current scope
  L375     [E0599] no method named `get_cwd` found for reference `&BrandSplashHeader` in the current scope
  L1152    [E0308] mismatched types
  L1409    [E0599] no method named `get_hide_thinking_block` found for reference `&std::sync::Arc<interactive_mode_services::SettingsManager>` in the current scope
  L1468    [E0596] cannot borrow data in an `Arc` as mutable
  L1498    [E0596] cannot borrow data in an `Arc` as mutable
  L1501    [E0596] cannot borrow data in an `Arc` as mutable
  L1516    [E0596] cannot borrow data in an `Arc` as mutable
  L1548    [E0609] no field `git_ref` on type `GitSource`
  L1647    [E0599] no method named `get_onboarding_shown` found for reference `&std::sync::Arc<interactive_mode_services::SettingsManager>` in the current scope
  L1648    [E0599] no method named `set_onboarding_shown` found for reference `&std::sync::Arc<interactive_mode_services::SettingsManager>` in the current scope
  L2191    [E0599] no variant, associated function, or constant named `Priority` found for enum `std::option::Option<T>` in the current scope
  L2418    [E0308] mismatched types
  L2598    [E0599] no method named `get_code_block_indent` found for reference `&std::sync::Arc<interactive_mode_services::SettingsManager>` in the current scope
  L2772    [E0308] mismatched types
  L2772    [E0308] mismatched types
  L3026    [E0507] cannot move out of `self.agents_view_request` which is behind a mutable reference
  L3053    [E0599] no method named `get_fullscreen` found for reference `&std::sync::Arc<interactive_mode_services::SettingsManager>` in the current scope
  L3236    [E0615] attempted to take value of method `role` on type `&CustomAgentMessage`
  L3453    [E0507] cannot move out of `*service_tier` which is behind a shared reference
    --- rendered ---
      error[E0599]: no method named `get_model_id` found for reference `&BrandSplashHeader` in the current scope
         --> crates\pi-coding-agent\src\modes\interactive\interactive_mode.rs:374:48
          |
      374 |             lines.push(labelled("model", &self.get_model_id().unwrap_or_else(|| "\u{2014}".to_string())));
          |                                                ^^^^^^^^^^^^ field, not a method
          |
      help: to call the trait object stored in `get_model_id`, surround the field access with parentheses
          |
      374 |             lines.push(labelled("model", &(self.get_model_id)().unwrap_or_else(|| "\u{2014}".to_string())));
          |                                           +                 +
    --- rendered ---
      error[E0599]: no method named `get_cwd` found for reference `&BrandSplashHeader` in the current scope
         --> crates\pi-coding-agent\src\modes\interactive\interactive_mode.rs:375:65
          |
      375 |             lines.push(labelled("cwd", &format_splash_cwd(&self.get_cwd())));
          |                                                                 ^^^^^^^ field, not a method
          |
          = help: items from traits can only be used if the trait is implemented and in scope
      note: `SessionCwdSource` defines an item `get_cwd`, perhaps you need to implement it
         --> crates\pi-coding-agent\src\core\session_cwd.rs:4:1
          |
        4 | pub trait SessionCwdSource {
          | ^^^^^^^^^^^^^^^^^^^^^^^^^^
      help: to call the trait object stored in `get_cwd`, surround the field access with parentheses
          |
    --- rendered ---
      error[E0308]: mismatched types
          --> crates\pi-coding-agent\src\modes\interactive\interactive_mode.rs:1152:81
           |
      1152 |         if !crate::modes::agents_view::agents_view_state::is_direct_agent_child(child, parent.0, parent.1, parent.2) {
           |             ------------------------------------------------------------------- ^^^^^ expected `agents_view_state::SessionSummary`, found `daemon_session_list::SessionSummary`
           |             |
           |             arguments to this function are incorrect
           |
           = note: `daemon_session_list::SessionSummary` and `agents_view_state::SessionSummary` have similar names, but are actually distinct types
      note: `daemon_session_list::SessionSummary` is defined in module `crate::modes::daemon::daemon_session_list` of the current crate
          --> crates\pi-coding-agent\src\modes\daemon\daemon_session_list.rs:101:1
           |
       101 | pub struct SessionSummary {
           | ^^^^^^^^^^^^^^^^^^^^^^^^^
    --- rendered ---
      error[E0599]: no method named `get_hide_thinking_block` found for reference `&std::sync::Arc<interactive_mode_services::SettingsManager>` in the current scope
          --> crates\pi-coding-agent\src\modes\interactive\interactive_mode.rs:1409:60
           |
      1409 |         mode.hide_thinking_block = mode.settings_manager().get_hide_thinking_block();
           |                                                            ^^^^^^^^^^^^^^^^^^^^^^^ method not found in `&std::sync::Arc<interactive_mode_services::SettingsManager>`
    --- rendered ---
      error[E0596]: cannot borrow data in an `Arc` as mutable
          --> crates\pi-coding-agent\src\modes\interactive\interactive_mode.rs:1468:22
           |
      1468 |         let handle = store.for_session(session_id);
           |                      ^^^^^ cannot borrow as mutable
           |
           = help: trait `DerefMut` is required to modify through a dereference, but it is not implemented for `std::sync::Arc<ClientPromptStashStore>`
    --- rendered ---
      error[E0596]: cannot borrow data in an `Arc` as mutable
          --> crates\pi-coding-agent\src\modes\interactive\interactive_mode.rs:1498:13
           |
      1498 |             store.release(&session_id, handle);
           |             ^^^^^ cannot borrow as mutable
           |
           = help: trait `DerefMut` is required to modify through a dereference, but it is not implemented for `std::sync::Arc<ClientPromptStashStore>`

### modes/interactive/components/tree_selector.rs  (18 errors)
  L1099    [E0599] no method named `is_empty` found for reference `&std::option::Option<std::option::Option<std::string::String>>` in the current scope
  L1102    [E0599] no method named `as_str` found for reference `&std::option::Option<std::option::Option<std::string::String>>` in the current scope
  L1544    [E0308] mismatched types
  L1545    [E0308] mismatched types
  L1633    [E0308] mismatched types
  L1634    [E0308] mismatched types
  L1635    [E0308] mismatched types
  L1636    [E0308] mismatched types
  L1637    [E0308] mismatched types
  L1642    [E0308] mismatched types
  L1643    [E0308] mismatched types
  L1650    [E0308] mismatched types
  L1651    [E0308] mismatched types
  L1738    [E0502] cannot borrow `*self` as mutable because it is also borrowed as immutable
  L1740    [E0502] cannot borrow `*self` as mutable because it is also borrowed as immutable
  L2122    [E0599] no method named `is_empty` found for reference `&std::option::Option<std::option::Option<std::string::String>>` in the current scope
  L2125    [E0308] mismatched types
  L2127    [E0277] can't compare `std::option::Option<std::option::Option<std::string::String>>` with `str`
    --- rendered ---
      error[E0599]: no method named `is_empty` found for reference `&std::option::Option<std::option::Option<std::string::String>>` in the current scope
          --> crates\pi-coding-agent\src\modes\interactive\components\tree_selector.rs:1099:60
           |
      1099 |                 let service_tier_display = if service_tier.is_empty() {
           |                                                            ^^^^^^^^ method not found in `&std::option::Option<std::option::Option<std::string::String>>`
    --- rendered ---
      error[E0599]: no method named `as_str` found for reference `&std::option::Option<std::option::Option<std::string::String>>` in the current scope
          --> crates\pi-coding-agent\src\modes\interactive\components\tree_selector.rs:1102:34
           |
      1102 |                     service_tier.as_str()
           |                                  ^^^^^^ method not found in `&std::option::Option<std::option::Option<std::string::String>>`
    --- rendered ---
      error[E0308]: mismatched types
          --> crates\pi-coding-agent\src\modes\interactive\components\tree_selector.rs:1544:56
           |
      1544 |                 key_hint("tui.select.confirm", "save", KeyTextOptions::default()),
           |                 --------                               ^^^^^^^^^^^^^^^^^^^^^^^^^ expected `&KeyTextOptions`, found `KeyTextOptions`
           |                 |
           |                 arguments to this function are incorrect
           |
      note: function defined here
          --> crates\pi-coding-agent\src\modes\interactive\components\keybinding_hints.rs:101:8
           |
       101 | pub fn key_hint(keybinding: &str, description: &str, options: &KeyTextOptions) -> String {
           |        ^^^^^^^^                                      ------------------------
      help: consider borrowing here
    --- rendered ---
      error[E0308]: mismatched types
          --> crates\pi-coding-agent\src\modes\interactive\components\tree_selector.rs:1545:57
           |
      1545 |                 key_hint("tui.select.cancel", "cancel", KeyTextOptions::default())
           |                 --------                                ^^^^^^^^^^^^^^^^^^^^^^^^^ expected `&KeyTextOptions`, found `KeyTextOptions`
           |                 |
           |                 arguments to this function are incorrect
           |
      note: function defined here
          --> crates\pi-coding-agent\src\modes\interactive\components\keybinding_hints.rs:101:8
           |
       101 | pub fn key_hint(keybinding: &str, description: &str, options: &KeyTextOptions) -> String {
           |        ^^^^^^^^                                      ------------------------
      help: consider borrowing here
    --- rendered ---
      error[E0308]: mismatched types
          --> crates\pi-coding-agent\src\modes\interactive\components\tree_selector.rs:1633:49
           |
      1633 |             key_text("app.tree.filter.default", false),
           |             --------                            ^^^^^ expected `&KeyTextOptions`, found `bool`
           |             |
           |             arguments to this function are incorrect
           |
      note: function defined here
          --> crates\pi-coding-agent\src\modes\interactive\components\keybinding_hints.rs:96:8
           |
        96 | pub fn key_text(keybinding: &str, options: &KeyTextOptions) -> String {
           |        ^^^^^^^^                   ------------------------
    --- rendered ---
      error[E0308]: mismatched types
          --> crates\pi-coding-agent\src\modes\interactive\components\tree_selector.rs:1634:49
           |
      1634 |             key_text("app.tree.filter.noTools", false),
           |             --------                            ^^^^^ expected `&KeyTextOptions`, found `bool`
           |             |
           |             arguments to this function are incorrect
           |
      note: function defined here
          --> crates\pi-coding-agent\src\modes\interactive\components\keybinding_hints.rs:96:8
           |
        96 | pub fn key_text(keybinding: &str, options: &KeyTextOptions) -> String {
           |        ^^^^^^^^                   ------------------------

### modes/interactive/components/assistant_message.rs  (16 errors)
  L63      [E0308] mismatched types
  L64      [E0308] mismatched types
  L65      [E0308] mismatched types
  L66      [E0308] mismatched types
  L67      [E0308] mismatched types
  L68      [E0308] mismatched types
  L69      [E0308] mismatched types
  L70      [E0308] mismatched types
  L71      [E0308] mismatched types
  L72      [E0308] mismatched types
  L73      [E0308] mismatched types
  L74      [E0308] mismatched types
  L75      [E0308] mismatched types
  L76      [E0308] mismatched types
  L81      [E0308] mismatched types
  L82      [E0308] mismatched types
    --- rendered ---
      error[E0308]: mismatched types
        --> crates\pi-coding-agent\src\modes\interactive\components\assistant_message.rs:63:21
         |
      63 |         heading: rc(theme_source.heading),
         |                  -- ^^^^^^^^^^^^^^^^^^^^ expected `Box<dyn Fn(&str) -> ... + Send + Sync>`, found `Arc<dyn Fn(&str) -> ... + Send + Sync>`
         |                  |
         |                  arguments to this function are incorrect
         |
         = note: expected struct `Box<(dyn for<'a> Fn(&'a str) -> std::string::String + std::marker::Send + Sync + 'static)>`
                    found struct `std::sync::Arc<(dyn for<'a> Fn(&'a str) -> std::string::String + std::marker::Send + Sync + 'static)>`
      note: function defined here
        --> crates\pi-coding-agent\src\modes\interactive\components\assistant_message.rs:58:8
         |
      58 |     fn rc(value: Box<dyn Fn(&str) -> String + Send + Sync>) -> Rc<dyn Fn(&str) -> String> {
    --- rendered ---
      error[E0308]: mismatched types
        --> crates\pi-coding-agent\src\modes\interactive\components\assistant_message.rs:64:18
         |
      64 |         link: rc(theme_source.link),
         |               -- ^^^^^^^^^^^^^^^^^ expected `Box<dyn Fn(&str) -> ... + Send + Sync>`, found `Arc<dyn Fn(&str) -> ... + Send + Sync>`
         |               |
         |               arguments to this function are incorrect
         |
         = note: expected struct `Box<(dyn for<'a> Fn(&'a str) -> std::string::String + std::marker::Send + Sync + 'static)>`
                    found struct `std::sync::Arc<(dyn for<'a> Fn(&'a str) -> std::string::String + std::marker::Send + Sync + 'static)>`
      note: function defined here
        --> crates\pi-coding-agent\src\modes\interactive\components\assistant_message.rs:58:8
         |
      58 |     fn rc(value: Box<dyn Fn(&str) -> String + Send + Sync>) -> Rc<dyn Fn(&str) -> String> {
    --- rendered ---
      error[E0308]: mismatched types
        --> crates\pi-coding-agent\src\modes\interactive\components\assistant_message.rs:65:22
         |
      65 |         link_url: rc(theme_source.link_url),
         |                   -- ^^^^^^^^^^^^^^^^^^^^^ expected `Box<dyn Fn(&str) -> ... + Send + Sync>`, found `Arc<dyn Fn(&str) -> ... + Send + Sync>`
         |                   |
         |                   arguments to this function are incorrect
         |
         = note: expected struct `Box<(dyn for<'a> Fn(&'a str) -> std::string::String + std::marker::Send + Sync + 'static)>`
                    found struct `std::sync::Arc<(dyn for<'a> Fn(&'a str) -> std::string::String + std::marker::Send + Sync + 'static)>`
      note: function defined here
        --> crates\pi-coding-agent\src\modes\interactive\components\assistant_message.rs:58:8
         |
      58 |     fn rc(value: Box<dyn Fn(&str) -> String + Send + Sync>) -> Rc<dyn Fn(&str) -> String> {
    --- rendered ---
      error[E0308]: mismatched types
        --> crates\pi-coding-agent\src\modes\interactive\components\assistant_message.rs:66:18
         |
      66 |         code: rc(theme_source.code),
         |               -- ^^^^^^^^^^^^^^^^^ expected `Box<dyn Fn(&str) -> ... + Send + Sync>`, found `Arc<dyn Fn(&str) -> ... + Send + Sync>`
         |               |
         |               arguments to this function are incorrect
         |
         = note: expected struct `Box<(dyn for<'a> Fn(&'a str) -> std::string::String + std::marker::Send + Sync + 'static)>`
                    found struct `std::sync::Arc<(dyn for<'a> Fn(&'a str) -> std::string::String + std::marker::Send + Sync + 'static)>`
      note: function defined here
        --> crates\pi-coding-agent\src\modes\interactive\components\assistant_message.rs:58:8
         |
      58 |     fn rc(value: Box<dyn Fn(&str) -> String + Send + Sync>) -> Rc<dyn Fn(&str) -> String> {
    --- rendered ---
      error[E0308]: mismatched types
        --> crates\pi-coding-agent\src\modes\interactive\components\assistant_message.rs:67:24
         |
      67 |         code_block: rc(theme_source.code_block),
         |                     -- ^^^^^^^^^^^^^^^^^^^^^^^ expected `Box<dyn Fn(&str) -> ... + Send + Sync>`, found `Arc<dyn Fn(&str) -> ... + Send + Sync>`
         |                     |
         |                     arguments to this function are incorrect
         |
         = note: expected struct `Box<(dyn for<'a> Fn(&'a str) -> std::string::String + std::marker::Send + Sync + 'static)>`
                    found struct `std::sync::Arc<(dyn for<'a> Fn(&'a str) -> std::string::String + std::marker::Send + Sync + 'static)>`
      note: function defined here
        --> crates\pi-coding-agent\src\modes\interactive\components\assistant_message.rs:58:8
         |
      58 |     fn rc(value: Box<dyn Fn(&str) -> String + Send + Sync>) -> Rc<dyn Fn(&str) -> String> {
    --- rendered ---
      error[E0308]: mismatched types
        --> crates\pi-coding-agent\src\modes\interactive\components\assistant_message.rs:68:31
         |
      68 |         code_block_border: rc(theme_source.code_block_border),
         |                            -- ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ expected `Box<dyn Fn(&str) -> ... + Send + Sync>`, found `Arc<dyn Fn(&str) -> ... + Send + Sync>`
         |                            |
         |                            arguments to this function are incorrect
         |
         = note: expected struct `Box<(dyn for<'a> Fn(&'a str) -> std::string::String + std::marker::Send + Sync + 'static)>`
                    found struct `std::sync::Arc<(dyn for<'a> Fn(&'a str) -> std::string::String + std::marker::Send + Sync + 'static)>`
      note: function defined here
        --> crates\pi-coding-agent\src\modes\interactive\components\assistant_message.rs:58:8
         |
      58 |     fn rc(value: Box<dyn Fn(&str) -> String + Send + Sync>) -> Rc<dyn Fn(&str) -> String> {

### modes/interactive/components/branch_summary_message.rs  (16 errors)
  L27      [E0308] mismatched types
  L28      [E0308] mismatched types
  L29      [E0308] mismatched types
  L30      [E0308] mismatched types
  L31      [E0308] mismatched types
  L32      [E0308] mismatched types
  L33      [E0308] mismatched types
  L34      [E0308] mismatched types
  L35      [E0308] mismatched types
  L36      [E0308] mismatched types
  L37      [E0308] mismatched types
  L38      [E0308] mismatched types
  L39      [E0308] mismatched types
  L40      [E0308] mismatched types
  L45      [E0308] mismatched types
  L46      [E0308] mismatched types
    --- rendered ---
      error[E0308]: mismatched types
        --> crates\pi-coding-agent\src\modes\interactive\components\branch_summary_message.rs:27:21
         |
      27 |         heading: rc(source.heading),
         |                  -- ^^^^^^^^^^^^^^ expected `Box<dyn Fn(&str) -> ... + Send + Sync>`, found `Arc<dyn Fn(&str) -> ... + Send + Sync>`
         |                  |
         |                  arguments to this function are incorrect
         |
         = note: expected struct `Box<(dyn for<'a> Fn(&'a str) -> std::string::String + std::marker::Send + Sync + 'static)>`
                    found struct `std::sync::Arc<(dyn for<'a> Fn(&'a str) -> std::string::String + std::marker::Send + Sync + 'static)>`
      note: function defined here
        --> crates\pi-coding-agent\src\modes\interactive\components\branch_summary_message.rs:22:8
         |
      22 |     fn rc(value: Box<dyn Fn(&str) -> String + Send + Sync>) -> Rc<dyn Fn(&str) -> String> {
    --- rendered ---
      error[E0308]: mismatched types
        --> crates\pi-coding-agent\src\modes\interactive\components\branch_summary_message.rs:28:18
         |
      28 |         link: rc(source.link),
         |               -- ^^^^^^^^^^^ expected `Box<dyn Fn(&str) -> ... + Send + Sync>`, found `Arc<dyn Fn(&str) -> ... + Send + Sync>`
         |               |
         |               arguments to this function are incorrect
         |
         = note: expected struct `Box<(dyn for<'a> Fn(&'a str) -> std::string::String + std::marker::Send + Sync + 'static)>`
                    found struct `std::sync::Arc<(dyn for<'a> Fn(&'a str) -> std::string::String + std::marker::Send + Sync + 'static)>`
      note: function defined here
        --> crates\pi-coding-agent\src\modes\interactive\components\branch_summary_message.rs:22:8
         |
      22 |     fn rc(value: Box<dyn Fn(&str) -> String + Send + Sync>) -> Rc<dyn Fn(&str) -> String> {
    --- rendered ---
      error[E0308]: mismatched types
        --> crates\pi-coding-agent\src\modes\interactive\components\branch_summary_message.rs:29:22
         |
      29 |         link_url: rc(source.link_url),
         |                   -- ^^^^^^^^^^^^^^^ expected `Box<dyn Fn(&str) -> ... + Send + Sync>`, found `Arc<dyn Fn(&str) -> ... + Send + Sync>`
         |                   |
         |                   arguments to this function are incorrect
         |
         = note: expected struct `Box<(dyn for<'a> Fn(&'a str) -> std::string::String + std::marker::Send + Sync + 'static)>`
                    found struct `std::sync::Arc<(dyn for<'a> Fn(&'a str) -> std::string::String + std::marker::Send + Sync + 'static)>`
      note: function defined here
        --> crates\pi-coding-agent\src\modes\interactive\components\branch_summary_message.rs:22:8
         |
      22 |     fn rc(value: Box<dyn Fn(&str) -> String + Send + Sync>) -> Rc<dyn Fn(&str) -> String> {
    --- rendered ---
      error[E0308]: mismatched types
        --> crates\pi-coding-agent\src\modes\interactive\components\branch_summary_message.rs:30:18
         |
      30 |         code: rc(source.code),
         |               -- ^^^^^^^^^^^ expected `Box<dyn Fn(&str) -> ... + Send + Sync>`, found `Arc<dyn Fn(&str) -> ... + Send + Sync>`
         |               |
         |               arguments to this function are incorrect
         |
         = note: expected struct `Box<(dyn for<'a> Fn(&'a str) -> std::string::String + std::marker::Send + Sync + 'static)>`
                    found struct `std::sync::Arc<(dyn for<'a> Fn(&'a str) -> std::string::String + std::marker::Send + Sync + 'static)>`
      note: function defined here
        --> crates\pi-coding-agent\src\modes\interactive\components\branch_summary_message.rs:22:8
         |
      22 |     fn rc(value: Box<dyn Fn(&str) -> String + Send + Sync>) -> Rc<dyn Fn(&str) -> String> {
    --- rendered ---
      error[E0308]: mismatched types
        --> crates\pi-coding-agent\src\modes\interactive\components\branch_summary_message.rs:31:24
         |
      31 |         code_block: rc(source.code_block),
         |                     -- ^^^^^^^^^^^^^^^^^ expected `Box<dyn Fn(&str) -> ... + Send + Sync>`, found `Arc<dyn Fn(&str) -> ... + Send + Sync>`
         |                     |
         |                     arguments to this function are incorrect
         |
         = note: expected struct `Box<(dyn for<'a> Fn(&'a str) -> std::string::String + std::marker::Send + Sync + 'static)>`
                    found struct `std::sync::Arc<(dyn for<'a> Fn(&'a str) -> std::string::String + std::marker::Send + Sync + 'static)>`
      note: function defined here
        --> crates\pi-coding-agent\src\modes\interactive\components\branch_summary_message.rs:22:8
         |
      22 |     fn rc(value: Box<dyn Fn(&str) -> String + Send + Sync>) -> Rc<dyn Fn(&str) -> String> {
    --- rendered ---
      error[E0308]: mismatched types
        --> crates\pi-coding-agent\src\modes\interactive\components\branch_summary_message.rs:32:31
         |
      32 |         code_block_border: rc(source.code_block_border),
         |                            -- ^^^^^^^^^^^^^^^^^^^^^^^^ expected `Box<dyn Fn(&str) -> ... + Send + Sync>`, found `Arc<dyn Fn(&str) -> ... + Send + Sync>`
         |                            |
         |                            arguments to this function are incorrect
         |
         = note: expected struct `Box<(dyn for<'a> Fn(&'a str) -> std::string::String + std::marker::Send + Sync + 'static)>`
                    found struct `std::sync::Arc<(dyn for<'a> Fn(&'a str) -> std::string::String + std::marker::Send + Sync + 'static)>`
      note: function defined here
        --> crates\pi-coding-agent\src\modes\interactive\components\branch_summary_message.rs:22:8
         |
      22 |     fn rc(value: Box<dyn Fn(&str) -> String + Send + Sync>) -> Rc<dyn Fn(&str) -> String> {

### modes/interactive/components/injected_prompt_message.rs  (14 errors)
  L141     [E0308] mismatched types
  L142     [E0308] mismatched types
  L143     [E0308] mismatched types
  L144     [E0308] mismatched types
  L145     [E0308] mismatched types
  L146     [E0308] mismatched types
  L147     [E0308] mismatched types
  L148     [E0308] mismatched types
  L149     [E0308] mismatched types
  L150     [E0308] mismatched types
  L151     [E0308] mismatched types
  L152     [E0308] mismatched types
  L153     [E0308] mismatched types
  L154     [E0308] mismatched types
    --- rendered ---
      error[E0308]: mismatched types
         --> crates\pi-coding-agent\src\modes\interactive\components\injected_prompt_message.rs:141:21
          |
      141 |         heading: rc(theme.heading),
          |                  -- ^^^^^^^^^^^^^ expected `Box<dyn Fn(&str) -> ... + Send + Sync>`, found `Arc<dyn Fn(&str) -> ... + Send + Sync>`
          |                  |
          |                  arguments to this function are incorrect
          |
          = note: expected struct `Box<(dyn for<'a> Fn(&'a str) -> std::string::String + std::marker::Send + Sync + 'static)>`
                     found struct `std::sync::Arc<(dyn for<'a> Fn(&'a str) -> std::string::String + std::marker::Send + Sync + 'static)>`
      note: function defined here
         --> crates\pi-coding-agent\src\modes\interactive\components\injected_prompt_message.rs:136:8
          |
      136 |     fn rc(value: Box<dyn Fn(&str) -> String + Send + Sync>) -> Rc<dyn Fn(&str) -> String> {
    --- rendered ---
      error[E0308]: mismatched types
         --> crates\pi-coding-agent\src\modes\interactive\components\injected_prompt_message.rs:142:18
          |
      142 |         link: rc(theme.link),
          |               -- ^^^^^^^^^^ expected `Box<dyn Fn(&str) -> ... + Send + Sync>`, found `Arc<dyn Fn(&str) -> ... + Send + Sync>`
          |               |
          |               arguments to this function are incorrect
          |
          = note: expected struct `Box<(dyn for<'a> Fn(&'a str) -> std::string::String + std::marker::Send + Sync + 'static)>`
                     found struct `std::sync::Arc<(dyn for<'a> Fn(&'a str) -> std::string::String + std::marker::Send + Sync + 'static)>`
      note: function defined here
         --> crates\pi-coding-agent\src\modes\interactive\components\injected_prompt_message.rs:136:8
          |
      136 |     fn rc(value: Box<dyn Fn(&str) -> String + Send + Sync>) -> Rc<dyn Fn(&str) -> String> {
    --- rendered ---
      error[E0308]: mismatched types
         --> crates\pi-coding-agent\src\modes\interactive\components\injected_prompt_message.rs:143:22
          |
      143 |         link_url: rc(theme.link_url),
          |                   -- ^^^^^^^^^^^^^^ expected `Box<dyn Fn(&str) -> ... + Send + Sync>`, found `Arc<dyn Fn(&str) -> ... + Send + Sync>`
          |                   |
          |                   arguments to this function are incorrect
          |
          = note: expected struct `Box<(dyn for<'a> Fn(&'a str) -> std::string::String + std::marker::Send + Sync + 'static)>`
                     found struct `std::sync::Arc<(dyn for<'a> Fn(&'a str) -> std::string::String + std::marker::Send + Sync + 'static)>`
      note: function defined here
         --> crates\pi-coding-agent\src\modes\interactive\components\injected_prompt_message.rs:136:8
          |
      136 |     fn rc(value: Box<dyn Fn(&str) -> String + Send + Sync>) -> Rc<dyn Fn(&str) -> String> {
    --- rendered ---
      error[E0308]: mismatched types
         --> crates\pi-coding-agent\src\modes\interactive\components\injected_prompt_message.rs:144:18
          |
      144 |         code: rc(theme.code),
          |               -- ^^^^^^^^^^ expected `Box<dyn Fn(&str) -> ... + Send + Sync>`, found `Arc<dyn Fn(&str) -> ... + Send + Sync>`
          |               |
          |               arguments to this function are incorrect
          |
          = note: expected struct `Box<(dyn for<'a> Fn(&'a str) -> std::string::String + std::marker::Send + Sync + 'static)>`
                     found struct `std::sync::Arc<(dyn for<'a> Fn(&'a str) -> std::string::String + std::marker::Send + Sync + 'static)>`
      note: function defined here
         --> crates\pi-coding-agent\src\modes\interactive\components\injected_prompt_message.rs:136:8
          |
      136 |     fn rc(value: Box<dyn Fn(&str) -> String + Send + Sync>) -> Rc<dyn Fn(&str) -> String> {
    --- rendered ---
      error[E0308]: mismatched types
         --> crates\pi-coding-agent\src\modes\interactive\components\injected_prompt_message.rs:145:24
          |
      145 |         code_block: rc(theme.code_block),
          |                     -- ^^^^^^^^^^^^^^^^ expected `Box<dyn Fn(&str) -> ... + Send + Sync>`, found `Arc<dyn Fn(&str) -> ... + Send + Sync>`
          |                     |
          |                     arguments to this function are incorrect
          |
          = note: expected struct `Box<(dyn for<'a> Fn(&'a str) -> std::string::String + std::marker::Send + Sync + 'static)>`
                     found struct `std::sync::Arc<(dyn for<'a> Fn(&'a str) -> std::string::String + std::marker::Send + Sync + 'static)>`
      note: function defined here
         --> crates\pi-coding-agent\src\modes\interactive\components\injected_prompt_message.rs:136:8
          |
      136 |     fn rc(value: Box<dyn Fn(&str) -> String + Send + Sync>) -> Rc<dyn Fn(&str) -> String> {
    --- rendered ---
      error[E0308]: mismatched types
         --> crates\pi-coding-agent\src\modes\interactive\components\injected_prompt_message.rs:146:31
          |
      146 |         code_block_border: rc(theme.code_block_border),
          |                            -- ^^^^^^^^^^^^^^^^^^^^^^^ expected `Box<dyn Fn(&str) -> ... + Send + Sync>`, found `Arc<dyn Fn(&str) -> ... + Send + Sync>`
          |                            |
          |                            arguments to this function are incorrect
          |
          = note: expected struct `Box<(dyn for<'a> Fn(&'a str) -> std::string::String + std::marker::Send + Sync + 'static)>`
                     found struct `std::sync::Arc<(dyn for<'a> Fn(&'a str) -> std::string::String + std::marker::Send + Sync + 'static)>`
      note: function defined here
         --> crates\pi-coding-agent\src\modes\interactive\components\injected_prompt_message.rs:136:8
          |
      136 |     fn rc(value: Box<dyn Fn(&str) -> String + Send + Sync>) -> Rc<dyn Fn(&str) -> String> {

### modes/interactive/components/side_question.rs  (14 errors)
  L24      [E0308] mismatched types
  L25      [E0308] mismatched types
  L26      [E0308] mismatched types
  L27      [E0308] mismatched types
  L28      [E0308] mismatched types
  L29      [E0308] mismatched types
  L30      [E0308] mismatched types
  L31      [E0308] mismatched types
  L32      [E0308] mismatched types
  L33      [E0308] mismatched types
  L34      [E0308] mismatched types
  L35      [E0308] mismatched types
  L36      [E0308] mismatched types
  L37      [E0308] mismatched types
    --- rendered ---
      error[E0308]: mismatched types
        --> crates\pi-coding-agent\src\modes\interactive\components\side_question.rs:24:21
         |
      24 |         heading: rc(theme.heading),
         |                  -- ^^^^^^^^^^^^^ expected `Box<dyn Fn(&str) -> ... + Send + Sync>`, found `Arc<dyn Fn(&str) -> ... + Send + Sync>`
         |                  |
         |                  arguments to this function are incorrect
         |
         = note: expected struct `Box<(dyn for<'a> Fn(&'a str) -> std::string::String + std::marker::Send + Sync + 'static)>`
                    found struct `std::sync::Arc<(dyn for<'a> Fn(&'a str) -> std::string::String + std::marker::Send + Sync + 'static)>`
      note: function defined here
        --> crates\pi-coding-agent\src\modes\interactive\components\side_question.rs:19:8
         |
      19 |     fn rc(value: Box<dyn Fn(&str) -> String + Send + Sync>) -> Rc<dyn Fn(&str) -> String> {
    --- rendered ---
      error[E0308]: mismatched types
        --> crates\pi-coding-agent\src\modes\interactive\components\side_question.rs:25:18
         |
      25 |         link: rc(theme.link),
         |               -- ^^^^^^^^^^ expected `Box<dyn Fn(&str) -> ... + Send + Sync>`, found `Arc<dyn Fn(&str) -> ... + Send + Sync>`
         |               |
         |               arguments to this function are incorrect
         |
         = note: expected struct `Box<(dyn for<'a> Fn(&'a str) -> std::string::String + std::marker::Send + Sync + 'static)>`
                    found struct `std::sync::Arc<(dyn for<'a> Fn(&'a str) -> std::string::String + std::marker::Send + Sync + 'static)>`
      note: function defined here
        --> crates\pi-coding-agent\src\modes\interactive\components\side_question.rs:19:8
         |
      19 |     fn rc(value: Box<dyn Fn(&str) -> String + Send + Sync>) -> Rc<dyn Fn(&str) -> String> {
    --- rendered ---
      error[E0308]: mismatched types
        --> crates\pi-coding-agent\src\modes\interactive\components\side_question.rs:26:22
         |
      26 |         link_url: rc(theme.link_url),
         |                   -- ^^^^^^^^^^^^^^ expected `Box<dyn Fn(&str) -> ... + Send + Sync>`, found `Arc<dyn Fn(&str) -> ... + Send + Sync>`
         |                   |
         |                   arguments to this function are incorrect
         |
         = note: expected struct `Box<(dyn for<'a> Fn(&'a str) -> std::string::String + std::marker::Send + Sync + 'static)>`
                    found struct `std::sync::Arc<(dyn for<'a> Fn(&'a str) -> std::string::String + std::marker::Send + Sync + 'static)>`
      note: function defined here
        --> crates\pi-coding-agent\src\modes\interactive\components\side_question.rs:19:8
         |
      19 |     fn rc(value: Box<dyn Fn(&str) -> String + Send + Sync>) -> Rc<dyn Fn(&str) -> String> {
    --- rendered ---
      error[E0308]: mismatched types
        --> crates\pi-coding-agent\src\modes\interactive\components\side_question.rs:27:18
         |
      27 |         code: rc(theme.code),
         |               -- ^^^^^^^^^^ expected `Box<dyn Fn(&str) -> ... + Send + Sync>`, found `Arc<dyn Fn(&str) -> ... + Send + Sync>`
         |               |
         |               arguments to this function are incorrect
         |
         = note: expected struct `Box<(dyn for<'a> Fn(&'a str) -> std::string::String + std::marker::Send + Sync + 'static)>`
                    found struct `std::sync::Arc<(dyn for<'a> Fn(&'a str) -> std::string::String + std::marker::Send + Sync + 'static)>`
      note: function defined here
        --> crates\pi-coding-agent\src\modes\interactive\components\side_question.rs:19:8
         |
      19 |     fn rc(value: Box<dyn Fn(&str) -> String + Send + Sync>) -> Rc<dyn Fn(&str) -> String> {
    --- rendered ---
      error[E0308]: mismatched types
        --> crates\pi-coding-agent\src\modes\interactive\components\side_question.rs:28:24
         |
      28 |         code_block: rc(theme.code_block),
         |                     -- ^^^^^^^^^^^^^^^^ expected `Box<dyn Fn(&str) -> ... + Send + Sync>`, found `Arc<dyn Fn(&str) -> ... + Send + Sync>`
         |                     |
         |                     arguments to this function are incorrect
         |
         = note: expected struct `Box<(dyn for<'a> Fn(&'a str) -> std::string::String + std::marker::Send + Sync + 'static)>`
                    found struct `std::sync::Arc<(dyn for<'a> Fn(&'a str) -> std::string::String + std::marker::Send + Sync + 'static)>`
      note: function defined here
        --> crates\pi-coding-agent\src\modes\interactive\components\side_question.rs:19:8
         |
      19 |     fn rc(value: Box<dyn Fn(&str) -> String + Send + Sync>) -> Rc<dyn Fn(&str) -> String> {
    --- rendered ---
      error[E0308]: mismatched types
        --> crates\pi-coding-agent\src\modes\interactive\components\side_question.rs:29:31
         |
      29 |         code_block_border: rc(theme.code_block_border),
         |                            -- ^^^^^^^^^^^^^^^^^^^^^^^ expected `Box<dyn Fn(&str) -> ... + Send + Sync>`, found `Arc<dyn Fn(&str) -> ... + Send + Sync>`
         |                            |
         |                            arguments to this function are incorrect
         |
         = note: expected struct `Box<(dyn for<'a> Fn(&'a str) -> std::string::String + std::marker::Send + Sync + 'static)>`
                    found struct `std::sync::Arc<(dyn for<'a> Fn(&'a str) -> std::string::String + std::marker::Send + Sync + 'static)>`
      note: function defined here
        --> crates\pi-coding-agent\src\modes\interactive\components\side_question.rs:19:8
         |
      19 |     fn rc(value: Box<dyn Fn(&str) -> String + Send + Sync>) -> Rc<dyn Fn(&str) -> String> {

### modes/interactive/auth_flows.rs  (12 errors)
  L529     [E0596] cannot borrow data in an `Arc` as mutable
  L559     [E0596] cannot borrow data in an `Arc` as mutable
  L647     [E0596] cannot borrow data in an `Arc` as mutable
  L651     [E0596] cannot borrow data in an `Arc` as mutable
  L663     [E0596] cannot borrow data in an `Arc` as mutable
  L668     [E0596] cannot borrow data in an `Arc` as mutable
  L681     [E0596] cannot borrow data in an `Arc` as mutable
  L703     [E0596] cannot borrow data in an `Arc` as mutable
  L726     [E0596] cannot borrow data in an `Arc` as mutable
  L824     [E0596] cannot borrow data in an `Arc` as mutable
  L847     [E0308] mismatched types
  L864     [E0308] mismatched types
    --- rendered ---
      error[E0596]: cannot borrow data in an `Arc` as mutable
         --> crates\pi-coding-agent\src\modes\interactive\auth_flows.rs:529:9
          |
      529 |         model_registry.refresh();
          |         ^^^^^^^^^^^^^^ cannot borrow as mutable
          |
          = help: trait `DerefMut` is required to modify through a dereference, but it is not implemented for `std::sync::Arc<interactive_mode_services::ModelRegistry>`
    --- rendered ---
      error[E0596]: cannot borrow data in an `Arc` as mutable
         --> crates\pi-coding-agent\src\modes\interactive\auth_flows.rs:559:9
          |
      559 |         model_registry.refresh();
          |         ^^^^^^^^^^^^^^ cannot borrow as mutable
          |
          = help: trait `DerefMut` is required to modify through a dereference, but it is not implemented for `std::sync::Arc<interactive_mode_services::ModelRegistry>`
    --- rendered ---
      error[E0596]: cannot borrow data in an `Arc` as mutable
         --> crates\pi-coding-agent\src\modes\interactive\auth_flows.rs:647:13
          |
      647 |             model_registry.reload();
          |             ^^^^^^^^^^^^^^ cannot borrow as mutable
          |
          = help: trait `DerefMut` is required to modify through a dereference, but it is not implemented for `std::sync::Arc<interactive_mode_services::ModelRegistry>`
    --- rendered ---
      error[E0596]: cannot borrow data in an `Arc` as mutable
         --> crates\pi-coding-agent\src\modes\interactive\auth_flows.rs:651:13
          |
      651 |             model_registry.reload();
          |             ^^^^^^^^^^^^^^ cannot borrow as mutable
          |
          = help: trait `DerefMut` is required to modify through a dereference, but it is not implemented for `std::sync::Arc<interactive_mode_services::ModelRegistry>`
    --- rendered ---
      error[E0596]: cannot borrow data in an `Arc` as mutable
         --> crates\pi-coding-agent\src\modes\interactive\auth_flows.rs:663:17
          |
      663 |                 model_registry.reload();
          |                 ^^^^^^^^^^^^^^ cannot borrow as mutable
          |
          = help: trait `DerefMut` is required to modify through a dereference, but it is not implemented for `std::sync::Arc<interactive_mode_services::ModelRegistry>`
    --- rendered ---
      error[E0596]: cannot borrow data in an `Arc` as mutable
         --> crates\pi-coding-agent\src\modes\interactive\auth_flows.rs:668:13
          |
      668 |             model_registry.set_prime_inference_team_selection(None);
          |             ^^^^^^^^^^^^^^ cannot borrow as mutable
          |
          = help: trait `DerefMut` is required to modify through a dereference, but it is not implemented for `std::sync::Arc<interactive_mode_services::ModelRegistry>`

### modes/interactive/components/extension_editor.rs  (6 errors)
  L76      [E0308] mismatched types
  L95      [E0308] mismatched types
  L98      [E0308] mismatched types
  L102     [E0308] mismatched types
  L110     [E0308] mismatched types
  L148     [E0277] the trait bound `RefMut<'_, Editor>: pi_tui::r#mod::Component` is not satisfied
    --- rendered ---
      error[E0308]: mismatched types
          --> crates\pi-coding-agent\src\modes\interactive\components\extension_editor.rs:76:55
           |
        76 |         let mut editor = Editor::new(Rc::clone(&tui), get_editor_theme(), options);
           |                          -----------                  ^^^^^^^^^^^^^^^^^^ expected `pi_tui::r#mod::EditorTheme`, found `interactive::theme::theme::EditorTheme`
           |                          |
           |                          arguments to this function are incorrect
           |
           = note: `interactive::theme::theme::EditorTheme` and `pi_tui::r#mod::EditorTheme` have similar names, but are actually distinct types
      note: `interactive::theme::theme::EditorTheme` is defined in the current crate
          --> crates\pi-coding-agent\src\modes\interactive\theme\theme.rs:1731:1
           |
      1731 | pub struct EditorTheme {
           | ^^^^^^^^^^^^^^^^^^^^^^
    --- rendered ---
      error[E0308]: mismatched types
         --> crates\pi-coding-agent\src\modes\interactive\components\extension_editor.rs:95:54
          |
       95 |             key_hint("tui.select.confirm", "submit", KeyTextOptions::default()),
          |             --------                                 ^^^^^^^^^^^^^^^^^^^^^^^^^ expected `&KeyTextOptions`, found `KeyTextOptions`
          |             |
          |             arguments to this function are incorrect
          |
      note: function defined here
         --> crates\pi-coding-agent\src\modes\interactive\components\keybinding_hints.rs:101:8
          |
      101 | pub fn key_hint(keybinding: &str, description: &str, options: &KeyTextOptions) -> String {
          |        ^^^^^^^^                                      ------------------------
      help: consider borrowing here
    --- rendered ---
      error[E0308]: mismatched types
         --> crates\pi-coding-agent\src\modes\interactive\components\extension_editor.rs:98:58
          |
       98 |                 key_hint("tui.input.newLine", "newline", KeyTextOptions::default())
          |                 --------                                 ^^^^^^^^^^^^^^^^^^^^^^^^^ expected `&KeyTextOptions`, found `KeyTextOptions`
          |                 |
          |                 arguments to this function are incorrect
          |
      note: function defined here
         --> crates\pi-coding-agent\src\modes\interactive\components\keybinding_hints.rs:101:8
          |
      101 | pub fn key_hint(keybinding: &str, description: &str, options: &KeyTextOptions) -> String {
          |        ^^^^^^^^                                      ------------------------
      help: consider borrowing here
    --- rendered ---
      error[E0308]: mismatched types
         --> crates\pi-coding-agent\src\modes\interactive\components\extension_editor.rs:102:57
          |
      102 |                 key_hint("tui.select.cancel", "cancel", KeyTextOptions::default())
          |                 --------                                ^^^^^^^^^^^^^^^^^^^^^^^^^ expected `&KeyTextOptions`, found `KeyTextOptions`
          |                 |
          |                 arguments to this function are incorrect
          |
      note: function defined here
         --> crates\pi-coding-agent\src\modes\interactive\components\keybinding_hints.rs:101:8
          |
      101 | pub fn key_hint(keybinding: &str, description: &str, options: &KeyTextOptions) -> String {
          |        ^^^^^^^^                                      ------------------------
      help: consider borrowing here
    --- rendered ---
      error[E0308]: mismatched types
         --> crates\pi-coding-agent\src\modes\interactive\components\extension_editor.rs:110:25
          |
      107 |                     key_hint(
          |                     -------- arguments to this function are incorrect
      ...
      110 |                         KeyTextOptions::default()
          |                         ^^^^^^^^^^^^^^^^^^^^^^^^^ expected `&KeyTextOptions`, found `KeyTextOptions`
          |
      note: function defined here
         --> crates\pi-coding-agent\src\modes\interactive\components\keybinding_hints.rs:101:8
          |
      101 | pub fn key_hint(keybinding: &str, description: &str, options: &KeyTextOptions) -> String {
          |        ^^^^^^^^                                      ------------------------
    --- rendered ---
      error[E0277]: the trait bound `RefMut<'_, Editor>: pi_tui::r#mod::Component` is not satisfied
         --> crates\pi-coding-agent\src\modes\interactive\components\extension_editor.rs:148:33
          |
      148 |         Component::handle_input(&mut self.editor.borrow_mut(), key_data);
          |         ----------------------- ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ the trait `pi_tui::r#mod::Component` is not implemented for `RefMut<'_, Editor>`
          |         |
          |         required by a bound introduced by this call
          |
          = help: the following other types implement trait `pi_tui::r#mod::Component`:
                    AgentMessageComponent
                    ArminComponent
                    AssistantMessageComponent
                    BashExecutionComponent
                    BorderedLoader

### modes/interactive/components/oauth_selector.rs  (5 errors)
  L207     [E0308] mismatched types
  L224     [E0277] the trait bound `oauth_selector::AuthSelectorCategory: Hash` is not satisfied
  L230     [E0599] the method `contains` exists for struct `std::collections::HashSet<oauth_selector::AuthSelectorCategory>`, but its trait bounds were not satisfied
  L459     [E0599] no method named `handle_input` found for struct `Input` in the current scope
  L477     [E0308] mismatched types
    --- rendered ---
      error[E0308]: mismatched types
         --> crates\pi-coding-agent\src\modes\interactive\components\oauth_selector.rs:207:54
          |
      207 |           component.list_layout = get_menu_list_layout(&MenuListLayoutOptions {
          |  _________________________________--------------------_^
          | |                                 |
          | |                                 arguments to this function are incorrect
      208 | |             preferred_visible_items: PREFERRED_VISIBLE_PROVIDERS,
      209 | |             reserved_rows: PROVIDER_LIST_RESERVED_ROWS,
      210 | |             comfortable_item_rows: 3,
      211 | |             compact_item_rows: Some(2),
      212 | |             ..Default::default()
      213 | |         });
          | |_________^ expected `MenuListLayoutOptions`, found `&MenuListLayoutOptions`
    --- rendered ---
      error[E0277]: the trait bound `oauth_selector::AuthSelectorCategory: Hash` is not satisfied
         --> crates\pi-coding-agent\src\modes\interactive\components\oauth_selector.rs:224:22
          |
      224 |                     .collect();
          |                      ^^^^^^^ the trait `Hash` is not implemented for `oauth_selector::AuthSelectorCategory`
          |
      help: the trait `FromIterator<T>` is conditionally implemented for `std::collections::HashSet<T, S>`
         --> /rustc/48a229ceaefd4985c50990b14116b6d856af0985/library\std\src\collections\hash\set.rs:1182:0
         ::: /rustc/48a229ceaefd4985c50990b14116b6d856af0985/library\std\src\collections\hash\set.rs:1184:12
          |
          = note: unsatisfied requirement introduced here: `oauth_selector::AuthSelectorCategory: Hash`
          = note: required for `std::collections::HashSet<oauth_selector::AuthSelectorCategory>` to implement `FromIterator<oauth_selector::AuthSelectorCategory>`
      note: required by a bound in `std::iter::Iterator::collect`
         --> /rustc/48a229ceaefd4985c50990b14116b6d856af0985/library\core\src\iter\traits\iterator.rs:2077:4
    --- rendered ---
      error[E0599]: the method `contains` exists for struct `std::collections::HashSet<oauth_selector::AuthSelectorCategory>`, but its trait bounds were not satisfied
         --> crates\pi-coding-agent\src\modes\interactive\components\oauth_selector.rs:230:37
          |
       19 | pub enum AuthSelectorCategory {
          | ----------------------------- doesn't satisfy `oauth_selector::AuthSelectorCategory: Hash`
      ...
      230 |                 .filter(|c| present.contains(c))
          |                                     ^^^^^^^^ method cannot be called due to unsatisfied trait bounds
          |
          = note: the following trait bounds were not satisfied:
                  `oauth_selector::AuthSelectorCategory: Hash`
      help: consider annotating `oauth_selector::AuthSelectorCategory` with `#[derive(Hash)]`
          |
       19 + #[derive(Hash)]
    --- rendered ---
      error[E0599]: no method named `handle_input` found for struct `Input` in the current scope
         --> crates\pi-coding-agent\src\modes\interactive\components\oauth_selector.rs:459:31
          |
      459 |             self.search_input.handle_input(key_data);
          |                               ^^^^^^^^^^^^ method not found in `Input`
          |
         ::: crates\pi-tui\src\tui.rs:70:8
          |
       70 |     fn handle_input(&mut self, data: &str) {
          |        ------------ the method is available for `Input` here
          |
          = help: items from traits can only be used if the trait is in scope
      help: trait `Component` which provides `handle_input` is implemented but not in scope; perhaps you want to import it
          |
    --- rendered ---
      error[E0308]: mismatched types
         --> crates\pi-coding-agent\src\modes\interactive\components\oauth_selector.rs:477:49
          |
      477 |           self.list_layout = get_menu_list_layout(&MenuListLayoutOptions {
          |  ____________________________--------------------_^
          | |                            |
          | |                            arguments to this function are incorrect
      478 | |             get_rows: self.viewport.get_rows.clone(),
      479 | |             preferred_visible_items: PREFERRED_VISIBLE_PROVIDERS,
      480 | |             total_items: Some(self.filtered_providers.len()),
      ...   |
      485 | |             ..Default::default()
      486 | |         });
          | |_________^ expected `MenuListLayoutOptions`, found `&MenuListLayoutOptions`

### modes/interactive/components/heartbeat_manager.rs  (4 errors)
  L323     [E0308] mismatched types
  L580     [E0308] mismatched types
  L587     [E0308] mismatched types
  L591     [E0308] mismatched types
    --- rendered ---
      error[E0308]: mismatched types
         --> crates\pi-coding-agent\src\modes\interactive\components\heartbeat_manager.rs:323:33
          |
      323 |         let job = heartbeat_job(heartbeat);
          |                   ------------- ^^^^^^^^^ expected `&AgentConnectionHeartbeat`, found `AgentConnectionHeartbeat`
          |                   |
          |                   arguments to this function are incorrect
          |
      note: function defined here
         --> crates\pi-coding-agent\src\modes\interactive\components\heartbeat_manager.rs:48:4
          |
       48 | fn heartbeat_job(heartbeat: &AgentConnectionHeartbeat) -> AgentCronJob {
          |    ^^^^^^^^^^^^^ ------------------------------------
      help: consider borrowing here
    --- rendered ---
      error[E0308]: mismatched types
         --> crates\pi-coding-agent\src\modes\interactive\components\heartbeat_manager.rs:580:13
          |
      577 |         key_hint(
          |         -------- arguments to this function are incorrect
      ...
      580 |             KeyTextOptions { primary_only: true },
          |             ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ expected `&KeyTextOptions`, found `KeyTextOptions`
          |
      note: function defined here
         --> crates\pi-coding-agent\src\modes\interactive\components\keybinding_hints.rs:101:8
          |
      101 | pub fn key_hint(keybinding: &str, description: &str, options: &KeyTextOptions) -> String {
          |        ^^^^^^^^                                      ------------------------
    --- rendered ---
      error[E0308]: mismatched types
         --> crates\pi-coding-agent\src\modes\interactive\components\heartbeat_manager.rs:587:48
          |
      587 |             key_hint("app.modal.back", "back", KeyTextOptions::default()),
          |             --------                           ^^^^^^^^^^^^^^^^^^^^^^^^^ expected `&KeyTextOptions`, found `KeyTextOptions`
          |             |
          |             arguments to this function are incorrect
          |
      note: function defined here
         --> crates\pi-coding-agent\src\modes\interactive\components\keybinding_hints.rs:101:8
          |
      101 | pub fn key_hint(keybinding: &str, description: &str, options: &KeyTextOptions) -> String {
          |        ^^^^^^^^                                      ------------------------
      help: consider borrowing here
    --- rendered ---
      error[E0308]: mismatched types
         --> crates\pi-coding-agent\src\modes\interactive\components\heartbeat_manager.rs:591:17
          |
      588 |             key_hint(
          |             -------- arguments to this function are incorrect
      ...
      591 |                 KeyTextOptions { primary_only: true }
          |                 ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ expected `&KeyTextOptions`, found `KeyTextOptions`
          |
      note: function defined here
         --> crates\pi-coding-agent\src\modes\interactive\components\keybinding_hints.rs:101:8
          |
      101 | pub fn key_hint(keybinding: &str, description: &str, options: &KeyTextOptions) -> String {
          |        ^^^^^^^^                                      ------------------------

### modes/interactive/components/model_selector.rs  (4 errors)
  L371     [E0308] mismatched types
  L733     [E0308] mismatched types
  L739     [E0599] no method named `handle_input` found for struct `Input` in the current scope
  L785     [E0308] mismatched types
    --- rendered ---
      error[E0308]: mismatched types
         --> crates\pi-coding-agent\src\modes\interactive\components\model_selector.rs:371:54
          |
      371 |           component.list_layout = get_menu_list_layout(&MenuListLayoutOptions {
          |  _________________________________--------------------_^
          | |                                 |
          | |                                 arguments to this function are incorrect
      372 | |             preferred_visible_items: PREFERRED_VISIBLE_MODELS,
      373 | |             reserved_rows: MODEL_LIST_RESERVED_ROWS_BASE,
      374 | |             comfortable_item_rows: 3,
      375 | |             compact_item_rows: Some(2),
      376 | |             ..Default::default()
      377 | |         });
          | |_________^ expected `MenuListLayoutOptions`, found `&MenuListLayoutOptions`
    --- rendered ---
      error[E0308]: mismatched types
         --> crates\pi-coding-agent\src\modes\interactive\components\model_selector.rs:733:52
          |
      733 |             || should_treat_as_back(key_data, Some(self.get_cursor()))
          |                                               ---- ^^^^^^^^^^^^^^^^^ expected `&dyn BackGuardInput`, found `usize`
          |                                               |
          |                                               arguments to this enum variant are incorrect
          |
          = note: expected reference `&dyn BackGuardInput`
                          found type `usize`
      help: the type constructed contains `usize` due to the type of the argument passed
         --> crates\pi-coding-agent\src\modes\interactive\components\model_selector.rs:733:47
          |
      733 |             || should_treat_as_back(key_data, Some(self.get_cursor()))
    --- rendered ---
      error[E0599]: no method named `handle_input` found for struct `Input` in the current scope
         --> crates\pi-coding-agent\src\modes\interactive\components\model_selector.rs:739:31
          |
      739 |             self.search_input.handle_input(key_data);
          |                               ^^^^^^^^^^^^ method not found in `Input`
          |
         ::: crates\pi-tui\src\tui.rs:70:8
          |
       70 |     fn handle_input(&mut self, data: &str) {
          |        ------------ the method is available for `Input` here
          |
          = help: items from traits can only be used if the trait is in scope
      help: trait `Component` which provides `handle_input` is implemented but not in scope; perhaps you want to import it
          |
    --- rendered ---
      error[E0308]: mismatched types
         --> crates\pi-coding-agent\src\modes\interactive\components\model_selector.rs:785:49
          |
      785 |           self.list_layout = get_menu_list_layout(&MenuListLayoutOptions {
          |  ____________________________--------------------_^
          | |                            |
          | |                            arguments to this function are incorrect
      786 | |             get_rows: self.viewport.get_rows.clone(),
      787 | |             preferred_visible_items: PREFERRED_VISIBLE_MODELS,
      788 | |             total_items: Some(self.filtered_models.len()),
      ...   |
      793 | |             ..Default::default()
      794 | |         });
          | |_________^ expected `MenuListLayoutOptions`, found `&MenuListLayoutOptions`