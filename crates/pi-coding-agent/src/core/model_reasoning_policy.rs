//! Compatibility levels for the Azure routes explicitly supported by this build.
use pi_ai::types::Model;

pub fn apply_azure_reasoning_levels(model: &mut Model) {
    if !matches!(model.provider.as_str(), "azure-openai-managed" | "azure-foundry-managed" | "azure-openai-responses") {
        return;
    }
    let levels: &[&str] = match model.id.to_ascii_lowercase().as_str() {
        "gpt-6-astra" | "gpt-5.6-sol" => &["low", "medium", "high", "xhigh", "max"],
        "fw-kimi-k3" | "kimi-k3" | "fw-glm-5.3" | "glm-5.3" => &["low", "high", "max"],
        _ => return,
    };
    model.reasoning = true;
    model.thinking_level_map = Some(super::thinking_levels::THINKING_LEVELS.into_iter()
        .map(|level| (level.to_string(), levels.contains(&level).then(|| level.to_string())))
        .collect());
}
