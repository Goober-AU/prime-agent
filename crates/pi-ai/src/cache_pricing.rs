//! Port of packages/ai/src/cache-pricing.ts

use crate::types::Model;

pub type AnthropicCacheDuration = String;
pub const ANTHROPIC_CACHE_DURATION_5M: &str = "5m";
pub const ANTHROPIC_CACHE_DURATION_1H: &str = "1h";

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AnthropicCacheCreationUsage {
    pub ephemeral_5m_input_tokens: f64,
    pub ephemeral_1h_input_tokens: f64,
}

const ANTHROPIC_CACHE_READ_COST_MULTIPLIER: f64 = 0.1;
const ANTHROPIC_FIVE_MINUTE_CACHE_WRITE_COST_MULTIPLIER: f64 = 1.25;
const ANTHROPIC_ONE_HOUR_CACHE_WRITE_COST_MULTIPLIER: f64 = 2.0;

/// `Number.EPSILON`
const NUMBER_EPSILON: f64 = f64::EPSILON;

pub fn has_standard_anthropic_cache_pricing(model: &Model) -> bool {
    let model_id = model.id.to_lowercase();
    let is_anthropic_model = model.provider == "anthropic"
        || model_id.starts_with("anthropic/")
        || model_id.starts_with("claude-");
    if !is_anthropic_model {
        return false;
    }

    let expected_cache_write_cost =
        model.cost.input * ANTHROPIC_FIVE_MINUTE_CACHE_WRITE_COST_MULTIPLIER;
    let tolerance = NUMBER_EPSILON
        * 1.0f64
            .max(model.cost.cache_write)
            .max(expected_cache_write_cost);
    (model.cost.cache_write - expected_cache_write_cost).abs() <= tolerance
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AnthropicCacheCosts {
    pub cache_read: f64,
    pub cache_write: f64,
}

pub fn get_anthropic_cache_costs(input_cost: f64, duration: &str) -> AnthropicCacheCosts {
    AnthropicCacheCosts {
        cache_read: input_cost * ANTHROPIC_CACHE_READ_COST_MULTIPLIER,
        cache_write: input_cost
            * if duration == ANTHROPIC_CACHE_DURATION_1H {
                ANTHROPIC_ONE_HOUR_CACHE_WRITE_COST_MULTIPLIER
            } else {
                ANTHROPIC_FIVE_MINUTE_CACHE_WRITE_COST_MULTIPLIER
            },
    }
}

pub fn get_anthropic_cache_write_cost(
    input_cost: f64,
    duration: &str,
    cache_creation: Option<&AnthropicCacheCreationUsage>,
) -> f64 {
    let Some(cache_creation) = cache_creation else {
        return get_anthropic_cache_costs(input_cost, duration).cache_write;
    };

    let five_minute_tokens = cache_creation.ephemeral_5m_input_tokens;
    let one_hour_tokens = cache_creation.ephemeral_1h_input_tokens;
    let total_tokens = five_minute_tokens + one_hour_tokens;
    if total_tokens == 0.0 {
        return get_anthropic_cache_costs(input_cost, duration).cache_write;
    }

    (input_cost
        * (five_minute_tokens * ANTHROPIC_FIVE_MINUTE_CACHE_WRITE_COST_MULTIPLIER
            + one_hour_tokens * ANTHROPIC_ONE_HOUR_CACHE_WRITE_COST_MULTIPLIER))
        / total_tokens
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(provider: &str, id: &str, input: f64, cache_write: f64) -> Model {
        let mut model = Model::default();
        model.provider = provider.to_string();
        model.id = id.to_string();
        model.cost.input = input;
        model.cost.cache_write = cache_write;
        model
    }

    #[test]
    fn standard_pricing_requires_anthropic_model_and_exact_ratio() {
        assert!(has_standard_anthropic_cache_pricing(&model("anthropic", "claude-x", 3.0, 3.75)));
        assert!(has_standard_anthropic_cache_pricing(&model("other", "anthropic/claude", 3.0, 3.75)));
        assert!(has_standard_anthropic_cache_pricing(&model("other", "claude-3", 3.0, 3.75)));
        assert!(!has_standard_anthropic_cache_pricing(&model("openai", "gpt-5", 3.0, 3.75)));
        assert!(!has_standard_anthropic_cache_pricing(&model("anthropic", "claude-x", 3.0, 3.0)));
    }

    #[test]
    fn cache_costs_use_duration_multipliers() {
        let five_minutes = get_anthropic_cache_costs(3.0, ANTHROPIC_CACHE_DURATION_5M);
        assert_eq!(five_minutes.cache_read, 0.3);
        assert_eq!(five_minutes.cache_write, 3.75);

        let one_hour = get_anthropic_cache_costs(3.0, ANTHROPIC_CACHE_DURATION_1H);
        assert_eq!(one_hour.cache_write, 6.0);
    }

    #[test]
    fn cache_write_cost_weights_ephemeral_tokens() {
        assert_eq!(get_anthropic_cache_write_cost(3.0, ANTHROPIC_CACHE_DURATION_5M, None), 3.75);
        assert_eq!(
            get_anthropic_cache_write_cost(
                3.0,
                ANTHROPIC_CACHE_DURATION_5M,
                Some(&AnthropicCacheCreationUsage {
                    ephemeral_5m_input_tokens: 0.0,
                    ephemeral_1h_input_tokens: 0.0,
                })
            ),
            3.75
        );
        // all one-hour tokens -> 2x multiplier
        assert_eq!(
            get_anthropic_cache_write_cost(
                1.0,
                ANTHROPIC_CACHE_DURATION_5M,
                Some(&AnthropicCacheCreationUsage {
                    ephemeral_5m_input_tokens: 0.0,
                    ephemeral_1h_input_tokens: 100.0,
                })
            ),
            2.0
        );
        // half and half -> (1.25 + 2) / 2
        assert_eq!(
            get_anthropic_cache_write_cost(
                1.0,
                ANTHROPIC_CACHE_DURATION_5M,
                Some(&AnthropicCacheCreationUsage {
                    ephemeral_5m_input_tokens: 100.0,
                    ephemeral_1h_input_tokens: 100.0,
                })
            ),
            1.625
        );
    }
}
