//! Native mounting and callbacks for TypeScript `showSettingsSelector`.
use super::*;
use crate::core::session_action_store::IdleEvictionMinutes;
use crate::core::settings_manager::WarningSettings;
use crate::modes::interactive::components::settings_selector::{
    SettingsCallbacks, SettingsConfig, SettingsSelectorComponent,
};
use crate::modes::interactive::theme::theme::{get_available_themes, set_theme};

pub(super) enum Change {
    AutoCompact(bool),
    IdleEvictionMinutes(IdleEvictionMinutes),
    ShowImages(bool),
    AutoResizeImages(bool),
    BlockImages(bool),
    EnableSkillCommands(bool),
    EnableBuiltinSkills(bool),
    SteeringMode(String),
    FollowUpMode(String),
    Transport(String),
    ThinkingLevel(String),
    Theme(String),
    HideThinkingBlock(bool),
    MermaidRenderingMode(String),
    TreeFilterMode(String),
    ShowHardwareCursor(bool),
    EditorPaddingX(f64),
    AutocompleteMaxVisible(f64),
    QuietStartup(bool),
    ClearOnShrink(bool),
    ShowTerminalProgress(bool),
    Fullscreen(bool),
    Warnings(WarningSettings),
    ThemePreview(String),
    Close,
}

pub(super) fn create(
    mode: &InteractiveMode,
    state: &wire::AgentConnectionState,
    send: &mpsc::Sender<HostEvent>,
) -> Result<SettingsSelectorComponent, String> {
    let manager = mode.settings_manager();
    let settings = manager.lock().map_err(|e| e.to_string())?;
    let config = SettingsConfig {
        idle_eviction_minutes: match settings.get_idle_eviction_minutes() {
            crate::core::settings_manager::IdleEvictionMinutes::Minutes(n) => {
                IdleEvictionMinutes::Minutes(n)
            }
            crate::core::settings_manager::IdleEvictionMinutes::Off => IdleEvictionMinutes::Off,
        },
        show_images: settings.get_show_images(),
        auto_resize_images: settings.get_image_auto_resize(),
        block_images: settings.get_block_images(),
        enable_skill_commands: settings.get_enable_skill_commands(),
        enable_builtin_skills: settings.get_enable_builtin_skills(),
        transport: settings.get_transport(),
        mermaid_rendering_mode: settings.get_mermaid_rendering_mode(),
        tree_filter_mode: settings.get_tree_filter_mode(),
        show_hardware_cursor: settings.get_show_hardware_cursor(),
        editor_padding_x: settings.get_editor_padding_x(),
        autocomplete_max_visible: settings.get_autocomplete_max_visible(),
        quiet_startup: settings.get_quiet_startup(),
        clear_on_shrink: settings.get_clear_on_shrink(),
        show_terminal_progress: settings.get_show_terminal_progress(),
        warnings: settings.get_warnings(),
        auto_compact: state.auto_compaction_enabled,
        steering_mode: state.steering_mode.clone(),
        follow_up_mode: state.follow_up_mode.clone(),
        thinking_level: state.thinking_level.as_str().into(),
        available_thinking_levels: available_thinking_levels(state)
            .iter()
            .map(|level| level.as_str().into())
            .collect(),
        current_theme: settings.get_theme().unwrap_or_else(|| "prime".into()),
        available_themes: get_available_themes(),
        hide_thinking_block: mode.hide_thinking_block,
        fullscreen: mode.fullscreen_enabled,
    };
    macro_rules! callback {
        ($variant:ident) => {{
            let send = send.clone();
            Box::new(move |value| {
                let _ = send.send(HostEvent::Setting(Change::$variant(value)));
            })
        }};
    }
    let close = send.clone();
    Ok(SettingsSelectorComponent::new(
        config,
        SettingsCallbacks {
            on_auto_compact_change: callback!(AutoCompact),
            on_idle_eviction_minutes_change: callback!(IdleEvictionMinutes),
            on_show_images_change: callback!(ShowImages),
            on_auto_resize_images_change: callback!(AutoResizeImages),
            on_block_images_change: callback!(BlockImages),
            on_enable_skill_commands_change: callback!(EnableSkillCommands),
            on_enable_builtin_skills_change: callback!(EnableBuiltinSkills),
            on_steering_mode_change: callback!(SteeringMode),
            on_follow_up_mode_change: callback!(FollowUpMode),
            on_transport_change: callback!(Transport),
            on_thinking_level_change: callback!(ThinkingLevel),
            on_theme_change: callback!(Theme),
            on_hide_thinking_block_change: callback!(HideThinkingBlock),
            on_mermaid_rendering_mode_change: callback!(MermaidRenderingMode),
            on_tree_filter_mode_change: callback!(TreeFilterMode),
            on_show_hardware_cursor_change: callback!(ShowHardwareCursor),
            on_editor_padding_x_change: callback!(EditorPaddingX),
            on_autocomplete_max_visible_change: callback!(AutocompleteMaxVisible),
            on_quiet_startup_change: callback!(QuietStartup),
            on_clear_on_shrink_change: callback!(ClearOnShrink),
            on_show_terminal_progress_change: callback!(ShowTerminalProgress),
            on_fullscreen_change: callback!(Fullscreen),
            on_warnings_change: callback!(Warnings),
            on_theme_preview: Some(callback!(ThemePreview)),
            on_cancel: Box::new(move || {
                let _ = close.send(HostEvent::Setting(Change::Close));
            }),
        },
    ))
}

pub(super) fn fullscreen(
    enabled: bool,
    mode: &Rc<RefCell<InteractiveMode>>,
    editor: &Rc<RefCell<CustomEditor>>,
    ui: &Rc<RefCell<TUI>>,
    transcript: &Rc<RefCell<Transcript>>,
) {
    mode.borrow_mut().fullscreen_enabled = enabled;
    if enabled {
        let dock = Rc::new(RefCell::new(pi_tui::tui::Container::new()));
        dock.borrow_mut().add_child(editor.clone());
        dock.borrow_mut()
            .add_child(Rc::new(RefCell::new(Tray(mode.clone(), editor.clone()))));
        let mouse = mode
            .borrow()
            .settings_manager()
            .lock()
            .map(|s| s.get_fullscreen_mouse())
            .unwrap_or(true);
        ui.borrow_mut()
            .enter_fullscreen(pi_tui::tui::FullscreenOptions {
                scroll: vec![transcript.clone()],
                dock,
                mouse,
                viewport_controls: true,
            });
    } else {
        ui.borrow_mut()
            .exit_fullscreen(pi_tui::tui::ExitFullscreenOptions {
                flush: true,
                leave_alt_screen: true,
            });
    }
    ui.borrow_mut().request_render();
}

pub(super) async fn apply(
    change: Change,
    mode: &Rc<RefCell<InteractiveMode>>,
    editor: &Rc<RefCell<CustomEditor>>,
    ui: &Rc<RefCell<TUI>>,
    transcript: &Rc<RefCell<Transcript>>,
    connection: &Arc<dyn wire::AgentConnection>,
) -> Result<(), String> {
    // Daemon-owned settings are committed there before the local view changes.
    match &change {
        Change::AutoCompact(value) => connection.set_auto_compaction_enabled(*value).await?,
        Change::SteeringMode(value) => connection.set_steering_mode(value).await?,
        Change::FollowUpMode(value) => connection.set_follow_up_mode(value).await?,
        Change::Transport(value) => connection.set_transport(value.clone()).await?,
        Change::ThinkingLevel(value) => {
            let level = serde_json::from_value(serde_json::Value::String(value.clone()))
                .map_err(|e| e.to_string())?;
            connection.set_thinking_level(level).await?;
        }
        _ => {}
    }
    {
        let manager = mode.borrow().settings_manager().clone();
        let mut settings = manager.lock().map_err(|e| e.to_string())?;
        match &change {
            Change::AutoCompact(value) => settings.set_compaction_enabled(*value),
            Change::IdleEvictionMinutes(value) => settings.set_idle_eviction_minutes(match value {
                IdleEvictionMinutes::Minutes(n) => {
                    crate::core::settings_manager::IdleEvictionMinutes::Minutes(*n)
                }
                IdleEvictionMinutes::Off => crate::core::settings_manager::IdleEvictionMinutes::Off,
            }),
            Change::ShowImages(value) => settings.set_show_images(*value),
            Change::AutoResizeImages(value) => settings.set_image_auto_resize(*value),
            Change::BlockImages(value) => settings.set_block_images(*value),
            Change::EnableSkillCommands(value) => settings.set_enable_skill_commands(*value),
            Change::EnableBuiltinSkills(value) => settings.set_enable_builtin_skills(*value),
            Change::SteeringMode(value) => settings.set_steering_mode(value),
            Change::FollowUpMode(value) => settings.set_follow_up_mode(value),
            Change::Transport(value) => settings.set_transport(value.clone()),
            Change::Theme(value) => settings.set_theme(value),
            Change::HideThinkingBlock(value) => settings.set_hide_thinking_block(*value),
            Change::MermaidRenderingMode(value) => settings.set_mermaid_rendering_mode(value),
            Change::TreeFilterMode(value) => settings.set_tree_filter_mode(value),
            Change::ShowHardwareCursor(value) => settings.set_show_hardware_cursor(*value),
            Change::EditorPaddingX(value) => settings.set_editor_padding_x(*value),
            Change::AutocompleteMaxVisible(value) => settings.set_autocomplete_max_visible(*value),
            Change::QuietStartup(value) => settings.set_quiet_startup(*value),
            Change::ClearOnShrink(value) => settings.set_clear_on_shrink(*value),
            Change::ShowTerminalProgress(value) => settings.set_show_terminal_progress(*value),
            Change::Fullscreen(value) => settings.set_fullscreen(*value),
            Change::Warnings(value) => settings.set_warnings(value.clone()),
            _ => {}
        }
    }
    match change {
        Change::ShowImages(enabled) => {
            for tool in transcript.borrow().all_tools() {
                tool.borrow_mut().set_show_images(enabled);
            }
        }
        Change::HideThinkingBlock(hidden) => {
            mode.borrow_mut().hide_thinking_block = hidden;
            for assistant in transcript.borrow().all_assistants() {
                assistant.borrow_mut().set_hide_thinking_block(hidden);
            }
        }
        Change::EditorPaddingX(padding) => editor.borrow_mut().editor_mut().set_padding_x(padding),
        Change::AutocompleteMaxVisible(maximum) => editor
            .borrow_mut()
            .editor_mut()
            .set_autocomplete_max_visible(maximum),
        Change::ShowHardwareCursor(enabled) => ui.borrow_mut().set_show_hardware_cursor(enabled),
        Change::ClearOnShrink(enabled) => ui.borrow_mut().set_clear_on_shrink(enabled),
        Change::Fullscreen(enabled) => fullscreen(enabled, mode, editor, ui, transcript),
        Change::Theme(name) | Change::ThemePreview(name) => {
            let result = set_theme(&name, true);
            if !result.success {
                mode.borrow_mut().show_error(&format!(
                    "Failed to load theme \"{name}\": {}\nFell back to dark theme.",
                    result.error.unwrap_or_default()
                ));
            }
            ui.borrow_mut().invalidate();
        }
        Change::EnableBuiltinSkills(_) => {
            connection.reload().await?;
        }
        Change::EnableSkillCommands(_) => {
            native_autocomplete::configure(
                &mut editor.borrow_mut(),
                mode.clone(),
                &mode.borrow().get_current_cwd(),
            );
        }
        Change::AutoCompact(value) => mode
            .borrow_mut()
            .patch_connection_state(|state| state.auto_compaction_enabled = value),
        Change::SteeringMode(value) => mode
            .borrow_mut()
            .patch_connection_state(|state| state.steering_mode = value),
        Change::FollowUpMode(value) => mode
            .borrow_mut()
            .patch_connection_state(|state| state.follow_up_mode = value),
        Change::ThinkingLevel(value) => mode.borrow_mut().patch_connection_state(|state| {
            state.thinking_level =
                serde_json::from_value(serde_json::Value::String(value)).unwrap_or_default()
        }),
        _ => {}
    }
    ui.borrow_mut().request_render();
    Ok(())
}
