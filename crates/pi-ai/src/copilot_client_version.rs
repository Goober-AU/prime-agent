//! Port of packages/ai/src/copilot-client-version.ts

/// Client identity impersonated on GitHub Copilot requests. The generated
/// catalog bakes these into every Copilot row's headers, and the OAuth flow
/// sends them directly; keep both in sync by editing only this file.
pub const COPILOT_CLIENT_USER_AGENT: &str = "GitHubCopilotChat/0.48.1";

/// `COPILOT_CLIENT_HEADERS` as `(name, value)` pairs in declaration order.
pub const COPILOT_CLIENT_HEADERS: [(&str, &str); 4] = [
    ("User-Agent", COPILOT_CLIENT_USER_AGENT),
    ("Editor-Version", "vscode/1.136.1"),
    ("Editor-Plugin-Version", "copilot-chat/0.48.1"),
    ("Copilot-Integration-Id", "vscode-chat"),
];

/// `COPILOT_CLIENT_HEADERS` as an ordered map.
pub fn copilot_client_headers() -> indexmap::IndexMap<String, String> {
    COPILOT_CLIENT_HEADERS
        .iter()
        .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_values_match_typescript() {
        let headers = copilot_client_headers();
        assert_eq!(
            headers.get("User-Agent").map(String::as_str),
            Some("GitHubCopilotChat/0.48.1")
        );
        assert_eq!(
            headers.get("Editor-Version").map(String::as_str),
            Some("vscode/1.136.1")
        );
        assert_eq!(
            headers.get("Editor-Plugin-Version").map(String::as_str),
            Some("copilot-chat/0.48.1")
        );
        assert_eq!(
            headers.get("Copilot-Integration-Id").map(String::as_str),
            Some("vscode-chat")
        );
        assert_eq!(headers.len(), 4);
    }
}
