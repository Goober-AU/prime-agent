//! Port of packages/coding-agent/src/core/websearch-credential.ts
//!
//! Shared identifiers for the bundled websearch skill's Serper credential, used
//! by the /login UI (auth.json key) and the kernel env injection.

pub const WEBSEARCH_SKILL_NAME: &str = "websearch";
pub const SERPER_CREDENTIAL_ID: &str = "serper";
pub const SERPER_CREDENTIAL_NAME: &str = "Serper (web search)";
pub const SERPER_ENV_VAR: &str = "SERPER_API_KEY";
