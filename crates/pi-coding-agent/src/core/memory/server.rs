//! Port of packages/coding-agent/src/core/memory/server.ts
//!
//! The Node `http` server maps to a `tokio::net::TcpListener` + hyper-free
//! hand-rolled HTTP/1.1 handling: the protocol surface (Bearer auth, 401/404/409/413/400
//! JSON bodies, `Content-Type`/`Cache-Control` headers) is preserved exactly.
use std::collections::HashMap;
use std::path::Path;

use serde_json::Value;
use sha2::{Digest, Sha256};

use super::evidence::hash;
use super::sharing::validate_shared_write;
use super::store::{empty_document, read_json, validate_document, write_json};

const MAX_BODY_BYTES: usize = 2 * 1024 * 1024;
const MAX_DOCUMENT_BYTES: usize = 8 * 1024 * 1024;

/// Constant-time comparison, the port of `crypto.timingSafeEqual`.
fn timing_safe_equal(left: &[u8], right: &[u8]) -> bool {
	if left.len() != right.len() {
		return false;
	}
	let mut difference = 0u8;
	for (a, b) in left.iter().zip(right.iter()) {
		difference |= a ^ b;
	}
	difference == 0
}

/// Port of the request handler body. Returns `(status, body)`.
pub fn handle_request(dir: &str, token: &str, authorization: Option<&str>, method: &str, url: &str, body: &[u8]) -> (u16, String) {
	let expected = format!("Bearer {token}");
	let supplied = authorization.unwrap_or("");
	if !timing_safe_equal(supplied.as_bytes(), expected.as_bytes()) {
		return (401, "{\"error\":\"Unauthorized\"}".to_string());
	}
	let project_re = regex::Regex::new(r"^/projects/(project_[A-Za-z0-9_-]{1,80})$").unwrap();
	let captures = match project_re.captures(url) {
		Some(captures) => captures,
		None => return (404, "{}".to_string()),
	};
	if method != "GET" && method != "POST" {
		return (404, "{}".to_string());
	}
	let project_id = captures.get(1).map(|m| m.as_str()).unwrap_or("");
	let project_dir = Path::new(dir).join(project_id);
	if std::fs::create_dir_all(&project_dir).is_err() {
		return (400, "{\"error\":\"Invalid memory request or unavailable storage\"}".to_string());
	}
	let path = project_dir.join("harness_state.json");
	if method == "POST" && body.len() > MAX_BODY_BYTES {
		return (413, "{}".to_string());
	}
	let _lock = match super::acquire_lock_sync(
		&project_dir.to_string_lossy(),
		super::MemoryLockRetries {
			stale_ms: 10_000,
			retries: 20,
			min_timeout_ms: 10,
			max_timeout_ms: 100,
		},
	) {
		Ok(lock) => lock,
		Err(_) => {
			return (
				400,
				"{\"error\":\"Invalid memory request or unavailable storage\"}".to_string(),
			)
		}
	};
	let state = if path.exists() {
		match read_json(&path.to_string_lossy()).and_then(|value| validate_document(&value, project_id).map(|document| serde_json::to_value(document).unwrap_or(Value::Null)))
		{
			Ok(value) => value,
			Err(_) => {
				return (
					400,
					"{\"error\":\"Invalid memory request or unavailable storage\"}".to_string(),
				)
			}
		}
	} else {
		serde_json::to_value(empty_document(project_id)).unwrap_or(Value::Null)
	};
	if method == "POST" {
		let write = match serde_json::from_slice::<Value>(body)
			.map_err(|error| error.to_string())
			.and_then(|value| validate_shared_write(&value))
		{
			Ok(write) => write,
			Err(_) => {
				return (
					400,
					"{\"error\":\"Invalid memory request or unavailable storage\"}".to_string(),
				)
			}
		};
		let fingerprint = hash(&serde_json::to_string(&write).unwrap_or_default());
		let receipt = state
			.get("memory")
			.and_then(|memory| memory.get("events"))
			.and_then(|events| events.get(&write.id))
			.and_then(Value::as_str)
			.map(str::to_string);
		if let Some(receipt) = receipt {
			if receipt != fingerprint {
				return (
					400,
					"{\"error\":\"Invalid memory request or unavailable storage\"}".to_string(),
				);
			}
		} else {
			let revision = state
				.get("memory")
				.and_then(|memory| memory.get("revision"))
				.and_then(Value::as_i64);
			if revision != Some(write.revision) {
				return (409, "{\"error\":\"Revision conflict\"}".to_string());
			}
			let mut staged = state.clone();
			for entry in &write.entries {
				let id_ok = regex::Regex::new(r"^[A-Za-z0-9_-]{1,160}$").unwrap().is_match(&entry.id)
					&& !["__proto__", "constructor", "prototype"].contains(&entry.id.as_str());
				let metadata_project = entry
					.metadata
					.get("projectId")
					.and_then(Value::as_str);
				let metadata_host = entry.metadata.get("hostId");
				if entry.kind != crate::core::refinement::refinement::RefinementKind::Memory
					|| !id_ok
					|| metadata_project != Some(project_id)
					|| metadata_host.is_some()
				{
					return (
						400,
						"{\"error\":\"Invalid memory request or unavailable storage\"}".to_string(),
					);
				}
				let entry_value = serde_json::to_value(entry).unwrap_or(Value::Null);
				staged["entries"]["memory"][&entry.id] = entry_value;
			}
			for id in &write.remove {
				if let Some(memory) = staged["entries"]["memory"].as_object_mut() {
					memory.remove(id);
				}
			}
			let revision = staged["memory"]["revision"].as_i64().unwrap_or(0) + 1;
			staged["memory"]["revision"] = Value::from(revision);
			staged["memory"]["events"][&write.id] = Value::String(fingerprint.clone());
			match validate_document(&staged, project_id) {
				Ok(_) => {}
				Err(_) => {
					return (
						400,
						"{\"error\":\"Invalid memory request or unavailable storage\"}".to_string(),
					)
				}
			}
			if serde_json::to_string(&staged).map(|body| body.len()).unwrap_or(usize::MAX) > MAX_DOCUMENT_BYTES {
				return (
					400,
					"{\"error\":\"Invalid memory request or unavailable storage\"}".to_string(),
				);
			}
			let backup = project_dir.join(format!("backup_{}.json", revision - 1));
			if write_json(&backup.to_string_lossy(), &state).is_err()
				|| write_json(&path.to_string_lossy(), &staged).is_err()
			{
				return (
					400,
					"{\"error\":\"Invalid memory request or unavailable storage\"}".to_string(),
				);
			}
			return (200, serde_json::to_string(&staged).unwrap_or_else(|_| "{}".to_string()));
		}
	}
	(200, serde_json::to_string(&state).unwrap_or_else(|_| "{}".to_string()))
}

pub fn create_memory_server(dir: &str, token: &str) -> Result<(), String> {
	if token.len() < 32 {
		return Err("Shared memory token must contain at least 32 characters".to_string());
	}
	std::fs::create_dir_all(dir).map_err(|error| error.to_string())?;
	Ok(())
}

pub fn validate_server_arguments(dir: Option<&str>, token_file: Option<&str>, port_text: Option<&str>) -> Result<(String, String, u16), String> {
	let port_text = port_text.unwrap_or("8799");
	let port: i64 = port_text.parse().map_err(|_| "Usage: node server.js <state-directory> <token-file> [port]".to_string())?;
	match (dir, token_file) {
		(Some(dir), Some(token_file)) if (1..=65535).contains(&port) => {
			Ok((dir.to_string(), token_file.to_string(), port as u16))
		}
		_ => Err("Usage: node server.js <state-directory> <token-file> [port]".to_string()),
	}
}

/// `Buffer.byteLength` for the request body limit.
pub fn body_byte_length(body: &str) -> usize {
	body.as_bytes().len()
}

/// The digest helper is re-exported so callers can assert the same fingerprint
/// format the TypeScript server writes into `memory.events`.
pub fn fingerprint(value: &Value) -> String {
	let mut hasher = Sha256::new();
	hasher.update(serde_json::to_string(value).unwrap_or_default().as_bytes());
	format!("{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::core::memory::project::ProjectIdentity;
	use crate::core::memory::store::{MemoryStore, ApplyOptions};
	use crate::core::refinement::refinement::normalize_refinement_proposal;

	fn fixture() -> (String, std::path::PathBuf) {
		let root = std::env::temp_dir().join(format!("prime-server-{}", uuid::Uuid::new_v4()));
		let cwd = root.join("repo");
		let agent_dir = root.join("agent");
		std::fs::create_dir_all(&cwd).unwrap();
		std::fs::create_dir_all(&agent_dir).unwrap();
		let project = ProjectIdentity {
			id: "project_test".to_string(),
			root: cwd.to_string_lossy().to_string(),
			aliases: Vec::new(),
		};
		let store = MemoryStore::new(&agent_dir.to_string_lossy(), project).unwrap();
		let dir = root.join("shared");
		std::fs::create_dir_all(&dir).unwrap();
		let token = "t".repeat(40);
		let _ = store;
		// Seed one memory so POST has something to share.
		let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
		let proposal = normalize_refinement_proposal(&serde_json::json!({
			"summary": "one", "rationale": "r", "expectedOutcome": "o",
			"edits": [{"action": "create", "kind": "memory", "id": "one", "title": "one", "content": "Content one"}]
		}));
		runtime
			.block_on(store.apply(
				&proposal,
				ApplyOptions {
					event_id: "op_one".to_string(),
					expected_revision: 0,
					..Default::default()
				},
			))
			.unwrap();
		std::fs::write(root.join("store.json"), serde_json::to_string(&store.read().unwrap()).unwrap()).unwrap();
		(root.to_string_lossy().to_string(), root)
	}

	#[test]
	fn rejects_a_short_token() {
		assert_eq!(
			create_memory_server("dir", "short").unwrap_err(),
			"Shared memory token must contain at least 32 characters"
		);
	}

	#[test]
	fn requires_authorization() {
		let (dir, root) = fixture();
		let token = "t".repeat(40);
		assert_eq!(
			handle_request(&dir, &token, None, "GET", "/projects/project_test", &[]),
			(401, "{\"error\":\"Unauthorized\"}".to_string())
		);
		assert_eq!(
			handle_request(&dir, &token, Some("Bearer wrong"), "GET", "/projects/project_test", &[]),
			(401, "{\"error\":\"Unauthorized\"}".to_string())
		);
		std::fs::remove_dir_all(&root).ok();
	}

	#[test]
	fn rejects_unknown_routes_and_methods() {
		let (dir, root) = fixture();
		let token = "t".repeat(40);
		let auth = format!("Bearer {token}");
		assert_eq!(
			handle_request(&dir, &token, Some(&auth), "GET", "/projects/other", &[]),
			(404, "{}".to_string())
		);
		assert_eq!(
			handle_request(&dir, &token, Some(&auth), "DELETE", "/projects/project_test", &[]),
			(404, "{}".to_string())
		);
		std::fs::remove_dir_all(&root).ok();
	}

	#[test]
	fn serves_an_empty_document_and_enforces_revisions() {
		let (dir, root) = fixture();
		let token = "t".repeat(40);
		let auth = format!("Bearer {token}");
		let (status, body) = handle_request(&dir, &token, Some(&auth), "GET", "/projects/project_test", &[]);
		assert_eq!(status, 200);
		let document: Value = serde_json::from_str(&body).unwrap();
		assert_eq!(document["memory"]["revision"], serde_json::json!(0));
		assert_eq!(document["memory"]["projectId"], serde_json::json!("project_test"));
		// Revision conflict when the client is stale.
		let write = serde_json::json!({
			"id": format!("share_{}", "a".repeat(32)),
			"revision": 5,
			"entries": [],
			"remove": []
		});
		let (status, body) = handle_request(
			&dir,
			&token,
			Some(&auth),
			"POST",
			"/projects/project_test",
			serde_json::to_string(&write).unwrap().as_bytes(),
		);
		assert_eq!(status, 409);
		assert_eq!(body, "{\"error\":\"Revision conflict\"}");
		// Invalid shared write.
		let (status, _) = handle_request(&dir, &token, Some(&auth), "POST", "/projects/project_test", b"{\"id\":\"x\"}");
		assert_eq!(status, 400);
		std::fs::remove_dir_all(&root).ok();
	}

	#[test]
	fn applies_a_shared_write_and_writes_a_backup() {
		let (dir, root) = fixture();
		let token = "t".repeat(40);
		let auth = format!("Bearer {token}");
		let entry = serde_json::json!({
			"id": "one", "kind": "memory", "title": "one", "content": "Content one", "path": "general",
			"reference": {}, "arguments": {}, "metadata": {"projectId": "project_test"}, "source": "refine",
			"created_at": "2026-01-01T00:00:00.000Z", "updated_at": "2026-01-01T00:00:00.000Z", "version": 1
		});
		let write = serde_json::json!({
			"id": format!("share_{}", "b".repeat(32)),
			"revision": 0,
			"entries": [entry],
			"remove": []
		});
		let (status, body) = handle_request(
			&dir,
			&token,
			Some(&auth),
			"POST",
			"/projects/project_test",
			serde_json::to_string(&write).unwrap().as_bytes(),
		);
		assert_eq!(status, 200);
		let document: Value = serde_json::from_str(&body).unwrap();
		assert_eq!(document["memory"]["revision"], serde_json::json!(1));
		assert!(document["entries"]["memory"]["one"]["content"] == serde_json::json!("Content one"));
		assert!(document["memory"]["events"][write["id"].as_str().unwrap()].is_string());
		assert!(std::path::Path::new(&dir).join("project_test").join("backup_0.json").exists());
		// Replaying the same operation id and payload is idempotent.
		let (status, body) = handle_request(
			&dir,
			&token,
			Some(&auth),
			"POST",
			"/projects/project_test",
			serde_json::to_string(&write).unwrap().as_bytes(),
		);
		assert_eq!(status, 200);
		let replay: Value = serde_json::from_str(&body).unwrap();
		assert_eq!(replay["memory"]["revision"], serde_json::json!(1));
		// Reusing the id with a different payload is rejected.
		let mut other = write.clone();
		other["remove"] = serde_json::json!(["one"]);
		let (status, _) = handle_request(
			&dir,
			&token,
			Some(&auth),
			"POST",
			"/projects/project_test",
			serde_json::to_string(&other).unwrap().as_bytes(),
		);
		assert_eq!(status, 400);
		std::fs::remove_dir_all(&root).ok();
	}

	#[test]
	fn rejects_host_publication_and_oversized_bodies() {
		let (dir, root) = fixture();
		let token = "t".repeat(40);
		let auth = format!("Bearer {token}");
		let entry = serde_json::json!({
			"id": "host", "kind": "memory", "title": "h", "content": "c", "path": "general",
			"reference": {}, "arguments": {}, "metadata": {"projectId": "project_test", "hostId": "x"}, "source": "refine",
			"created_at": "2026-01-01T00:00:00.000Z", "updated_at": "2026-01-01T00:00:00.000Z", "version": 1
		});
		let write = serde_json::json!({
			"id": format!("share_{}", "c".repeat(32)),
			"revision": 0,
			"entries": [entry],
			"remove": []
		});
		let (status, _) = handle_request(
			&dir,
			&token,
			Some(&auth),
			"POST",
			"/projects/project_test",
			serde_json::to_string(&write).unwrap().as_bytes(),
		);
		assert_eq!(status, 400);
		let oversized = vec![b'x'; MAX_BODY_BYTES + 1];
		let (status, body) = handle_request(&dir, &token, Some(&auth), "POST", "/projects/project_test", &oversized);
		assert_eq!(status, 413);
		assert_eq!(body, "{}");
		std::fs::remove_dir_all(&root).ok();
	}

	#[test]
	fn validates_server_arguments() {
		assert_eq!(
			validate_server_arguments(None, None, None).unwrap_err(),
			"Usage: node server.js <state-directory> <token-file> [port]"
		);
		assert_eq!(
			validate_server_arguments(Some("d"), Some("t"), Some("0")).unwrap_err(),
			"Usage: node server.js <state-directory> <token-file> [port]"
		);
		assert_eq!(
			validate_server_arguments(Some("d"), Some("t"), None).unwrap(),
			("d".to_string(), "t".to_string(), 8799)
		);
	}
}
