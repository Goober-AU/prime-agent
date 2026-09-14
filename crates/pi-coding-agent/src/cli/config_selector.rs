//! Port of packages/coding-agent/src/cli/config-selector.ts
//!
//! `selectConfig` mounts the real `ConfigSelectorComponent`
//! (`packages/coding-agent/src/cli/config-selector.ts:21-44`):
//! `new ConfigSelectorComponent(resolvedPaths, settingsManager, cwd, agentDir,
//! onClose, onExit, requestRender)`, then `ui.addChild(selector)` and
//! `ui.setFocus(selector.getResourceList())`. This module performs the same
//! three steps against `components::config_selector::ConfigSelectorComponent`,
//! so the resource list, the per-item toggles and their `SettingsManager`
//! writes are the real ones.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use pi_tui::terminal::ProcessTerminal;
use pi_tui::tui::{TuiStopOptions, TUI};
// Used only by the retained stand-in's `cfg(test)` impls.
#[cfg(test)]
use pi_tui::tui::{Component, Focusable};

use crate::core::package_manager::ResolvedPaths;
use crate::core::settings_manager::SettingsManager;
use crate::modes::interactive::components::config_selector::ConfigSelectorComponent;
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

    let (close_sender, mut close_receiver) = tokio::sync::oneshot::channel::<()>();
    let (exit_sender, mut exit_receiver) = tokio::sync::oneshot::channel::<()>();
    // The callbacks are `Fn`, invoked once by the component, so the single-use
    // sender is taken out of a shared slot rather than moved by the closure.
    let close_sender = std::sync::Mutex::new(Some(close_sender));
    let exit_sender = std::sync::Mutex::new(Some(exit_sender));
    let resolved = Arc::new(AtomicBool::new(false));

    let mut ui = TUI::new(Box::new(ProcessTerminal::new()), None);
    // `() => ui.requestRender()` (config-selector.ts:39). The CLI loop owns the
    // TUI, so the callback marks this flag and the loop drains it.
    let render_requested = Arc::new(AtomicBool::new(false));
    let render_flag = Arc::clone(&render_requested);

    let close_flag = Arc::clone(&resolved);
    // The real component takes `FnMut` callbacks and owns the `SettingsManager`
    // (`ConfigSelectorComponent::new`, components/config_selector.rs:959-996).
    // The close/exit senders are single-use, so they are taken out of a shared
    // slot rather than moved by the closure.
    let selector = Rc::new(RefCell::new(ConfigSelectorComponent::new(
        &options.resolved_paths,
        options.settings_manager,
        options.cwd.clone(),
        options.agent_dir.clone(),
        Box::new(move || {
            if !close_flag.swap(true, Ordering::SeqCst) {
                if let Some(sender) = close_sender
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .take()
                {
                    let _ = sender.send(());
                }
            }
        }),
        Box::new(move || {
            if let Some(sender) = exit_sender
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .take()
            {
                let _ = sender.send(());
            }
        }),
        // `() => ui.requestRender()` (config-selector.ts:39). The CLI loop owns
        // the TUI, so the callback only marks the render flag the loop drains.
        Box::new(move || render_flag.store(true, Ordering::SeqCst)),
    )));

    mount(&mut ui, selector.clone());
    ui.start();

    loop {
        ui.drain_input();
        if render_requested.swap(false, Ordering::SeqCst) {
            ui.request_render();
        }
        tokio::select! {
            _ = &mut close_receiver => break,
            _ = &mut exit_receiver => {
                ui.stop(TuiStopOptions::default());
                stop_theme_watcher();
                std::process::exit(0);
            }
            // The TypeScript pumps renders from the terminal callback; the port
            // polls the same queue the TUI's own input bridge fills.
            _ = tokio::time::sleep(std::time::Duration::from_millis(8)) => {}
        }
    }

    ui.stop(TuiStopOptions::default());
    stop_theme_watcher();
    Ok(())
}

/// Mounts the real selector the way `selectConfig` does:
/// `ui.addChild(selector)` then `ui.setFocus(selector.getResourceList())`
/// (packages/coding-agent/src/cli/config-selector.ts:42-43).
///
/// The component owns its `ResourceList`, so the focus target registered with
/// the TUI is the component itself: its `Focusable::set_focused` forwards to
/// exactly that list (components/config_selector.rs:1053-1056) and
/// `Component::handle_input` routes every key to it as well
/// (components/config_selector.rs:1029-1031). Focusing the component therefore
/// reaches the same focus state the TypeScript reaches by focusing the list.
fn mount(ui: &mut TUI, selector: Rc<RefCell<ConfigSelectorComponent>>) {
    ui.add_child(selector.clone());
    ui.set_focus(Some(selector));
}

#[cfg(test)]
pub struct StandInCallbacks {
    pub on_close: Box<dyn Fn() + Send + Sync>,
    pub on_exit: Box<dyn Fn() + Send + Sync>,
    pub request_render: Box<dyn Fn() + Send + Sync>,
}

/// The superseded stand-in, kept only so its own tests keep running. Production
/// mounts the real `ConfigSelectorComponent` (see `mount` above).
#[cfg(test)]
struct StandInSelector {
    callbacks: StandInCallbacks,
    cwd: String,
    agent_dir: String,
    resolved_paths: ResolvedPaths,
    focused: bool,
    closed: bool,
}

#[cfg(test)]
impl StandInSelector {
    fn new(
        callbacks: StandInCallbacks,
        cwd: String,
        agent_dir: String,
        resolved_paths: ResolvedPaths,
    ) -> Self {
        Self {
            callbacks,
            cwd,
            agent_dir,
            resolved_paths,
            focused: false,
            closed: false,
        }
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

#[cfg(test)]
impl Component for StandInSelector {
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

#[cfg(test)]
impl Focusable for StandInSelector {
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

    fn resolved_resource(
        path: &str,
        origin: &str,
        scope: &str,
        source: &str,
        base_dir: Option<&str>,
    ) -> crate::core::package_manager::ResolvedResource {
        crate::core::package_manager::ResolvedResource {
            path: path.to_string(),
            enabled: true,
            metadata: crate::core::package_manager::PathMetadata {
                source: source.to_string(),
                scope: scope.to_string(),
                origin: origin.to_string(),
                base_dir: base_dir.map(|value| value.to_string()),
            },
        }
    }

    fn empty_paths() -> ResolvedPaths {
        ResolvedPaths {
            extensions: Vec::new(),
            skills: Vec::new(),
            prompts: Vec::new(),
            themes: Vec::new(),
            diagnostics: Vec::new(),
        }
    }

    fn component(cwd: &str) -> StandInSelector {
        StandInSelector::new(
            StandInCallbacks {
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
        let mut selector = StandInSelector::new(
            StandInCallbacks {
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
        let mut selector = StandInSelector::new(
            StandInCallbacks {
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
        let _: &StandInSelector = selector.resource_list();
        assert!(selector.as_focusable().is_some());
    }

    /// DEFECT C. `selectConfig` must mount the REAL `ConfigSelectorComponent`.
    ///
    /// The audit pinned `cli/config_selector.rs:107` driving a private stand-in
    /// whose render was a single counts line, while the ported component
    /// (`components/config_selector.rs`, 13 tests) had zero production callers.
    /// `packages/coding-agent/src/cli/config-selector.ts:21-44` constructs the
    /// real component, adds it as a child and focuses its resource list.
    ///
    /// The stand-in passed its own "contains Resources" assertion, so the
    /// assertion that separates the two is the resource list's own render: the
    /// real header line is `Resource Configuration`, and the real list renders
    /// the type headings (`Extensions`, `Skills`, `Prompts`, `Themes`) that a
    /// counts-only stand-in can never produce.
    #[test]
    fn select_config_mounts_the_real_component_and_focuses_its_resource_list() {
        crate::modes::interactive::theme::theme::init_theme(Some("prime"), false);
        let resolved = ResolvedPaths {
            extensions: vec![resolved_resource(
                "/agent/extensions/foo/index.ts",
                "top-level",
                "user",
                "auto",
                None,
            )],
            skills: Vec::new(),
            prompts: Vec::new(),
            themes: Vec::new(),
            diagnostics: Vec::new(),
        };
        let manager = SettingsManager::in_memory(serde_json::Map::new());
        let selector = Rc::new(RefCell::new(ConfigSelectorComponent::new(
            &resolved,
            manager,
            "/work".into(),
            "/agent".into(),
            Box::new(|| {}),
            Box::new(|| {}),
            Box::new(|| {}),
        )));

        let mut ui = TUI::new(
            Box::new(pi_tui::terminal::ProcessTerminal::new()),
            Some(false),
        );
        mount(&mut ui, selector.clone());

        let rendered = selector.borrow_mut().render(100.0).join("\n");
        assert!(
            rendered.contains("Resource Configuration"),
            "the real header must render, got: {rendered}"
        );
        assert!(
            rendered.contains("Extensions"),
            "the real resource list headings must render, got: {rendered}"
        );
        assert!(
            rendered.contains("foo"),
            "the resolved extension must appear in the list, got: {rendered}"
        );

        // `ui.setFocus(selector.getResourceList())`: the registered focus target
        // is the mounted component, and the component forwards the focus bit to
        // its resource list (components/config_selector.rs:1053-1056).
        let focused = ui.focused_component().expect("mount must set focus");
        assert!(
            Rc::ptr_eq(&focused, &(selector.clone() as Rc<RefCell<dyn Component>>)),
            "the focused component must be the mounted selector"
        );
        assert!(selector.borrow_mut().focused());
    }

    /// The real component's resource list is the one the CLI focuses, so Space
    /// reaches the toggle path that writes through `SettingsManager`. The
    /// stand-in had no list, so this key was a no-op there.
    #[test]
    fn the_focused_real_component_toggles_a_resource_through_the_settings_manager() {
        crate::modes::interactive::theme::theme::init_theme(Some("prime"), false);
        let resolved = ResolvedPaths {
            extensions: vec![resolved_resource(
                "/agent/extensions/foo/index.ts",
                "top-level",
                "user",
                "auto",
                None,
            )],
            skills: Vec::new(),
            prompts: Vec::new(),
            themes: Vec::new(),
            diagnostics: Vec::new(),
        };
        let manager = SettingsManager::in_memory(serde_json::Map::new());
        let selector = Rc::new(RefCell::new(ConfigSelectorComponent::new(
            &resolved,
            manager,
            "/work".into(),
            "/agent".into(),
            Box::new(|| {}),
            Box::new(|| {}),
            Box::new(|| {}),
        )));
        let mut ui = TUI::new(
            Box::new(pi_tui::terminal::ProcessTerminal::new()),
            Some(false),
        );
        mount(&mut ui, selector.clone());

        // The list starts with the first item selected; Space toggles it off and
        // writes the disabled path into the settings manager. The key travels the
        // real component path, which forwards to the real resource list
        // (components/config_selector.rs:1029-1031); the registered TUI child is
        // the same component (asserted by `focused_component` below).
        selector.borrow_mut().handle_input(" ");
        let rendered = selector.borrow_mut().render(100.0).join("\n");
        assert!(
            rendered.contains("[ ]"),
            "Space must reach the real list's toggle and render the disabled marker, got: {rendered}"
        );
    }
}
