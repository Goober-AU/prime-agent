//! Port of packages/coding-agent/src/core/model-tool-output-policy.ts

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use pi_agent_core::types::AgentMessage;
use pi_ai::types::ImageOrTextContent;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::utils::atomic_file::{write_file_atomic_sync, WriteFileAtomicOptions};

pub const MODEL_TOOL_OUTPUT_POLICY_ENV: &str = "PRIME_AGENT_MODEL_TOOL_OUTPUT_POLICY";
pub const MODEL_TOOL_OUTPUT_POLICY_OFF: &str = "off";
pub const REPEATED_LARGE_TEXT_POLICY: &str = "repeated-large-text-v1";
pub const MODEL_TOOL_OUTPUT_MIN_BYTES: usize = 16 * 1024;
const ARTIFACT_DIRECTORY: &str = "model-tool-output-v1";

/// `type ModelToolOutputPolicy = "off" | "repeated-large-text-v1"`.
pub type ModelToolOutputPolicy = &'static str;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelToolOutputArtifactV1 {
    pub version: i64,
    #[serde(rename = "artifactId")]
    pub artifact_id: String,
    #[serde(rename = "sessionId")]
    pub session_id: String,
    #[serde(rename = "filePath")]
    pub file_path: String,
    pub sha256: String,
    #[serde(rename = "sizeBytes")]
    pub size_bytes: f64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModelToolOutputScope {
    pub session_id: String,
    pub session_artifact_dir: String,
}

#[derive(Debug, Clone, Default)]
pub struct ModelToolOutputPolicyOptions {
    pub policy: Option<ModelToolOutputPolicy>,
    pub scope: Option<ModelToolOutputScope>,
}

fn is_record(value: &Value) -> Option<&serde_json::Map<String, Value>> {
    value.as_object()
}

fn sha256(text: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn sha256_bytes(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

fn artifact_directory(scope: &ModelToolOutputScope) -> PathBuf {
    resolve(&scope.session_artifact_dir).join(ARTIFACT_DIRECTORY)
}

fn expected_artifact_path(scope: &ModelToolOutputScope, digest: &str) -> PathBuf {
    artifact_directory(scope).join(format!("{}.txt", digest))
}

fn resolve(value: &str) -> PathBuf {
    let path = PathBuf::from(value);
    if path.is_absolute() {
        normalize(&path)
    } else {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        normalize(&cwd.join(path))
    }
}

/// `path.resolve()` normalisation without touching the filesystem.
fn normalize(path: &Path) -> PathBuf {
    let mut result = PathBuf::new();
    for component in path.components() {
        use std::path::Component;
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !result.pop() {
                    result.push("..");
                }
            }
            other => result.push(other.as_os_str()),
        }
    }
    result
}

fn assert_safe_scope(scope: &ModelToolOutputScope) -> Result<(), String> {
    if scope.session_id.trim().is_empty() {
        return Err("Tool-output artifact session id is empty".to_string());
    }
    if scope.session_artifact_dir.trim().is_empty() {
        return Err("Tool-output artifact directory is empty".to_string());
    }
    Ok(())
}

/// `resolveModelToolOutputPolicy(value = process.env[...])`.
pub fn resolve_model_tool_output_policy(value: Option<&str>) -> ModelToolOutputPolicy {
    let value = match value {
        Some(value) => Some(value.to_string()),
        None => std::env::var(MODEL_TOOL_OUTPUT_POLICY_ENV).ok(),
    };
    if value.as_deref() == Some(REPEATED_LARGE_TEXT_POLICY) {
        REPEATED_LARGE_TEXT_POLICY
    } else {
        MODEL_TOOL_OUTPUT_POLICY_OFF
    }
}

fn is_hex_digest(value: &str) -> bool {
    static DIGEST: once_cell::sync::Lazy<Regex> =
        once_cell::sync::Lazy::new(|| Regex::new("^[a-f0-9]{64}$").expect("valid regex literal"));
    DIGEST.is_match(value)
}

pub fn is_model_tool_output_artifact(value: &Value) -> bool {
    let Some(object) = is_record(value) else {
        return false;
    };
    let version_ok = object.get("version").and_then(Value::as_i64) == Some(1);
    let artifact_id_ok = object
        .get("artifactId")
        .and_then(Value::as_str)
        .map(|value| !value.is_empty())
        .unwrap_or(false);
    let session_id_ok = object
        .get("sessionId")
        .and_then(Value::as_str)
        .map(|value| !value.is_empty())
        .unwrap_or(false);
    let file_path_ok = object
        .get("filePath")
        .and_then(Value::as_str)
        .map(|value| !value.is_empty())
        .unwrap_or(false);
    let sha_ok = object
        .get("sha256")
        .and_then(Value::as_str)
        .map(is_hex_digest)
        .unwrap_or(false);
    let size_ok = match object.get("sizeBytes") {
        Some(Value::Number(number)) => number
            .as_f64()
            .map(|value| value.is_finite() && value.fract() == 0.0 && value >= 0.0)
            .unwrap_or(false),
        _ => false,
    };
    version_ok && artifact_id_ok && session_id_ok && file_path_ok && sha_ok && size_ok
}

fn assert_artifact_envelope(
    reference: &ModelToolOutputArtifactV1,
    scope: &ModelToolOutputScope,
) -> Result<PathBuf, String> {
    assert_safe_scope(scope)?;
    if reference.session_id != scope.session_id {
        return Err("Tool-output artifact belongs to another session".to_string());
    }
    if reference.artifact_id != format!("tool-output-v1:{}", reference.sha256) {
        return Err("Tool-output artifact id does not match its digest".to_string());
    }
    let root = artifact_directory(scope);
    let expected = expected_artifact_path(scope, &reference.sha256);
    if resolve(&reference.file_path) != expected
        || expected.parent() != Some(root.as_path())
        || expected.file_name().map(|name| name.to_string_lossy().to_string())
            != Some(format!("{}.txt", reference.sha256))
    {
        return Err("Tool-output artifact path is outside the current session scope".to_string());
    }
    let relative = expected.strip_prefix(&root).ok();
    match relative {
        Some(relative)
            if !relative.as_os_str().is_empty()
                && !relative.to_string_lossy().starts_with("..")
                && root.join(relative) == expected => {}
        _ => return Err("Tool-output artifact path failed containment validation".to_string()),
    }
    Ok(expected)
}

fn assert_session_artifact_root(scope: &ModelToolOutputScope) -> Result<PathBuf, String> {
    let session_root = resolve(&scope.session_artifact_dir);
    let metadata = std::fs::symlink_metadata(&session_root)
        .map_err(|error| error.to_string())?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err("Tool-output session artifact root is not a regular directory".to_string());
    }
    std::fs::canonicalize(&session_root).map_err(|error| error.to_string())
}

fn assert_artifact_root(scope: &ModelToolOutputScope) -> Result<PathBuf, String> {
    let session_real = assert_session_artifact_root(scope)?;
    let artifact_root = artifact_directory(scope);
    let metadata = std::fs::symlink_metadata(&artifact_root).map_err(|error| error.to_string())?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err("Tool-output artifact root is not a regular directory".to_string());
    }
    let artifact_real = std::fs::canonicalize(&artifact_root).map_err(|error| error.to_string())?;
    if artifact_real.parent() != Some(session_real.as_path()) {
        return Err("Tool-output artifact root resolved outside the current session".to_string());
    }
    Ok(artifact_real)
}

pub fn read_model_tool_output_artifact(
    reference: &ModelToolOutputArtifactV1,
    scope: &ModelToolOutputScope,
) -> Result<String, String> {
    let expected = assert_artifact_envelope(reference, scope)?;
    let root_real = assert_artifact_root(scope)?;
    let metadata = std::fs::symlink_metadata(&expected).map_err(|error| error.to_string())?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err("Tool-output artifact is not a regular file".to_string());
    }
    if metadata.len() as f64 != reference.size_bytes {
        return Err("Tool-output artifact size check failed".to_string());
    }
    let file_real = std::fs::canonicalize(&expected).map_err(|error| error.to_string())?;
    if file_real.parent() != Some(root_real.as_path()) {
        return Err("Tool-output artifact resolved outside the current session scope".to_string());
    }
    let bytes = std::fs::read(&file_real).map_err(|error| error.to_string())?;
    if bytes.len() as f64 != reference.size_bytes || sha256_bytes(&bytes) != reference.sha256 {
        return Err("Tool-output artifact identity check failed".to_string());
    }
    String::from_utf8(bytes).map_err(|error| error.to_string())
}

pub fn persist_model_tool_output_artifact(
    text: &str,
    scope: &ModelToolOutputScope,
) -> Result<ModelToolOutputArtifactV1, String> {
    assert_safe_scope(scope)?;
    let digest = sha256(text);
    let root = artifact_directory(scope);
    let file_path = expected_artifact_path(scope, &digest);
    // Validate the session root before creating a child so a symlink/reparse-point
    // scope cannot make even the directory creation escape the session.
    assert_session_artifact_root(scope)?;
    create_dir_mode(&root, 0o700).map_err(|error| error.to_string())?;
    assert_artifact_root(scope)?;
    let reference = ModelToolOutputArtifactV1 {
        version: 1,
        artifact_id: format!("tool-output-v1:{}", digest),
        session_id: scope.session_id.clone(),
        file_path: file_path.to_string_lossy().to_string(),
        sha256: digest,
        size_bytes: text.len() as f64,
    };
    match read_model_tool_output_artifact(&reference, scope) {
        Ok(existing) => {
            if existing != text {
                return Err("Tool-output artifact digest collision".to_string());
            }
            return Ok(reference);
        }
        Err(error) => {
            let missing = std::fs::symlink_metadata(&file_path).is_err();
            if !missing {
                return Err(error);
            }
        }
    }
    let path_string = file_path.to_string_lossy().to_string();
    write_file_atomic_sync(
        &path_string,
        text,
        WriteFileAtomicOptions {
            mode: Some(0o600),
            fsync: true,
            fsync_dir: true,
        },
    )
    .map_err(|error| error.to_string())?;
    if read_model_tool_output_artifact(&reference, scope)? != text {
        return Err("Tool-output artifact verification failed after commit".to_string());
    }
    Ok(reference)
}

fn create_dir_mode(path: &Path, mode: u32) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true).mode(mode);
        builder.create(path)
    }
    #[cfg(not(unix))]
    {
        let _ = mode;
        std::fs::create_dir_all(path)
    }
}

fn tool_artifact_from_message(message: &AgentMessage) -> Option<ModelToolOutputArtifactV1> {
    let AgentMessage::Message(pi_ai::types::Message::ToolResult(tool_result)) = message else {
        return None;
    };
    let details = tool_result.details.as_ref()?;
    let object = is_record(details)?;
    let value = object.get("modelOutputArtifact")?;
    if !is_model_tool_output_artifact(value) {
        return None;
    }
    serde_json::from_value(value.clone()).ok()
}

struct RepeatedTextCandidate {
    text: String,
    tool_call_id: String,
    artifact: ModelToolOutputArtifactV1,
}

fn repeated_text_candidate(message: &AgentMessage) -> Option<RepeatedTextCandidate> {
    let AgentMessage::Message(pi_ai::types::Message::ToolResult(tool_result)) = message else {
        return None;
    };
    if tool_result.tool_name != "ipython"
        || tool_result.is_error
        || tool_result.content.len() != 1
    {
        return None;
    }
    let ImageOrTextContent::Text(text_content) = &tool_result.content[0] else {
        return None;
    };
    let details = tool_result
        .details
        .as_ref()
        .and_then(is_record)
        .map(|object| object.clone());
    let Some(details) = details else {
        return None;
    };
    let status_ok = details.get("status").and_then(Value::as_str) == Some("ok");
    let stderr_empty = match details.get("stderr") {
        Some(Value::String(value)) => value.is_empty(),
        _ => true,
    };
    let background_empty = match details.get("backgroundOutput") {
        Some(Value::String(value)) => value.is_empty(),
        _ => true,
    };
    let kernel_restarted = details.get("kernelRestarted") == Some(&Value::Bool(true));
    let has_diffs = matches!(details.get("diffs"), Some(Value::Array(items)) if !items.is_empty());
    let has_attachments =
        matches!(details.get("attachments"), Some(Value::Array(items)) if !items.is_empty());
    let has_messages =
        matches!(details.get("sentAgentMessages"), Some(Value::Array(items)) if !items.is_empty());
    if !status_ok
        || !stderr_empty
        || !background_empty
        || kernel_restarted
        || has_diffs
        || has_attachments
        || has_messages
    {
        return None;
    }
    let text = text_content.text.clone();
    let size_bytes = text.len();
    if size_bytes < MODEL_TOOL_OUTPUT_MIN_BYTES {
        return None;
    }
    let artifact = tool_artifact_from_message(message)?;
    if artifact.size_bytes != size_bytes as f64 || artifact.sha256 != sha256(&text) {
        return None;
    }
    Some(RepeatedTextCandidate {
        text,
        tool_call_id: tool_result.tool_call_id.clone(),
        artifact,
    })
}

fn repeated_output_notice(candidate: &RepeatedTextCandidate) -> String {
    let reference = &candidate.artifact;
    format!(
        "<repeated_tool_output version=\"1\">\nThe IPython cell executed normally. Its text result is byte-for-byte identical to tool call {}; no execution was cached or skipped.\nThe complete result is stored in this session-scoped artifact:\nartifact_id: {}\nsession_id: {}\npath: {}\nsha256: {}\nsize_bytes: {}\nTo recover any omitted section, use IPython to read the exact path, then verify both SHA-256 and byte size before trusting the content. A missing or mismatched artifact is a retrieval error, not an empty successful result.\n</repeated_tool_output>",
        serde_json::to_string(&candidate.tool_call_id).unwrap_or_default(),
        reference.artifact_id,
        serde_json::to_string(&reference.session_id).unwrap_or_default(),
        serde_json::to_string(&reference.file_path).unwrap_or_default(),
        reference.sha256,
        reference.size_bytes
    )
}

/// Reduce only later, byte-identical, large successful IPython text results.
/// Every tool result remains in place with its original call id. The first copy
/// and every error/image/notice remain unchanged. Missing artifacts fail open to
/// the full transcript text instead of producing an unusable reference.
pub fn apply_model_tool_output_policy(
    messages: &[AgentMessage],
    options: &ModelToolOutputPolicyOptions,
) -> Vec<AgentMessage> {
    let policy = options
        .policy
        .unwrap_or_else(|| resolve_model_tool_output_policy(None));
    let Some(scope) = options.scope.as_ref() else {
        return messages.to_vec();
    };
    if policy != REPEATED_LARGE_TEXT_POLICY {
        return messages.to_vec();
    }
    let mut seen: HashMap<String, Arc<RepeatedTextCandidate>> = HashMap::new();
    let mut result: Vec<AgentMessage> = Vec::with_capacity(messages.len());
    for message in messages {
        let candidate = match repeated_text_candidate(message) {
            Some(candidate) => candidate,
            None => {
                result.push(message.clone());
                continue;
            }
        };
        let key = format!("{}:{}", candidate.artifact.sha256, candidate.artifact.size_bytes);
        let previous = match seen.get(&key) {
            Some(previous) => previous.clone(),
            None => {
                seen.insert(key, Arc::new(candidate));
                result.push(message.clone());
                continue;
            }
        };
        if previous.text != candidate.text {
            result.push(message.clone());
            continue;
        }
        let ok = read_model_tool_output_artifact(&previous.artifact, scope)
            .map(|value| value == previous.text)
            .unwrap_or(false)
            && read_model_tool_output_artifact(&candidate.artifact, scope)
                .map(|value| value == candidate.text)
                .unwrap_or(false);
        if !ok {
            result.push(message.clone());
            continue;
        }
        let AgentMessage::Message(pi_ai::types::Message::ToolResult(tool_result)) = message else {
            result.push(message.clone());
            continue;
        };
        let mut updated = tool_result.clone();
        updated.content = vec![ImageOrTextContent::Text(pi_ai::types::TextContent {
            content_type: pi_ai::types::TEXT_CONTENT_TYPE.to_string(),
            text: repeated_output_notice(&previous),
        })];
        result.push(AgentMessage::Message(pi_ai::types::Message::ToolResult(updated)));
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_ai::types::{Message, TextContent, ToolResultMessage, TEXT_CONTENT_TYPE};
    use serde_json::json;

    fn scope(dir: &Path) -> ModelToolOutputScope {
        ModelToolOutputScope {
            session_id: "session-1".to_string(),
            session_artifact_dir: dir.to_string_lossy().to_string(),
        }
    }

    fn large_text() -> String {
        "x".repeat(MODEL_TOOL_OUTPUT_MIN_BYTES + 1)
    }

    fn ipython_message(text: &str, artifact: &ModelToolOutputArtifactV1, call_id: &str) -> AgentMessage {
        let mut message = ToolResultMessage::new(
            call_id,
            "ipython",
            vec![ImageOrTextContent::Text(TextContent::new(text))],
            false,
            1,
        );
        message.details = Some(json!({
            "status": "ok",
            "modelOutputArtifact": serde_json::to_value(artifact).unwrap()
        }));
        AgentMessage::Message(Message::ToolResult(message))
    }

    #[test]
    fn policy_resolution_defaults_to_off() {
        assert_eq!(resolve_model_tool_output_policy(Some("nonsense")), MODEL_TOOL_OUTPUT_POLICY_OFF);
        assert_eq!(resolve_model_tool_output_policy(Some("")), MODEL_TOOL_OUTPUT_POLICY_OFF);
        assert_eq!(
            resolve_model_tool_output_policy(Some(REPEATED_LARGE_TEXT_POLICY)),
            REPEATED_LARGE_TEXT_POLICY
        );
    }

    #[test]
    fn artifact_validation_matches_typescript() {
        let artifact = json!({
            "version": 1,
            "artifactId": "tool-output-v1:".to_string() + &"a".repeat(64),
            "sessionId": "s",
            "filePath": "/tmp/x.txt",
            "sha256": "a".repeat(64),
            "sizeBytes": 5
        });
        assert!(is_model_tool_output_artifact(&artifact));
        let mut bad = artifact.clone();
        bad["sha256"] = json!("short");
        assert!(!is_model_tool_output_artifact(&bad));
        let mut bad = artifact.clone();
        bad["sizeBytes"] = json!(-1);
        assert!(!is_model_tool_output_artifact(&bad));
        let mut bad = artifact.clone();
        bad["version"] = json!(2);
        assert!(!is_model_tool_output_artifact(&bad));
        assert!(!is_model_tool_output_artifact(&json!([])));
    }

    #[test]
    fn persist_and_read_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let scope = scope(dir.path());
        let text = large_text();
        let reference = persist_model_tool_output_artifact(&text, &scope).unwrap();
        assert_eq!(reference.version, 1);
        assert_eq!(reference.artifact_id, format!("tool-output-v1:{}", reference.sha256));
        assert_eq!(reference.size_bytes as usize, text.len());
        assert_eq!(read_model_tool_output_artifact(&reference, &scope).unwrap(), text);
        // Idempotent: persisting the same text again returns the same reference.
        let again = persist_model_tool_output_artifact(&text, &scope).unwrap();
        assert_eq!(again, reference);
    }

    #[test]
    fn envelope_rejects_foreign_sessions_and_paths() {
        let dir = tempfile::tempdir().unwrap();
        let scope = scope(dir.path());
        let reference = persist_model_tool_output_artifact(&large_text(), &scope).unwrap();

        let mut foreign = reference.clone();
        foreign.session_id = "other".to_string();
        assert_eq!(
            read_model_tool_output_artifact(&foreign, &scope).unwrap_err(),
            "Tool-output artifact belongs to another session"
        );

        let mut bad_id = reference.clone();
        bad_id.artifact_id = "tool-output-v1:deadbeef".to_string();
        assert_eq!(
            read_model_tool_output_artifact(&bad_id, &scope).unwrap_err(),
            "Tool-output artifact id does not match its digest"
        );

        let mut outside = reference.clone();
        outside.file_path = dir.path().join("elsewhere.txt").to_string_lossy().to_string();
        assert_eq!(
            read_model_tool_output_artifact(&outside, &scope).unwrap_err(),
            "Tool-output artifact path is outside the current session scope"
        );

        let mut wrong_size = reference.clone();
        wrong_size.size_bytes += 1.0;
        assert_eq!(
            read_model_tool_output_artifact(&wrong_size, &scope).unwrap_err(),
            "Tool-output artifact size check failed"
        );
    }

    #[test]
    fn empty_scope_is_rejected() {
        let scope = ModelToolOutputScope {
            session_id: " ".to_string(),
            session_artifact_dir: "x".to_string(),
        };
        assert_eq!(
            persist_model_tool_output_artifact("x", &scope).unwrap_err(),
            "Tool-output artifact session id is empty"
        );
    }

    #[test]
    fn policy_replaces_only_the_second_identical_result() {
        let dir = tempfile::tempdir().unwrap();
        let scope = scope(dir.path());
        let text = large_text();
        let reference = persist_model_tool_output_artifact(&text, &scope).unwrap();
        let first = ipython_message(&text, &reference, "call-1");
        let second = ipython_message(&text, &reference, "call-2");
        let messages = vec![first.clone(), second];
        let result = apply_model_tool_output_policy(
            &messages,
            &ModelToolOutputPolicyOptions {
                policy: Some(REPEATED_LARGE_TEXT_POLICY),
                scope: Some(scope),
            },
        );
        assert_eq!(result.len(), 2);
        assert_eq!(result[0], first);
        let AgentMessage::Message(Message::ToolResult(tool_result)) = &result[1] else {
            panic!("expected tool result");
        };
        let ImageOrTextContent::Text(text_content) = &tool_result.content[0] else {
            panic!("expected text");
        };
        assert!(text_content.text.starts_with("<repeated_tool_output version=\"1\">"));
        assert!(text_content.text.contains("tool call \"call-1\""));
        assert_eq!(tool_result.tool_call_id, "call-2");
    }

    #[test]
    fn policy_keeps_errors_small_results_and_missing_artifacts() {
        let dir = tempfile::tempdir().unwrap();
        let scope = scope(dir.path());
        let text = large_text();
        let reference = persist_model_tool_output_artifact(&text, &scope).unwrap();
        let first = ipython_message(&text, &reference, "call-1");

        // A missing artifact fails open.
        std::fs::remove_file(&reference.file_path).unwrap();
        let second = ipython_message(&text, &reference, "call-2");
        let result = apply_model_tool_output_policy(
            &[first.clone(), second.clone()],
            &ModelToolOutputPolicyOptions {
                policy: Some(REPEATED_LARGE_TEXT_POLICY),
                scope: Some(scope.clone()),
            },
        );
        assert_eq!(result, vec![first.clone(), second]);

        // Policy off leaves everything untouched.
        let result = apply_model_tool_output_policy(
            &[first.clone(), first.clone()],
            &ModelToolOutputPolicyOptions {
                policy: Some(MODEL_TOOL_OUTPUT_POLICY_OFF),
                scope: Some(scope),
            },
        );
        assert_eq!(result, vec![first.clone(), first]);
    }

    #[test]
    fn small_or_error_results_are_not_candidates() {
        let dir = tempfile::tempdir().unwrap();
        let scope = scope(dir.path());
        let small = "tiny";
        let reference = ModelToolOutputArtifactV1 {
            version: 1,
            artifact_id: format!("tool-output-v1:{}", sha256(small)),
            session_id: scope.session_id.clone(),
            file_path: expected_artifact_path(&scope, &sha256(small))
                .to_string_lossy()
                .to_string(),
            sha256: sha256(small),
            size_bytes: small.len() as f64,
        };
        assert!(repeated_text_candidate(&ipython_message(small, &reference, "call")).is_none());

        let mut message = ToolResultMessage::new(
            "call",
            "ipython",
            vec![ImageOrTextContent::Text(TextContent::new(large_text()))],
            true,
            1,
        );
        message.details = Some(json!({"status": "ok"}));
        assert!(repeated_text_candidate(&AgentMessage::Message(Message::ToolResult(message))).is_none());
    }

    #[test]
    fn text_content_construction_keeps_type_field() {
        let content = TextContent {
            content_type: TEXT_CONTENT_TYPE.to_string(),
            text: "x".to_string(),
        };
        assert_eq!(
            serde_json::to_value(ImageOrTextContent::Text(content)).unwrap(),
            json!({"type": "text", "text": "x"})
        );
    }
}
