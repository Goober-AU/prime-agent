//! Port of packages/coding-agent/src/core/diagnostics.ts

use serde::{Deserialize, Serialize};

/// `resourceType: "extension" | "skill" | "prompt" | "theme"`.
pub type ResourceType = String;

pub const RESOURCE_TYPE_EXTENSION: &str = "extension";
pub const RESOURCE_TYPE_SKILL: &str = "skill";
pub const RESOURCE_TYPE_PROMPT: &str = "prompt";
pub const RESOURCE_TYPE_THEME: &str = "theme";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceCollision {
    pub resource_type: ResourceType,
    /// skill name, command/tool/flag name, prompt name, theme name
    pub name: String,
    pub winner_path: String,
    pub loser_path: String,
    /// e.g. "npm:foo", "git:...", "local"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub winner_source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub loser_source: Option<String>,
}

/// `type: "warning" | "error" | "collision"`.
pub type ResourceDiagnosticType = String;

pub const RESOURCE_DIAGNOSTIC_WARNING: &str = "warning";
pub const RESOURCE_DIAGNOSTIC_ERROR: &str = "error";
pub const RESOURCE_DIAGNOSTIC_COLLISION: &str = "collision";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceDiagnostic {
    #[serde(rename = "type")]
    pub diagnostic_type: ResourceDiagnosticType,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub collision: Option<ResourceCollision>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostic_omits_absent_optional_keys() {
        let diagnostic = ResourceDiagnostic {
            diagnostic_type: RESOURCE_DIAGNOSTIC_WARNING.to_string(),
            message: "m".to_string(),
            path: None,
            collision: None,
        };
        assert_eq!(
            serde_json::to_string(&diagnostic).unwrap(),
            r#"{"type":"warning","message":"m"}"#
        );
    }

    #[test]
    fn collision_round_trips_camel_case_fields() {
        let collision = ResourceCollision {
            resource_type: RESOURCE_TYPE_SKILL.to_string(),
            name: "websearch".to_string(),
            winner_path: "/a".to_string(),
            loser_path: "/b".to_string(),
            winner_source: Some("npm:foo".to_string()),
            loser_source: None,
        };
        let json = serde_json::to_string(&collision).unwrap();
        assert_eq!(
            json,
            r#"{"resourceType":"skill","name":"websearch","winnerPath":"/a","loserPath":"/b","winnerSource":"npm:foo"}"#
        );
        let back: ResourceCollision = serde_json::from_str(&json).unwrap();
        assert_eq!(back, collision);
    }
}
