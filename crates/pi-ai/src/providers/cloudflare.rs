//! Port of packages/ai/src/providers/cloudflare.ts
use crate::types::Model;

pub const CLOUDFLARE_WORKERS_AI_BASE_URL: &str =
	"https://api.cloudflare.com/client/v4/accounts/{CLOUDFLARE_ACCOUNT_ID}/ai/v1";

/// AI Gateway Unified API. https://developers.cloudflare.com/ai-gateway/usage/unified-api/
pub const CLOUDFLARE_AI_GATEWAY_COMPAT_BASE_URL: &str =
	"https://gateway.ai.cloudflare.com/v1/{CLOUDFLARE_ACCOUNT_ID}/{CLOUDFLARE_GATEWAY_ID}/compat";

pub const CLOUDFLARE_AI_GATEWAY_OPENAI_BASE_URL: &str =
	"https://gateway.ai.cloudflare.com/v1/{CLOUDFLARE_ACCOUNT_ID}/{CLOUDFLARE_GATEWAY_ID}/openai";

pub const CLOUDFLARE_AI_GATEWAY_ANTHROPIC_BASE_URL: &str =
	"https://gateway.ai.cloudflare.com/v1/{CLOUDFLARE_ACCOUNT_ID}/{CLOUDFLARE_GATEWAY_ID}/anthropic";

/// TS: `isCloudflareProvider(provider)`
pub fn is_cloudflare_provider(provider: &str) -> bool {
	provider == "cloudflare-workers-ai" || provider == "cloudflare-ai-gateway"
}

/// Substitute `{VAR}` placeholders in a Cloudflare baseUrl from process.env.
///
/// TS: `resolveCloudflareBaseUrl(model)`
pub fn resolve_cloudflare_base_url(model: &Model) -> Result<String, String> {
	let url = &model.base_url;
	if !url.contains('{') {
		return Ok(url.clone());
	}
	let mut out = String::new();
	let chars: Vec<char> = url.chars().collect();
	let mut index = 0usize;
	while index < chars.len() {
		let ch = chars[index];
		if ch == '{' {
			// Match /\{([A-Z_][A-Z0-9_]*)\}/ at this position.
			if let Some(close) = chars[index + 1..].iter().position(|c| *c == '}') {
				let name: String = chars[index + 1..index + 1 + close].iter().collect();
				if is_env_var_name(&name) {
					match std::env::var(&name) {
						Ok(value) if !value.is_empty() => {
							out.push_str(&value);
							index = index + close + 2;
							continue;
						}
						_ => {
							return Err(format!(
								"{} is required for provider {} but is not set.",
								name, model.provider
							))
						}
					}
				}
			}
		}
		out.push(ch);
		index += 1;
	}
	Ok(out)
}

/// `[A-Z_][A-Z0-9_]*`
fn is_env_var_name(name: &str) -> bool {
	let mut chars = name.chars();
	match chars.next() {
		Some(first) if first == '_' || first.is_ascii_uppercase() => {}
		_ => return false,
	}
	chars.all(|c| c == '_' || c.is_ascii_uppercase() || c.is_ascii_digit())
}

#[cfg(test)]
mod tests {
	use super::*;

	fn model(provider: &str, base_url: &str) -> Model {
		Model {
			provider: provider.to_string(),
			base_url: base_url.to_string(),
			..Default::default()
		}
	}

	#[test]
	fn base_url_constants_match_typescript() {
		assert_eq!(
			CLOUDFLARE_WORKERS_AI_BASE_URL,
			"https://api.cloudflare.com/client/v4/accounts/{CLOUDFLARE_ACCOUNT_ID}/ai/v1"
		);
		assert_eq!(
			CLOUDFLARE_AI_GATEWAY_COMPAT_BASE_URL,
			"https://gateway.ai.cloudflare.com/v1/{CLOUDFLARE_ACCOUNT_ID}/{CLOUDFLARE_GATEWAY_ID}/compat"
		);
		assert_eq!(
			CLOUDFLARE_AI_GATEWAY_OPENAI_BASE_URL,
			"https://gateway.ai.cloudflare.com/v1/{CLOUDFLARE_ACCOUNT_ID}/{CLOUDFLARE_GATEWAY_ID}/openai"
		);
		assert_eq!(
			CLOUDFLARE_AI_GATEWAY_ANTHROPIC_BASE_URL,
			"https://gateway.ai.cloudflare.com/v1/{CLOUDFLARE_ACCOUNT_ID}/{CLOUDFLARE_GATEWAY_ID}/anthropic"
		);
	}

	#[test]
	fn is_cloudflare_provider_matches_both_providers() {
		assert!(is_cloudflare_provider("cloudflare-workers-ai"));
		assert!(is_cloudflare_provider("cloudflare-ai-gateway"));
		assert!(!is_cloudflare_provider("anthropic"));
	}

	#[test]
	fn resolve_returns_url_unchanged_without_placeholder() {
		let resolved = resolve_cloudflare_base_url(&model("cloudflare-workers-ai", "https://example.com/v1")).unwrap();
		assert_eq!(resolved, "https://example.com/v1");
	}

	#[test]
	fn resolve_substitutes_environment_variables() {
		std::env::set_var("CLOUDFLARE_ACCOUNT_ID_TEST_ONLY", "acct");
		let resolved =
			resolve_cloudflare_base_url(&model("cloudflare-workers-ai", "https://x/{CLOUDFLARE_ACCOUNT_ID_TEST_ONLY}/v1"))
				.unwrap();
		assert_eq!(resolved, "https://x/acct/v1");
		std::env::remove_var("CLOUDFLARE_ACCOUNT_ID_TEST_ONLY");
	}

	#[test]
	fn resolve_errors_when_variable_missing() {
		std::env::remove_var("CLOUDFLARE_MISSING_TEST_ONLY");
		let error =
			resolve_cloudflare_base_url(&model("cloudflare-ai-gateway", "https://x/{CLOUDFLARE_MISSING_TEST_ONLY}/v1"))
				.unwrap_err();
		assert_eq!(
			error,
			"CLOUDFLARE_MISSING_TEST_ONLY is required for provider cloudflare-ai-gateway but is not set."
		);
	}

	#[test]
	fn resolve_leaves_non_env_placeholders_alone() {
		let resolved = resolve_cloudflare_base_url(&model("cloudflare-ai-gateway", "https://x/{lowercase}/v1")).unwrap();
		assert_eq!(resolved, "https://x/{lowercase}/v1");
	}
}
