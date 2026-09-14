//! Port of packages/coding-agent/src/core/usage.ts

use pi_ai::types::Usage;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionUsageSummary {
    pub input_tokens: f64,
    pub output_tokens: f64,
    pub cost: f64,
}

/// Returns `undefined` when every counter is zero, matching the TypeScript.
pub fn session_usage_summary_from(usage: &Usage) -> Option<SessionUsageSummary> {
    let input_tokens = usage.input + usage.cache_read + usage.cache_write;
    if input_tokens == 0.0 && usage.output == 0.0 && usage.cost.total == 0.0 {
        return None;
    }
    Some(SessionUsageSummary {
        input_tokens,
        output_tokens: usage.output,
        cost: usage.cost.total,
    })
}

pub fn empty_usage() -> Usage {
    Usage::zero()
}

pub fn add_assistant_usage(total: &mut Usage, usage: &Usage) {
    total.input += usage.input;
    total.output += usage.output;
    total.cache_read += usage.cache_read;
    total.cache_write += usage.cache_write;
    total.total_tokens += usage.total_tokens;
    total.cost.input += usage.cost.input;
    total.cost.output += usage.cost.output;
    total.cost.cache_read += usage.cost.cache_read;
    total.cost.cache_write += usage.cost.cache_write;
    total.cost.total += usage.cost.total;
}

/// Remove a previously added usage, clamping at zero to absorb attribution drift.
pub fn subtract_assistant_usage(total: &mut Usage, usage: &Usage) {
    total.input = (total.input - usage.input).max(0.0);
    total.output = (total.output - usage.output).max(0.0);
    total.cache_read = (total.cache_read - usage.cache_read).max(0.0);
    total.cache_write = (total.cache_write - usage.cache_write).max(0.0);
    total.total_tokens = (total.total_tokens - usage.total_tokens).max(0.0);
    total.cost.input = (total.cost.input - usage.cost.input).max(0.0);
    total.cost.output = (total.cost.output - usage.cost.output).max(0.0);
    total.cost.cache_read = (total.cost.cache_read - usage.cost.cache_read).max(0.0);
    total.cost.cache_write = (total.cost.cache_write - usage.cost.cache_write).max(0.0);
    total.cost.total = (total.cost.total - usage.cost.total).max(0.0);
}

pub fn clone_usage(usage: &Usage) -> Usage {
    usage.clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usage(input: f64, output: f64, cache_read: f64, cache_write: f64, total: f64) -> Usage {
        let mut value = empty_usage();
        value.input = input;
        value.output = output;
        value.cache_read = cache_read;
        value.cache_write = cache_write;
        value.total_tokens = total;
        value.cost.total = total;
        value
    }

    #[test]
    fn summary_is_none_when_everything_is_zero() {
        assert_eq!(session_usage_summary_from(&empty_usage()), None);
    }

    #[test]
    fn summary_sums_cache_into_input_tokens() {
        let summary = session_usage_summary_from(&usage(1.0, 2.0, 3.0, 4.0, 5.0)).unwrap();
        assert_eq!(summary.input_tokens, 8.0);
        assert_eq!(summary.output_tokens, 2.0);
        assert_eq!(summary.cost, 5.0);
    }

    #[test]
    fn add_and_subtract_are_inverse_and_clamp_at_zero() {
        let mut total = empty_usage();
        add_assistant_usage(&mut total, &usage(10.0, 5.0, 1.0, 1.0, 17.0));
        assert_eq!(total.input, 10.0);
        subtract_assistant_usage(&mut total, &usage(10.0, 5.0, 1.0, 1.0, 17.0));
        assert_eq!(total, empty_usage());
        subtract_assistant_usage(&mut total, &usage(1.0, 1.0, 1.0, 1.0, 1.0));
        assert_eq!(total, empty_usage());
    }
}
