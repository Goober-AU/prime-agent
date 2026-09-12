//! Port of packages/coding-agent/src/modes/interactive/components/scoped-models-selector.ts

use std::collections::HashMap;

use pi_ai::types::Model;
use pi_tui::components::input::Input;
use pi_tui::components::spacer::Spacer;
use pi_tui::components::text::Text;
use pi_tui::keys::{key, key_ctrl, matches_key};
use pi_tui::tui::{Component, Focusable};

use crate::modes::interactive::theme::theme::theme;

use super::dynamic_border::DynamicBorder;
use super::keybinding_hints::{key_text, KeyTextOptions};

/// `type EnabledIds = string[] | null`
pub type EnabledIds = Option<Vec<String>>;

fn is_enabled(enabled_ids: &EnabledIds, id: &str) -> bool {
    match enabled_ids {
        None => true,
        Some(ids) => ids.iter().any(|existing| existing == id),
    }
}

fn toggle(enabled_ids: &EnabledIds, id: &str) -> EnabledIds {
    match enabled_ids {
        // First toggle: start with only this one
        None => Some(vec![id.to_string()]),
        Some(ids) => match ids.iter().position(|existing| existing == id) {
            Some(index) => Some(
                ids[..index]
                    .iter()
                    .cloned()
                    .chain(ids[index + 1..].iter().cloned())
                    .collect(),
            ),
            None => Some(ids.iter().cloned().chain([id.to_string()]).collect()),
        },
    }
}

fn enable_all(
    enabled_ids: &EnabledIds,
    all_ids: &[String],
    target_ids: Option<&[String]>,
) -> EnabledIds {
    let ids = match enabled_ids {
        // Already all enabled
        None => return None,
        Some(ids) => ids.clone(),
    };
    let targets: Vec<String> = match target_ids {
        Some(targets) => targets.to_vec(),
        None => all_ids.to_vec(),
    };
    let mut result = ids;
    for id in targets {
        if !result.contains(&id) {
            result.push(id);
        }
    }
    if result.len() == all_ids.len() {
        None
    } else {
        Some(result)
    }
}

fn clear_all(
    enabled_ids: &EnabledIds,
    all_ids: &[String],
    target_ids: Option<&[String]>,
) -> EnabledIds {
    match enabled_ids {
        None => Some(match target_ids {
            Some(targets) => all_ids
                .iter()
                .filter(|id| !targets.contains(id))
                .cloned()
                .collect(),
            None => Vec::new(),
        }),
        Some(ids) => {
            let targets: Vec<String> = match target_ids {
                Some(targets) => targets.to_vec(),
                None => ids.clone(),
            };
            Some(
                ids.iter()
                    .filter(|id| !targets.contains(id))
                    .cloned()
                    .collect(),
            )
        }
    }
}

fn move_id(enabled_ids: &EnabledIds, id: &str, delta: i64) -> EnabledIds {
    let list = match enabled_ids {
        None => return None,
        Some(ids) => ids.clone(),
    };
    let Some(index) = list.iter().position(|existing| existing == id) else {
        return Some(list);
    };
    let new_index = index as i64 + delta;
    if new_index < 0 || new_index >= list.len() as i64 {
        return Some(list);
    }
    let mut result = list;
    result.swap(index, new_index as usize);
    Some(result)
}

fn get_sorted_ids(enabled_ids: &EnabledIds, all_ids: &[String]) -> Vec<String> {
    match enabled_ids {
        None => all_ids.to_vec(),
        Some(ids) => ids
            .iter()
            .cloned()
            .chain(all_ids.iter().filter(|id| !ids.contains(id)).cloned())
            .collect(),
    }
}

/// `interface ModelItem`
#[derive(Debug, Clone)]
pub struct ModelItem {
    pub full_id: String,
    pub model: Model,
    pub enabled: bool,
}

/// `ModelsConfig`
#[derive(Debug, Clone)]
pub struct ModelsConfig {
    pub all_models: Vec<Model>,
    pub enabled_model_ids: Option<Vec<String>>,
}

/// `ModelsCallbacks`
pub struct ModelsCallbacks {
    /// Called whenever the enabled model set or order changes (session-only, no persist)
    pub on_change: Box<dyn Fn(Option<Vec<String>>)>,
    /// Called when user wants to persist current selection to settings
    pub on_persist: Box<dyn Fn(Option<Vec<String>>)>,
    pub on_cancel: Box<dyn Fn()>,
}

pub struct ScopedModelsSelectorComponent {
    models_by_id: HashMap<String, Model>,
    all_ids: Vec<String>,
    enabled_ids: EnabledIds,
    pub filtered_items: Vec<ModelItem>,
    pub selected_index: usize,
    search_query: String,
    search_input: Input,
    focused: bool,
    /// The list/footer are rendered as plain lines in the port; this keeps the
    /// same visible window the TypeScript computes.
    pub visible_range: (usize, usize),
    pub footer_text: String,
    callbacks: ModelsCallbacks,
    max_visible: usize,
    is_dirty: bool,
}

impl ScopedModelsSelectorComponent {
    /// `constructor(config, callbacks)`
    pub fn new(config: ModelsConfig, callbacks: ModelsCallbacks) -> Self {
        let mut models_by_id: HashMap<String, Model> = HashMap::new();
        let mut all_ids: Vec<String> = Vec::new();
        for model in &config.all_models {
            let full_id = format!("{}/{}", model.provider, model.id);
            models_by_id.insert(full_id.clone(), model.clone());
            all_ids.push(full_id);
        }

        let mut component = Self {
            models_by_id,
            all_ids,
            enabled_ids: config.enabled_model_ids.map(|ids| ids.clone()),
            filtered_items: Vec::new(),
            selected_index: 0,
            search_query: String::new(),
            search_input: Input::new(),
            focused: false,
            visible_range: (0, 0),
            footer_text: String::new(),
            callbacks,
            max_visible: 8,
            is_dirty: false,
        };
        component.filtered_items = component.build_items();
        component.footer_text = component.get_footer_text();
        component.update_list();
        component
    }

    fn build_items(&self) -> Vec<ModelItem> {
        // Filter out IDs that no longer have a corresponding model (e.g., after logout)
        get_sorted_ids(&self.enabled_ids, &self.all_ids)
            .into_iter()
            .filter(|id| self.models_by_id.contains_key(id))
            .map(|id| ModelItem {
                full_id: id.clone(),
                model: self.models_by_id[&id].clone(),
                enabled: is_enabled(&self.enabled_ids, &id),
            })
            .collect()
    }

    /// Port of `getFooterText`.
    pub fn get_footer_text(&self) -> String {
        let enabled_count = match &self.enabled_ids {
            Some(ids) => ids.len(),
            None => self.all_ids.len(),
        };
        let all_enabled = self.enabled_ids.is_none();
        let count_text = if all_enabled {
            "all enabled".to_string()
        } else {
            format!("{enabled_count}/{} enabled", self.all_ids.len())
        };
        let parts = [
            format!(
                "{} toggle",
                key_text("tui.select.confirm", &KeyTextOptions::default())
            ),
            format!(
                "{} all",
                key_text("app.models.enableAll", &KeyTextOptions::default())
            ),
            format!(
                "{} clear",
                key_text("app.models.clearAll", &KeyTextOptions::default())
            ),
            format!(
                "{} provider",
                key_text("app.models.toggleProvider", &KeyTextOptions::default())
            ),
            format!(
                "{}/{} reorder",
                key_text("app.models.reorderUp", &KeyTextOptions::default()),
                key_text("app.models.reorderDown", &KeyTextOptions::default())
            ),
            format!(
                "{} save",
                key_text("app.models.save", &KeyTextOptions::default())
            ),
            count_text,
        ];
        if self.is_dirty {
            format!(
                "{}{}",
                theme().fg("dim", &format!("  {} ", parts.join(" \u{b7} "))),
                theme().fg("warning", "(unsaved)")
            )
        } else {
            theme().fg("dim", &format!("  {}", parts.join(" \u{b7} ")))
        }
    }

    /// Port of `refresh`.
    pub fn refresh(&mut self) {
        let query = self.search_input.get_value().to_string();
        let query_changed = query != self.search_query;
        self.search_query = query.clone();
        let items = self.build_items();
        self.filtered_items = if query.is_empty() {
            items
        } else {
            pi_tui::fuzzy::fuzzy_filter(&items, &query, &|item| {
                format!("{} {}", item.model.id, item.model.provider)
            })
        };
        self.selected_index = if query_changed {
            0
        } else {
            self.selected_index
                .min(self.filtered_items.len().saturating_sub(1))
        };
        self.update_list();
        self.footer_text = self.get_footer_text();
    }

    fn notify_change(&mut self) {
        (self.callbacks.on_change)(self.enabled_ids.clone());
    }

    /// Port of `updateList`'s visible window.
    pub fn update_list(&mut self) {
        if self.filtered_items.is_empty() {
            self.visible_range = (0, 0);
            return;
        }

        let start_index = (self.selected_index as i64 - (self.max_visible / 2) as i64)
            .min(self.filtered_items.len() as i64 - self.max_visible as i64)
            .max(0) as usize;
        self.visible_range = (
            start_index,
            (start_index + self.max_visible).min(self.filtered_items.len()),
        );
    }

    /// The rendered list lines (`updateList`'s child list).
    pub fn list_lines(&self) -> Vec<String> {
        if self.filtered_items.is_empty() {
            return vec![theme().fg("muted", "  No matching models")];
        }

        let (start_index, end_index) = self.visible_range;
        let all_enabled = self.enabled_ids.is_none();
        let mut lines: Vec<String> = Vec::new();

        for i in start_index..end_index {
            let item = &self.filtered_items[i];
            let is_selected = i == self.selected_index;
            let prefix = if is_selected {
                theme().fg("accent", "\u{203a} ")
            } else {
                "  ".to_string()
            };
            let model_text = if is_selected {
                theme().fg("accent", &item.model.id)
            } else {
                item.model.id.clone()
            };
            let provider_badge = theme().fg("muted", &format!(" [{}]", item.model.provider));
            let status = if all_enabled {
                String::new()
            } else if item.enabled {
                theme().fg("success", " \u{2713}")
            } else {
                theme().fg("dim", " \u{2717}")
            };
            lines.push(format!("{prefix}{model_text}{provider_badge}{status}"));
        }

        if start_index > 0 || end_index < self.filtered_items.len() {
            lines.push(theme().fg(
                "muted",
                &format!(
                    "  ({}/{})",
                    self.selected_index + 1,
                    self.filtered_items.len()
                ),
            ));
        }

        if !self.filtered_items.is_empty() {
            let selected = &self.filtered_items[self.selected_index];
            lines.push(String::new());
            lines.push(theme().fg("muted", &format!("  Model Name: {}", selected.model.name)));
        }
        lines
    }

    /// Port of `handleInput`.
    pub fn handle_input(&mut self, data: &str) {
        let kb = pi_tui::keybindings::get_keybindings();

        if kb.matches(data, "tui.select.up") {
            if self.filtered_items.is_empty() {
                return;
            }
            self.selected_index = if self.selected_index == 0 {
                self.filtered_items.len() - 1
            } else {
                self.selected_index - 1
            };
            self.update_list();
            return;
        }
        if kb.matches(data, "tui.select.down") {
            if self.filtered_items.is_empty() {
                return;
            }
            self.selected_index = if self.selected_index == self.filtered_items.len() - 1 {
                0
            } else {
                self.selected_index + 1
            };
            self.update_list();
            return;
        }

        let reorder_up = kb.matches(data, "app.models.reorderUp");
        let reorder_down = kb.matches(data, "app.models.reorderDown");
        if reorder_up || reorder_down {
            if self.enabled_ids.is_none() {
                return;
            }
            let item = self.filtered_items.get(self.selected_index).cloned();
            if let Some(item) = item {
                if is_enabled(&self.enabled_ids, &item.full_id) {
                    let delta: i64 = if reorder_up { -1 } else { 1 };
                    let current_index = self
                        .enabled_ids
                        .as_ref()
                        .and_then(|ids| ids.iter().position(|id| id == &item.full_id))
                        .unwrap_or(0) as i64;
                    let new_index = current_index + delta;
                    let length = self.enabled_ids.as_ref().map(|ids| ids.len()).unwrap_or(0) as i64;
                    if new_index >= 0 && new_index < length {
                        self.enabled_ids = move_id(&self.enabled_ids, &item.full_id, delta);
                        self.is_dirty = true;
                        self.selected_index = (self.selected_index as i64 + delta).max(0) as usize;
                        self.refresh();
                        self.notify_change();
                    }
                }
            }
            return;
        }

        if kb.matches(data, "tui.select.confirm") {
            if let Some(item) = self.filtered_items.get(self.selected_index).cloned() {
                self.enabled_ids = toggle(&self.enabled_ids, &item.full_id);
                self.is_dirty = true;
                self.refresh();
                self.notify_change();
            }
            return;
        }

        if kb.matches(data, "app.models.enableAll") {
            let target_ids = if self.search_input.get_value().is_empty() {
                None
            } else {
                Some(
                    self.filtered_items
                        .iter()
                        .map(|item| item.full_id.clone())
                        .collect::<Vec<String>>(),
                )
            };
            self.enabled_ids = enable_all(&self.enabled_ids, &self.all_ids, target_ids.as_deref());
            self.is_dirty = true;
            self.refresh();
            self.notify_change();
            return;
        }

        if kb.matches(data, "app.models.clearAll") {
            let target_ids = if self.search_input.get_value().is_empty() {
                None
            } else {
                Some(
                    self.filtered_items
                        .iter()
                        .map(|item| item.full_id.clone())
                        .collect::<Vec<String>>(),
                )
            };
            self.enabled_ids = clear_all(&self.enabled_ids, &self.all_ids, target_ids.as_deref());
            self.is_dirty = true;
            self.refresh();
            self.notify_change();
            return;
        }

        if kb.matches(data, "app.models.toggleProvider") {
            if let Some(item) = self.filtered_items.get(self.selected_index).cloned() {
                let provider = item.model.provider.clone();
                let provider_ids: Vec<String> = self
                    .all_ids
                    .iter()
                    .filter(|id| {
                        self.models_by_id
                            .get(*id)
                            .map(|model| model.provider == provider)
                            .unwrap_or(false)
                    })
                    .cloned()
                    .collect();
                let all_enabled = provider_ids
                    .iter()
                    .all(|id| is_enabled(&self.enabled_ids, id));
                self.enabled_ids = if all_enabled {
                    clear_all(&self.enabled_ids, &self.all_ids, Some(&provider_ids))
                } else {
                    enable_all(&self.enabled_ids, &self.all_ids, Some(&provider_ids))
                };
                self.is_dirty = true;
                self.refresh();
                self.notify_change();
            }
            return;
        }

        if kb.matches(data, "app.models.save") {
            (self.callbacks.on_persist)(self.enabled_ids.clone());
            self.is_dirty = false;
            self.footer_text = self.get_footer_text();
            return;
        }

        if matches_key(data, &key_ctrl("c")) {
            if !self.search_input.get_value().is_empty() {
                self.search_input.set_value(String::new());
                self.refresh();
            } else {
                (self.callbacks.on_cancel)();
            }
            return;
        }

        if matches_key(data, key::escape) {
            (self.callbacks.on_cancel)();
            return;
        }

        self.search_input.handle_input(data);
        self.refresh();
    }

    /// Port of `getSearchInput()`.
    pub fn search_input(&self) -> &Input {
        &self.search_input
    }

    pub fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
        pi_tui::tui::Focusable::set_focused(&mut self.search_input, focused);
    }

    pub fn is_focused(&self) -> bool {
        self.focused
    }

    pub fn is_dirty(&self) -> bool {
        self.is_dirty
    }

    pub fn enabled_ids(&self) -> &EnabledIds {
        &self.enabled_ids
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_ai::types::Model;

    fn model(provider: &str, id: &str) -> Model {
        Model {
            id: id.to_string(),
            name: id.to_string(),
            provider: provider.to_string(),
            ..Default::default()
        }
    }

    fn noop_callbacks() -> ModelsCallbacks {
        ModelsCallbacks {
            on_change: Box::new(|_| {}),
            on_persist: Box::new(|_| {}),
            on_cancel: Box::new(|| {}),
        }
    }

    #[test]
    fn toggle_starts_from_only_the_toggled_id() {
        assert_eq!(toggle(&None, "a"), Some(vec!["a".to_string()]));
        assert_eq!(
            toggle(&Some(vec!["a".to_string(), "b".to_string()]), "a"),
            Some(vec!["b".to_string()])
        );
        assert_eq!(
            toggle(&Some(vec!["a".to_string()]), "c"),
            Some(vec!["a".to_string(), "c".to_string()])
        );
    }

    #[test]
    fn enable_all_returns_null_when_everything_is_enabled() {
        let all = vec!["a".to_string(), "b".to_string()];
        assert_eq!(enable_all(&None, &all, None), None);
        assert_eq!(enable_all(&Some(vec!["a".to_string()]), &all, None), None);
        let two = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        assert_eq!(
            enable_all(&Some(vec!["a".to_string()]), &two, None),
            Some(vec!["a".to_string(), "b".to_string(), "c".to_string()])
        );
    }

    #[test]
    fn clear_all_with_targets_keeps_the_other_ids() {
        let all = vec!["a".to_string(), "b".to_string()];
        assert_eq!(
            clear_all(&None, &all, Some(&["a".to_string()])),
            Some(vec!["b".to_string()])
        );
        assert_eq!(clear_all(&None, &all, None), Some(Vec::new()));
    }

    #[test]
    fn move_swaps_within_bounds_only() {
        let ids = Some(vec!["a".to_string(), "b".to_string()]);
        assert_eq!(
            move_id(&ids, "a", 1),
            Some(vec!["b".to_string(), "a".to_string()])
        );
        assert_eq!(move_id(&ids, "a", -1), ids);
        assert_eq!(move_id(&None, "a", 1), None);
    }

    #[test]
    fn sorted_ids_keep_enabled_order_then_the_rest() {
        let all = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        assert_eq!(
            get_sorted_ids(&Some(vec!["c".to_string()]), &all),
            vec!["c".to_string(), "a".to_string(), "b".to_string()]
        );
        assert_eq!(get_sorted_ids(&None, &all), all);
    }

    #[test]
    fn footer_reports_all_enabled_and_unsaved_state() {
        let component = ScopedModelsSelectorComponent::new(
            ModelsConfig {
                all_models: vec![model("a", "1")],
                enabled_model_ids: None,
            },
            noop_callbacks(),
        );
        assert!(component.get_footer_text().contains("all enabled"));
        assert!(!component.is_dirty());
    }

    #[test]
    fn confirm_toggles_the_selected_model_and_marks_dirty() {
        let mut component = ScopedModelsSelectorComponent::new(
            ModelsConfig {
                all_models: vec![model("a", "1"), model("a", "2")],
                enabled_model_ids: None,
            },
            noop_callbacks(),
        );
        component.handle_input("\r");
        assert!(component.is_dirty());
        assert_eq!(
            component.enabled_ids().as_ref().map(|ids| ids.len()),
            Some(1)
        );
    }

    #[test]
    fn escape_cancels_and_control_c_clears_the_filter_first() {
        let cancelled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = std::sync::Arc::clone(&cancelled);
        let mut component = ScopedModelsSelectorComponent::new(
            ModelsConfig {
                all_models: vec![model("a", "1")],
                enabled_model_ids: None,
            },
            ModelsCallbacks {
                on_change: Box::new(|_| {}),
                on_persist: Box::new(|_| {}),
                on_cancel: Box::new(move || flag.store(true, std::sync::atomic::Ordering::SeqCst)),
            },
        );
        component.handle_input("\u{1b}");
        assert!(cancelled.load(std::sync::atomic::Ordering::SeqCst));

        cancelled.store(false, std::sync::atomic::Ordering::SeqCst);
        component.search_input.set_value("q".to_string());
        component.handle_input("\u{3}");
        assert!(!cancelled.load(std::sync::atomic::Ordering::SeqCst));
        assert!(component.search_input.get_value().is_empty());
    }
}
