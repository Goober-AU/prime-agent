//! Port of packages/coding-agent/src/core/rlm-max-depth.ts
//!
//! Wire-safe types for the immediate /rlm-max-depth state APIs.

use serde::{Deserialize, Serialize};

pub type RlmMaxDepthSource = String;

pub const RLM_MAX_DEPTH_SOURCE_DEFAULT: &str = "default";
pub const RLM_MAX_DEPTH_SOURCE_ENV: &str = "env";
pub const RLM_MAX_DEPTH_SOURCE_GLOBAL: &str = "global";
pub const RLM_MAX_DEPTH_SOURCE_INHERITED: &str = "inherited";
pub const RLM_MAX_DEPTH_SOURCE_CHAT: &str = "chat";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RlmMaxDepthStatus {
    pub max_depth: f64,
    pub source: RlmMaxDepthSource,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetRlmMaxDepthResult {
    pub max_depth: f64,
    pub source: RlmMaxDepthSource,
    pub global_saved: bool,
    /// `globalError?: string` - absent when the global save succeeded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub global_error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_serializes_camel_case_and_omits_absent_error() {
        let result = SetRlmMaxDepthResult {
            max_depth: 3.0,
            source: RLM_MAX_DEPTH_SOURCE_CHAT.to_string(),
            global_saved: false,
            global_error: None,
        };
        assert_eq!(
            serde_json::to_string(&result).unwrap(),
            r#"{"maxDepth":3.0,"source":"chat","globalSaved":false}"#
        );
        let with_error = SetRlmMaxDepthResult {
            global_error: Some("boom".to_string()),
            ..result
        };
        assert!(serde_json::to_string(&with_error).unwrap().contains(r#""globalError":"boom""#));
    }
}
