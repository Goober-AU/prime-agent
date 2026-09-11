//! Port of packages/ai/src/providers/opencode-headers.ts
use indexmap::IndexMap;

/// TS: `withOpenCodeHeaders(provider, sessionId, headers)`
///
/// The TS generic `T extends string | null` maps to `IndexMap<String, Option<String>>` so a
/// `null` header value stays distinguishable from an absent one.
pub fn with_opencode_headers(
	provider: &str,
	session_id: Option<&str>,
	headers: &IndexMap<String, Option<String>>,
) -> IndexMap<String, Option<String>> {
	if provider != "opencode" && provider != "opencode-go" {
		return headers.clone();
	}

	let mut merged: IndexMap<String, Option<String>> = IndexMap::new();
	merged.insert("User-Agent".to_string(), Some("prime-agent".to_string()));
	if let Some(session_id) = session_id {
		if !session_id.is_empty() {
			merged.insert("x-opencode-session".to_string(), Some(session_id.to_string()));
		}
	}
	for (name, value) in headers {
		// Keep the SDK's User-Agent casing so Google's SDK replaces its default.
		let key = name.to_lowercase();
		let out_key = if key == "user-agent" { "User-Agent".to_string() } else { key };
		merged.insert(out_key, value.clone());
	}
	merged
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn passes_headers_through_for_other_providers() {
		let mut headers = IndexMap::new();
		headers.insert("X-Test".to_string(), Some("1".to_string()));
		let merged = with_opencode_headers("anthropic", Some("session"), &headers);
		assert_eq!(merged.len(), 1);
		assert_eq!(merged.get("X-Test"), Some(&Some("1".to_string())));
	}

	#[test]
	fn adds_default_user_agent_and_session_header() {
		let headers = IndexMap::new();
		let merged = with_opencode_headers("opencode", Some("abc"), &headers);
		assert_eq!(merged.get("User-Agent"), Some(&Some("prime-agent".to_string())));
		assert_eq!(merged.get("x-opencode-session"), Some(&Some("abc".to_string())));
	}

	#[test]
	fn omits_session_header_when_missing() {
		let headers = IndexMap::new();
		let merged = with_opencode_headers("opencode-go", None, &headers);
		assert!(!merged.contains_key("x-opencode-session"));
	}

	#[test]
	fn overrides_user_agent_case_insensitively() {
		let mut headers = IndexMap::new();
		headers.insert("user-agent".to_string(), Some("custom".to_string()));
		let merged = with_opencode_headers("opencode", None, &headers);
		assert_eq!(merged.len(), 1);
		assert_eq!(merged.get("User-Agent"), Some(&Some("custom".to_string())));
	}

	#[test]
	fn preserves_null_header_values() {
		let mut headers = IndexMap::new();
		headers.insert("x-null".to_string(), None);
		let merged = with_opencode_headers("opencode", None, &headers);
		assert_eq!(merged.get("x-null"), Some(&None));
	}
}
