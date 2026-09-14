//! Port of packages/ai/src/utils/overflow.ts
//!
//! The `OVERFLOW_PATTERNS` / `NON_OVERFLOW_PATTERNS` lists are kept in the same
//! order as the TypeScript. The regexes are matched with `regex` (all patterns
//! are ASCII + case-insensitive, which `regex` supports natively).

use std::sync::OnceLock;

use regex::RegexSet;

use crate::types::{AssistantMessage, STOP_REASON_ERROR, STOP_REASON_LENGTH, STOP_REASON_STOP};

/// Regex patterns to detect context overflow errors from different providers.
pub const OVERFLOW_PATTERNS: [&str; 21] = [
    r"(?i)prompt is too long",                            // Anthropic token overflow
    r"(?i)request_too_large",                             // Anthropic request byte-size overflow (HTTP 413)
    r"(?i)input is too long for requested model",         // Amazon Bedrock
    r"(?i)exceeds the context window",                    // OpenAI (Completions & Responses API)
    r"(?i)input token count.*exceeds the maximum",        // Google (Gemini)
    r"(?i)maximum prompt length is \d+",                  // xAI (Grok)
    r"(?i)reduce the length of the messages",             // Groq
    r"(?i)maximum context length is \d+ tokens",          // OpenRouter (all backends)
    r"(?i)exceeds the model's maximum context length",    // LiteLLM (input + requested output)
    r"(?i)exceeds the limit of \d+",                      // GitHub Copilot
    r"(?i)exceeds the available context size",            // llama.cpp server
    r"(?i)greater than the context length",               // LM Studio
    r"(?i)context window exceeds limit",                  // MiniMax
    r"(?i)exceeded model token limit",                    // Kimi For Coding
    r"(?i)too large for model with \d+ maximum context length", // Mistral
    r"(?i)model_context_window_exceeded",                 // z.ai non-standard finish_reason surfaced as error text
    r"(?i)prompt too long; exceeded (?:max )?context length", // Ollama explicit overflow error
    r"(?i)context[_ ]length[_ ]exceeded",                 // Generic fallback
    r"(?i)too many tokens",                               // Generic fallback
    r"(?i)token limit exceeded",                          // Generic fallback
    r"(?i)^4(?:00|13)\s*(?:status code)?\s*\(no body\)",  // Cerebras: 400/413 with no body
];

/// Patterns that indicate non-overflow errors (e.g. rate limiting, server errors).
/// Error messages matching any of these are excluded from overflow detection
/// even if they also match an OVERFLOW_PATTERN.
pub const NON_OVERFLOW_PATTERNS: [&str; 3] = [
    r"(?i)^(Throttling error|Service unavailable):", // AWS Bedrock non-overflow errors
    r"(?i)rate limit",                               // Generic rate limiting
    r"(?i)too many requests",                        // Generic HTTP 429 style
];

fn overflow_pattern_set() -> &'static RegexSet {
    static SET: OnceLock<RegexSet> = OnceLock::new();
    SET.get_or_init(|| RegexSet::new(OVERFLOW_PATTERNS).expect("overflow patterns compile"))
}

fn non_overflow_pattern_set() -> &'static RegexSet {
    static SET: OnceLock<RegexSet> = OnceLock::new();
    SET.get_or_init(|| RegexSet::new(NON_OVERFLOW_PATTERNS).expect("non-overflow patterns compile"))
}

/// Check if an assistant message represents a context overflow error.
///
/// This handles two cases:
/// 1. Error-based overflow: Most providers return stopReason "error" with a
///    specific error message pattern.
/// 2. Silent overflow: Some providers accept overflow requests and return
///    successfully. For these, we check if usage.input exceeds the context window.
pub fn is_context_overflow(message: &AssistantMessage, context_window: Option<f64>) -> bool {
    if message.stop_reason == STOP_REASON_ERROR {
        if let Some(error_message) = message.error_message.as_ref() {
            // Skip messages matching known non-overflow patterns (e.g. throttling / rate-limit)
            let is_non_overflow = non_overflow_pattern_set().is_match(error_message);
            if !is_non_overflow && overflow_pattern_set().is_match(error_message) {
                return true;
            }
        }
    }

    // overflow.ts:129 and overflow.ts:139 gate the usage-based checks on
    // `contextWindow` truthiness (`if (contextWindow && ...)`), so 0 is skipped like
    // `undefined`. A `Some(0.0)` here comes from `unwrap_or(0.0)`
    // (crates/pi-coding-agent/src/core/runtime_members.rs:2258-2259) and
    // agent_session.rs:13172; without the gate case 2 degenerates to `input > 0` and case 3
    // to `input >= 0`, which reports an overflow the TypeScript never reports.
    // Non-finite values are treated the same way because `NaN > x` is false anyway and
    // `>= NaN * 0.99` can never be a meaningful context window.
    let context_window = context_window.filter(|window| window.is_finite() && *window != 0.0);
    if let Some(context_window) = context_window {
        if message.stop_reason == STOP_REASON_STOP {
            let input_tokens = message.usage.input + message.usage.cache_read;
            if input_tokens > context_window {
                return true;
            }
        }

        // Case 3: Length-stop overflow (Xiaomi MiMo style) - server truncates oversized input
        // to fit the context window, leaving no room for output. Returns stopReason "length"
        // with output=0 and input+cacheRead filling the context window.
        if message.stop_reason == STOP_REASON_LENGTH && message.usage.output == 0.0 {
            let input_tokens = message.usage.input + message.usage.cache_read;
            if input_tokens >= context_window * 0.99 {
                return true;
            }
        }
    }

    false
}

/// TypeScript name kept for callers that use `isContextOverflow`.
pub fn is_context_overflow_error(message: &AssistantMessage, context_window: Option<f64>) -> bool {
    is_context_overflow(message, context_window)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Usage;

    const LITELLM_ERROR: &str = "400 litellm.BadRequestError: OpenAIException - Requested token count exceeds the model's maximum context length of 262144 tokens. You requested a total of 270128 tokens: 261936 tokens from the input messages and 8192 tokens for the completion. Please reduce the number of tokens in the input messages or the completion to fit within the limit.. Received Model Group=qwen3.8-27b-nvfp4";

    fn error_message(error_message: &str) -> AssistantMessage {
        let mut message = AssistantMessage::new("openai-completions", "ollama", "qwen3.5:35b", 0);
        message.stop_reason = STOP_REASON_ERROR.to_string();
        message.error_message = Some(error_message.to_string());
        message
    }

    fn length_stop_message(input: f64, cache_read: f64, output: f64) -> AssistantMessage {
        let mut message = AssistantMessage::new("openai-completions", "xiaomi", "mimo-v2.5-pro", 0);
        message.stop_reason = STOP_REASON_LENGTH.to_string();
        message.usage = Usage {
            input,
            output,
            cache_read,
            cache_write: 0.0,
            total_tokens: input + cache_read + output,
            cost: Default::default(),
        };
        message
    }

    #[test]
    fn detects_litellm_rejection_with_and_without_context_window() {
        assert!(is_context_overflow(&error_message(LITELLM_ERROR), Some(262144.0)));
        assert!(is_context_overflow(&error_message(LITELLM_ERROR), None));
    }

    #[test]
    fn detects_case_insensitive_rejection() {
        let message = error_message(
            "REQUESTED TOKEN COUNT EXCEEDS THE MODEL'S MAXIMUM CONTEXT LENGTH OF 262144 TOKENS.",
        );
        assert!(is_context_overflow(&message, None));
    }

    #[test]
    fn does_not_classify_unrelated_or_rate_limited_rejections() {
        let cases = [
            "429 rate limit: too many tokens",
            "Requested token count exceeds the per-minute token quota.",
            "400 litellm.BadRequestError: unsupported parameter: temperature",
        ];
        for error in cases {
            assert!(
                !is_context_overflow(&error_message(error), Some(262144.0)),
                "unexpected overflow for {}",
                error
            );
        }
        let prefixed = format!("429 rate limit: upstream previously returned {}", LITELLM_ERROR);
        assert!(!is_context_overflow(&error_message(&prefixed), Some(262144.0)));
    }

    #[test]
    fn ignores_context_error_text_for_other_stop_reasons() {
        for stop_reason in [STOP_REASON_STOP, "aborted"] {
            let mut message = error_message(LITELLM_ERROR);
            message.stop_reason = stop_reason.to_string();
            assert!(!is_context_overflow(&message, Some(262144.0)));
        }
    }

    #[test]
    fn detects_ollama_prompt_too_long_and_ignores_other_errors() {
        assert!(is_context_overflow(
            &error_message("400 `prompt too long; exceeded max context length by 100918 tokens`"),
            Some(32768.0)
        ));
        assert!(!is_context_overflow(
            &error_message("500 `model runner crashed unexpectedly`"),
            Some(32768.0)
        ));
    }

    #[test]
    fn non_overflow_patterns_win() {
        assert!(!is_context_overflow(
            &error_message("Throttling error: Too many tokens, please wait before trying again."),
            Some(200000.0)
        ));
        assert!(!is_context_overflow(
            &error_message("Service unavailable: The service is temporarily unavailable."),
            Some(200000.0)
        ));
        assert!(!is_context_overflow(
            &error_message("Rate limit exceeded, please retry after 30 seconds."),
            Some(200000.0)
        ));
        assert!(!is_context_overflow(
            &error_message("Too many requests. Please slow down."),
            Some(200000.0)
        ));
    }

    #[test]
    fn silent_overflow_on_stop_with_input_over_context() {
        let mut message = AssistantMessage::new("openai-completions", "zai", "glm", 0);
        message.stop_reason = STOP_REASON_STOP.to_string();
        message.usage.input = 300000.0;
        message.usage.cache_read = 1000.0;
        assert!(is_context_overflow(&message, Some(200000.0)));
        assert!(!is_context_overflow(&message, None));
    }

    #[test]
    fn zero_or_non_finite_context_window_skips_the_usage_checks() {
        // overflow.ts:129/139: `if (contextWindow && ...)` - a falsy context window (0) is
        // skipped exactly like `undefined`, so a 0-context custom model is never reported as
        // an overflow by the usage-based cases.
        let mut stop_message = AssistantMessage::new("openai-completions", "custom", "local", 0);
        stop_message.stop_reason = STOP_REASON_STOP.to_string();
        stop_message.usage.input = 300000.0;
        stop_message.usage.cache_read = 1000.0;
        assert!(is_context_overflow(&stop_message, Some(200000.0)));
        assert!(!is_context_overflow(&stop_message, None));
        assert!(!is_context_overflow(&stop_message, Some(0.0)));
        assert!(!is_context_overflow(&stop_message, Some(f64::NAN)));

        let length_message = length_stop_message(58.0, 0.0, 0.0);
        assert!(is_context_overflow(&length_message, Some(20.0)));
        assert!(!is_context_overflow(&length_message, Some(0.0)));
    }

    #[test]
    fn length_stop_overflow_detection() {
        assert!(is_context_overflow(&length_stop_message(58.0, 1048512.0, 0.0), Some(1048576.0)));
        assert!(!is_context_overflow(&length_stop_message(1000.0, 0.0, 4096.0), Some(200000.0)));
        assert!(!is_context_overflow(&length_stop_message(100.0, 0.0, 0.0), Some(200000.0)));
    }

    #[test]
    fn pattern_lists_keep_typescript_order() {
        assert_eq!(OVERFLOW_PATTERNS.len(), 21);
        assert_eq!(NON_OVERFLOW_PATTERNS.len(), 3);
        assert_eq!(overflow_pattern_set().patterns().len(), 21);
    }
}
