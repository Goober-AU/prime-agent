//! Port of packages/coding-agent/src/core/source-info.ts

use serde::{Deserialize, Serialize};

/// `PathMetadata` from `core/package-manager.ts`.
///
/// `package-manager.ts` belongs to another slice; this local definition keeps
/// the dependency explicit until `pi_coding_agent::core::package_manager` lands,
/// at which point this becomes a re-export.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PathMetadata {
    pub source: String,
    pub scope: SourceScope,
    pub origin: SourceOrigin,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_dir: Option<String>,
}

pub type SourceScope = String;
pub type SourceOrigin = String;

pub const SOURCE_SCOPE_USER: &str = "user";
pub const SOURCE_SCOPE_PROJECT: &str = "project";
pub const SOURCE_SCOPE_TEMPORARY: &str = "temporary";

pub const SOURCE_ORIGIN_PACKAGE: &str = "package";
pub const SOURCE_ORIGIN_TOP_LEVEL: &str = "top-level";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceInfo {
    pub path: String,
    pub source: String,
    pub scope: SourceScope,
    pub origin: SourceOrigin,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_dir: Option<String>,
}

pub fn create_source_info(path: &str, metadata: &PathMetadata) -> SourceInfo {
    SourceInfo {
        path: path.to_string(),
        source: metadata.source.clone(),
        scope: metadata.scope.clone(),
        origin: metadata.origin.clone(),
        base_dir: metadata.base_dir.clone(),
    }
}

/// Options for [`create_synthetic_source_info`].
///
/// `scope?` and `origin?` are optional keys with defaults, so `None` is
/// "absent" and the defaults below are applied.
#[derive(Debug, Clone, Default)]
pub struct SyntheticSourceInfoOptions {
    pub source: String,
    pub scope: Option<SourceScope>,
    pub origin: Option<SourceOrigin>,
    pub base_dir: Option<String>,
}

pub fn create_synthetic_source_info(path: &str, options: &SyntheticSourceInfoOptions) -> SourceInfo {
    SourceInfo {
        path: path.to_string(),
        source: options.source.clone(),
        scope: options
            .scope
            .clone()
            .unwrap_or_else(|| SOURCE_SCOPE_TEMPORARY.to_string()),
        origin: options
            .origin
            .clone()
            .unwrap_or_else(|| SOURCE_ORIGIN_TOP_LEVEL.to_string()),
        base_dir: options.base_dir.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synthetic_source_info_defaults_scope_and_origin() {
        let info = create_synthetic_source_info(
            "/tmp/skill.md",
            &SyntheticSourceInfoOptions {
                source: "local".to_string(),
                ..Default::default()
            },
        );
        assert_eq!(info.scope, "temporary");
        assert_eq!(info.origin, "top-level");
        assert_eq!(info.base_dir, None);
    }

    #[test]
    fn create_source_info_copies_metadata() {
        let info = create_source_info(
            "/p",
            &PathMetadata {
                source: "npm:foo".to_string(),
                scope: SOURCE_SCOPE_USER.to_string(),
                origin: SOURCE_ORIGIN_PACKAGE.to_string(),
                base_dir: Some("/base".to_string()),
            },
        );
        assert_eq!(
            serde_json::to_string(&info).unwrap(),
            r#"{"path":"/p","source":"npm:foo","scope":"user","origin":"package","baseDir":"/base"}"#
        );
    }
}
