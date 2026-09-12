//! Port of packages/coding-agent/src/cli/config-selector.ts
//!
//! TODO(slice): `ConfigSelectorComponent`
//! (modes/interactive/components/config-selector.ts, interactive slice) is not
//! landed - the file is still empty. The component is a TUI surface, not CLI
//! logic, so this module keeps `selectConfig`'s exact flow (theme init, TUI
//! lifecycle, close/exit callbacks, focus on the resource list) and drives a
//! private local stand-in component that renders the same header line and
//! reports close/exit through the same callbacks.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use pi_tui::terminal::ProcessTerminal;
use pi_tui::tui::{Component, Focusable, TUI, TuiStopOptions};

use crate::core::package_manager::ResolvedPaths;
use crate::core::settings_manager::SettingsManager;
use crate::modes::interactive::theme::theme::{init_theme, stop_theme_watcher};

pub struct ConfigSelectorOptions {
    pub resolved_paths: ResolvedPaths,
    pub settings_manager: SettingsManager,
    pub cwd: String,
    pub agent_dir: String,
}

/// Port of `selectConfig`. The TypeScript resolves its promise from the
/// component's `onClose` callback; the port waits on the same callback through a
/// oneshot channel, so a close request ends the function and an exit request
/// ends the process.
pub async fn select_config(options: ConfigSelectorOptions) -> Result<(), String> {
    init_theme(options.settings_manager.get_theme().as_deref(), true);

    let (close_sender, close_receiver) = tokio::sync::oneshot::channel::<()>();
    let (exit_sender, exit_receiver) = tokio::sync::oneshot::channel::<()>();
    // The callbacks are `Fn`, invoked once by the component, so the single-use
    // sender is taken out of a shared slot rather than moved by the closure.
    let close_sender = std::sync::Mutex::new(Some(close_sender));
    let exit_sender = std::sync::Mutex::new(Some(exit_sender));
    let resolved = Arc::new(AtomicBool::new(false));

    let mut ui = TUI::new(Box::new(ProcessTerminal::new()), None);

    let close_flag = Arc::clone(&resolved);
    let request_render = Arc::new(AtomicBool::new(false));
    let selector = Rc::new(RefCell::new(ConfigSelectorComponent::new(
        ConfigSelectorCallbacks {
            on_close: Box::new(move || {
                if !close_flag.swap(true, Ordering::SeqCst) {
                    if let Some(sender) =
                        close_sender.lock().unwrap_or_else(|error| error.into_inner()).take()
                    {
                        let _ = sender.send(());
                    }
                }
            }),
            on_exit: Box::new(move || {
                if let Some(sender) =
                    exit_sender.lock().unwrap_or_else(|error| error.into_inner()).take()
                {
                    let _ = sender.send(());
                }
            }),
            request_render: {
                let flag = Arc::clone(&request_render);
                Box::new(move || flag.store(true, Ordering::SeqCst))
            },
        },
        options.cwd.clone(),
        options.agent_dir.clone(),
        options.resolved_paths.clone(),
    )));

    let resource_list: Rc<RefCell<dyn Component>> = selector.clone();
    ui.add_child(resource_list.clone());
    ui.set_focus(Some(resource_list));
    ui.start();

    tokio::select! {
        _ = close_receiver => {}
        _ = exit_receiver => {
            ui.stop(TuiStopOptions::default());
            stop_theme_watcher();
            std::process::exit(0);
        }
    }

    ui.stop(TuiStopOptions::default());
    stop_theme_watcher();
    Ok(())
}

pub struct ConfigSelectorCallbacks {
    pub on_close: Box<dyn Fn() + Send + Sync>,
    pub on_exit: Box<dyn Fn() + Send + Sync>,
    pub request_render: Box<dyn Fn() + Send + Sync>,
}

/// Private local stand-in for `ConfigSelectorComponent`
/// (modes/interactive/components/config-selector.ts). The real component renders
/// the resource list and toggles resources through `SettingsManager`; the
/// stand-in keeps the container contract, the focusable surface and the
/// close/exit callbacks that `selectConfig` drives.
struct ConfigSelectorComponent {
    callbacks: ConfigSelectorCallbacks,
    cwd: String,
    agent_dir: String,
    resolved_paths: ResolvedPaths,
    focused: bool,
    closed: bool,
}

impl ConfigSelectorComponent {
    fn new(
        callbacks: ConfigSelectorCallbacks,
        cwd: String,
        agent_dir: String,
        resolved_paths: ResolvedPaths,
    ) -> Self {
        Self { callbacks, cwd, agent_dir, resolved_paths, focused: false, closed: false }
    }

    /// `getResourceList()` - the stand-in is its own resource list.
    fn resource_list(&self) -> &Self {
        self
    }

    fn close(&mut self) {
        if self.closed {
            return;
        }
        self.closed = true;
        (self.callbacks.on_close)();
    }

    fn exit(&mut self) {
        (self.callbacks.on_exit)();
    }
}

impl Component for ConfigSelectorComponent {
    fn render(&mut self, width: f64) -> Vec<String> {
        let counts = [
            ("extensions", self.resolved_paths.extensions.len()),
            ("skills", self.resolved_paths.skills.len()),
            ("prompts", self.resolved_paths.prompts.len()),
            ("themes", self.resolved_paths.themes.len()),
        ];
        let mut header = String::from("Resources");
        for (label, count) in counts {
            header.push_str(&format!("  {}: {}", label, count));
        }
        header.push_str(&format!("  cwd: {}", self.cwd));
        header.push_str(&format!("  agent: {}", self.agent_dir));
        if self.focused {
            header.push_str("  [focused]");
        }
        let width = width.max(0.0) as usize;
        if width > 0 && header.chars().count() > width {
            return vec![header.chars().take(width).collect()];
        }
        vec![header]
    }

    fn handle_input(&mut self, data: &str) {
        match data {
            "\u{1b}" | "q" => self.close(),
            "\u{3}" => self.exit(),
            _ => {}
        }
    }

    fn invalidate(&mut self) {}

    fn as_focusable(&mut self) -> Option<&mut dyn Focusable> {
        Some(self)
    }
}

impl Focusable for ConfigSelectorComponent {
    fn focused(&self) -> bool {
        self.focused
    }

    fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
        (self.callbacks.request_render)();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_paths() -> ResolvedPaths {
        ResolvedPaths {
            extensions: Vec::new(),
            skills: Vec::new(),
            prompts: Vec::new(),
            themes: Vec::new(),
            diagnostics: Vec::new(),
        }
    }

    fn component(cwd: &str) -> ConfigSelectorComponent {
        ConfigSelectorComponent::new(
            ConfigSelectorCallbacks {
                on_close: Box::new(|| {}),
                on_exit: Box::new(|| {}),
                request_render: Box::new(|| {}),
            },
            cwd.to_string(),
            "/agent".to_string(),
            empty_paths(),
        )
    }

    #[test]
    fn renders_the_resource_counts_and_paths() {
        let mut selector = component("/work");
        let lines = selector.render(200.0);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].starts_with("Resources  extensions: 0"));
        assert!(lines[0].contains("cwd: /work"));
        assert!(lines[0].contains("agent: /agent"));
        assert!(!lines[0].contains("[focused]"));
    }

    #[test]
    fn render_is_clipped_to_the_viewport_width() {
        let mut selector = component("/work");
        let lines = selector.render(9.0);
        assert_eq!(lines[0].chars().count(), 9);
        assert_eq!(lines[0], "Resources");
    }

    #[test]
    fn focus_state_is_forwarded_to_the_render_and_the_request_render_callback() {
        let renders = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&renders);
        let mut selector = ConfigSelectorComponent::new(
            ConfigSelectorCallbacks {
                on_close: Box::new(|| {}),
                on_exit: Box::new(|| {}),
                request_render: Box::new(move || flag.store(true, Ordering::SeqCst)),
            },
            "/work".to_string(),
            "/agent".to_string(),
            empty_paths(),
        );
        assert!(!selector.focused());
        selector.set_focused(true);
        assert!(selector.focused());
        assert!(renders.load(Ordering::SeqCst));
        assert!(selector.render(200.0)[0].contains("[focused]"));
    }

    #[test]
    fn escape_closes_once_and_ctrl_c_exits() {
        let closes = Arc::new(AtomicBool::new(false));
        let exits = Arc::new(AtomicBool::new(false));
        let close_flag = Arc::clone(&closes);
        let exit_flag = Arc::clone(&exits);
        let mut selector = ConfigSelectorComponent::new(
            ConfigSelectorCallbacks {
                on_close: Box::new(move || close_flag.store(true, Ordering::SeqCst)),
                on_exit: Box::new(move || exit_flag.store(true, Ordering::SeqCst)),
                request_render: Box::new(|| {}),
            },
            "/work".to_string(),
            "/agent".to_string(),
            empty_paths(),
        );
        selector.handle_input("x");
        assert!(!closes.load(Ordering::SeqCst));
        selector.handle_input("\u{1b}");
        assert!(closes.load(Ordering::SeqCst));
        selector.handle_input("\u{1b}");
        selector.handle_input("\u{3}");
        assert!(exits.load(Ordering::SeqCst));
    }

    #[test]
    fn the_component_is_its_own_resource_list_and_is_focusable() {
        let mut selector = component("/work");
        let _: &ConfigSelectorComponent = selector.resource_list();
        assert!(selector.as_focusable().is_some());
    }
}
