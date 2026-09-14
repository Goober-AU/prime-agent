//! Port of packages/coding-agent/src/modes/model-autocomplete.ts

use pi_tui::autocomplete::AutocompleteItem;
use pi_tui::fuzzy::fuzzy_filter;

/// `ModelAutocompleteCandidate`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelAutocompleteCandidate {
    pub id: String,
    pub provider: String,
}

/// `getModelArgumentCompletions(prefix, models)`.
pub fn get_model_argument_completions(
    prefix: &str,
    models: &[ModelAutocompleteCandidate],
) -> Option<Vec<AutocompleteItem>> {
    if models.is_empty() {
        return None;
    }

    let items: Vec<ModelAutocompleteCandidate> = models.to_vec();
    let filtered = fuzzy_filter(&items, prefix, &|item: &ModelAutocompleteCandidate| {
        format!("{} {}", item.id, item.provider)
    });
    if filtered.is_empty() {
        return None;
    }

    Some(
        filtered
            .into_iter()
            .map(|item| AutocompleteItem {
                value: format!("{}/{}", item.provider, item.id),
                label: item.id,
                description: Some(item.provider),
                argument_hint: None,
                source_tag: None,
                takes_argument: None,
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(id: &str, provider: &str) -> ModelAutocompleteCandidate {
        ModelAutocompleteCandidate {
            id: id.to_string(),
            provider: provider.to_string(),
        }
    }

    #[test]
    fn no_models_means_no_completions() {
        assert!(get_model_argument_completions("cl", &[]).is_none());
    }

    #[test]
    fn an_empty_prefix_lists_every_model() {
        let models = vec![candidate("claude", "anthropic"), candidate("gpt", "openai")];
        let items = get_model_argument_completions("", &models).unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].value, "anthropic/claude");
        assert_eq!(items[0].label, "claude");
        assert_eq!(items[0].description.as_deref(), Some("anthropic"));
    }

    #[test]
    fn the_prefix_matches_the_id_or_the_provider() {
        let models = vec![candidate("claude", "anthropic"), candidate("gpt", "openai")];
        let items = get_model_argument_completions("openai", &models).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].value, "openai/gpt");
    }

    #[test]
    fn a_missing_prefix_returns_none() {
        let models = vec![candidate("claude", "anthropic")];
        assert!(get_model_argument_completions("zzz", &models).is_none());
    }
}
