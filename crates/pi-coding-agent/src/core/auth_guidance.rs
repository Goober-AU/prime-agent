//! Port of packages/coding-agent/src/core/auth-guidance.ts

use std::path::Path;

use regex::Regex;

const UNKNOWN_PROVIDER: &str = "unknown";
pub const LOGIN_RECOVERY_MESSAGE: &str = "Run /login to update credentials.";

/// `getDocsPath()` is owned by the config slice; this keeps the same shape
/// (`<packageDir>/docs`) without depending on a module outside this slice.
fn get_docs_path() -> String {
    let package_dir = std::env::var("PI_PACKAGE_DIR").unwrap_or_else(|_| {
        let manifest = env!("CARGO_MANIFEST_DIR");
        Path::new(manifest)
            .parent()
            .and_then(Path::parent)
            .map(|path| path.to_string_lossy().to_string())
            .unwrap_or_else(|| manifest.to_string())
    });
    let joined = Path::new(&package_dir).join("docs");
    std::fs::canonicalize(&joined)
        .map(|path| path.to_string_lossy().to_string())
        .unwrap_or_else(|_| joined.to_string_lossy().to_string())
}

pub fn get_provider_login_help() -> String {
    let docs = get_docs_path();
    [
        "Use /login to log into a provider via OAuth or API key. See:".to_string(),
        format!("  {}", Path::new(&docs).join("providers.md").to_string_lossy()),
        format!("  {}", Path::new(&docs).join("models.md").to_string_lossy()),
    ]
    .join("\n")
}

pub fn format_no_models_available_message() -> String {
    format!("No models available. {}", get_provider_login_help())
}

/// Whether a model fallback message is the "no models available" warning.
///
/// That warning is a claim about current state (no model could be resolved), so
/// consumers must re-check it against the live session before showing it; the
/// other fallback variants ("Could not restore model X. Using Y") are one-time
/// startup notices that stay valid.
pub fn is_no_models_available_message(message: Option<&str>) -> bool {
    message == Some(format_no_models_available_message().as_str())
}

pub fn format_no_model_selected_message() -> String {
    format!(
        "No model selected.\n\n{}\n\nThen use /model to select a model.",
        get_provider_login_help()
    )
}

pub fn format_no_api_key_found_message(provider: &str) -> String {
    let provider_display = if provider == UNKNOWN_PROVIDER {
        "the selected model"
    } else {
        provider
    };
    format!(
        "No API key found for {}.\n\n{}",
        provider_display,
        get_provider_login_help()
    )
}

pub fn format_authentication_failed_message(provider: &str) -> String {
    format!(
        "Authentication failed for \"{}\". Credentials may have expired or network is unavailable.\n\n{}",
        provider, LOGIN_RECOVERY_MESSAGE
    )
}

fn regex(pattern: &str) -> Regex {
    Regex::new(pattern).expect("valid regex literal")
}

pub fn is_likely_authentication_error(message: &str) -> bool {
    regex(r"(?i)\b(401|403)\b").is_match(message)
        || regex(r"(?i)unauthorized|forbidden|invalid[_ -]?api[_ -]?key|api key.*invalid").is_match(message)
        || regex(r"(?i)authentication failed|invalid authentication|missing authentication").is_match(message)
        || regex(r"(?i)(expired|invalid) token|token expired|access denied|permission denied").is_match(message)
}

pub fn add_login_guidance_to_auth_error(message: &str) -> String {
    if regex(r"/login\b").is_match(message) {
        return message.to_string();
    }
    format!("{}\n\n{}", message, LOGIN_RECOVERY_MESSAGE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_models_message_is_recognised() {
        let message = format_no_models_available_message();
        assert!(is_no_models_available_message(Some(&message)));
        assert!(!is_no_models_available_message(Some("other")));
        assert!(!is_no_models_available_message(None));
    }

    #[test]
    fn unknown_provider_reads_as_the_selected_model() {
        assert!(format_no_api_key_found_message("unknown").contains("No API key found for the selected model."));
        assert!(format_no_api_key_found_message("anthropic").contains("No API key found for anthropic."));
    }

    #[test]
    fn authentication_error_detection() {
        assert!(is_likely_authentication_error("HTTP 401 Unauthorized"));
        assert!(is_likely_authentication_error("invalid_api_key"));
        assert!(is_likely_authentication_error("token expired"));
        assert!(!is_likely_authentication_error("rate limited"));
    }

    #[test]
    fn login_guidance_is_added_once() {
        assert_eq!(add_login_guidance_to_auth_error("boom"), format!("boom\n\n{}", LOGIN_RECOVERY_MESSAGE));
        assert_eq!(add_login_guidance_to_auth_error("see /login"), "see /login");
    }
}
