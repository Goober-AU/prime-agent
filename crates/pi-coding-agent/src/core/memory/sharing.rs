//! Port of packages/coding-agent/src/core/memory/sharing.ts
use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::core::refinement::refinement::HarnessEntry;

use super::evidence::hash;
use super::store::{
    empty_document, read_json, record, validate_document, write_json, MemoryDocument, MemoryStore,
};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SharedWrite {
    pub id: String,
    pub revision: i64,
    pub entries: Vec<HarnessEntry>,
    pub remove: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SharedCache {
    pub schema: f64,
    pub url: String,
    pub revision: i64,
    pub state: MemoryDocument,
    pub pending: Vec<SharedWrite>,
    pub connected: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Clone)]
pub struct MemorySharing {
    pub path: String,
    pub store: MemoryStore,
}

impl MemorySharing {
    pub fn new(store: MemoryStore) -> MemorySharing {
        let path = Path::new(&store.dir)
            .join("shared.json")
            .to_string_lossy()
            .to_string();
        MemorySharing { path, store }
    }

    pub fn cache(&self) -> SharedCache {
        let url = self
            .store
            .settings()
            .shared
            .and_then(|value| value)
            .map(|config| config.url)
            .unwrap_or_default();
        let cached: Option<SharedCache> = if Path::new(&self.path).exists() {
            read_json(&self.path)
                .ok()
                .and_then(|value| serde_json::from_value(value).ok())
        } else {
            None
        };
        // Changing or detaching an endpoint immediately invalidates its cached assets.
        if let Some(cached) = &cached {
            if cached.url == url && !url.is_empty() {
                let mut next = cached.clone();
                next.state = validate_document(
                    &serde_json::to_value(&cached.state).unwrap_or(Value::Null),
                    &self.store.project.id,
                )
                .unwrap_or_else(|_| empty_document(&self.store.project.id));
                return next;
            }
        }
        SharedCache {
            schema: 1.0,
            url,
            revision: 0,
            state: empty_document(&self.store.project.id),
            pending: Vec::new(),
            connected: false,
            error: None,
        }
    }

    pub async fn queue(&self, ids: &[String], remove: Vec<String>) -> Result<SharedWrite, String> {
        let ids = ids.to_vec();
        let store = self.store.clone();
        self.store
            .exclusive(move || {
                if store.settings().shared_config().is_none() {
                    return Err("Configure sharing for this project first".to_string());
                }
                let doc = store.read()?;
                let mut entries = Vec::new();
                for id in &ids {
                    let entry = doc
                        .entries
                        .get("memory")
                        .and_then(|bucket| bucket.get(id))
                        .cloned()
                        .ok_or_else(|| format!("Only project memories can be shared: {id}"))?;
                    if entry.metadata.get("hostId").and_then(Value::as_str).is_some_and(|host| !host.is_empty()) {
                        return Err(format!("Only project memories can be shared: {id}"));
                    }
                    entries.push(entry);
                }
                let sharing = MemorySharing::new(store.clone());
                let mut cached = sharing.cache();
                if !cached.pending.is_empty() {
                    return Err("Sync or resolve the pending shared write first".to_string());
                }
                let revision = cached.revision;
                let digest = hash(
                    &serde_json::to_string(&serde_json::json!({
                        "revision": revision,
                        "entries": entries,
                        "remove": remove,
                    }))
                    .unwrap_or_default(),
                );
                let write = SharedWrite {
                    id: format!("share_{}", &digest[..32.min(digest.len())]),
                    revision,
                    entries,
                    remove,
                };
                cached.pending.push(write.clone());
                store.backup(Some(doc))?;
                write_json(
                    &sharing.path,
                    &serde_json::to_value(&cached).unwrap_or(Value::Null),
                )?;
                Ok(write)
            })
            .await
    }

    pub async fn sync(&self) -> Result<SharedCache, String> {
        let store = self.store.clone();
        let path = self.path.clone();
        let lock_store = store.clone();
        lock_store
            .exclusive_async(move || {
                let store = store.clone();
                let path = path.clone();
                async move {
                    let sharing = MemorySharing {
                        path: path.clone(),
                        store: store.clone(),
                    };
                    let config = match store.settings().shared.and_then(|value| value) {
                        Some(config) => config,
                        None => return Err("Sharing is not configured".to_string()),
                    };
                    let mut cached = sharing.cache();
                    let token = std::fs::read_to_string(&config.token_file)
                        .map_err(|error| error.to_string())?
                        .trim()
                        .to_string();
                    if token.len() < 32 {
                        return Err(
                            "Shared memory token must contain at least 32 characters".to_string()
                        );
                    }
                    let outcome =
                        request_shared_state(&config.url, &store.project.id, &token, &mut cached)
                            .await;
                    match outcome {
                        Ok(()) => {}
                        Err(error) => {
                            cached.connected = false;
                            cached.error = Some(error);
                        }
                    }
                    write_json(&path, &serde_json::to_value(&cached).unwrap_or(Value::Null))?;
                    Ok(cached)
                }
            })
            .await
    }

    pub async fn discard_pending(&self) -> Result<(), String> {
        let store = self.store.clone();
        let path = self.path.clone();
        self.store
            .exclusive(move || {
                let sharing = MemorySharing {
                    path: path.clone(),
                    store: store.clone(),
                };
                let mut cached = sharing.cache();
                let pending_path = Path::new(&store.dir).join(format!(
                    "shared-pending-{}.json",
                    chrono::Utc::now().timestamp_millis()
                ));
                write_json(
                    &pending_path.to_string_lossy(),
                    &serde_json::to_value(&cached.pending).unwrap_or(Value::Null),
                )?;
                cached.pending = Vec::new();
                write_json(&path, &serde_json::to_value(&cached).unwrap_or(Value::Null))?;
                Ok(())
            })
            .await
    }
}

/// Port of the `request(write?)` closure inside `sync()`. The response body is
/// read incrementally so an oversized payload is rejected before parsing.
async fn request_shared_state(
    url: &str,
    project_id: &str,
    token: &str,
    cached: &mut SharedCache,
) -> Result<(), String> {
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_millis(5000))
        .build()
        .map_err(|error| error.to_string())?;
    let endpoint = format!("{url}/projects/{project_id}");
    let mut request = |write: Option<SharedWrite>| {
        let client = client.clone();
        let endpoint = endpoint.clone();
        let token = token.to_string();
        async move {
            let mut builder = if write.is_some() {
                client.post(&endpoint)
            } else {
                client.get(&endpoint)
            };
            builder = builder
                .header("Authorization", format!("Bearer {token}"))
                .header("Content-Type", "application/json");
            if let Some(write) = &write {
                builder = builder.body(serde_json::to_string(write).unwrap_or_default());
            }
            let response = builder.send().await.map_err(|error| error.to_string())?;
            if !response.status().is_success() {
                return Err(if response.status().as_u16() == 409 {
                    "Shared revision conflict; inspect remote state and requeue explicitly"
                        .to_string()
                } else {
                    format!("Shared memory HTTP {}", response.status().as_u16())
                });
            }
            let mut body: Vec<u8> = Vec::new();
            let mut response = response;
            loop {
                match response.chunk().await {
                    Ok(Some(chunk)) => {
                        body.extend_from_slice(&chunk);
                        if body.len() > 8 * 1024 * 1024 {
                            return Err("Shared memory response too large".to_string());
                        }
                    }
                    Ok(None) => break,
                    Err(error) => return Err(error.to_string()),
                }
            }
            if body.is_empty() {
                return Err("Empty shared memory response".to_string());
            }
            let value: Value = serde_json::from_slice(&body).map_err(|error| error.to_string())?;
            validate_document(&value, project_id)
        }
    };
    for pending in cached.pending.clone() {
        let state = request(Some(pending)).await?;
        cached.revision = state.memory.revision;
        cached.state = state;
    }
    cached.pending = Vec::new();
    let state = request(None).await?;
    cached.revision = state.memory.revision;
    cached.state = state;
    cached.connected = true;
    cached.error = None;
    Ok(())
}

pub fn validate_shared_write(value: &Value) -> Result<SharedWrite, String> {
    let data = record(value)?;
    let id_ok = data
        .get("id")
        .and_then(Value::as_str)
        .map(|id| {
            regex::Regex::new(r"^share_[a-f0-9]{32}$")
                .unwrap()
                .is_match(id)
        })
        .unwrap_or(false);
    let revision_ok = match data.get("revision") {
        Some(Value::Number(number)) => number
            .as_f64()
            .map(|value| value.fract() == 0.0 && value >= 0.0 && value <= 9007199254740991.0)
            .unwrap_or(false),
        _ => false,
    };
    let entries = data.get("entries").and_then(Value::as_array);
    let remove = data.get("remove").and_then(Value::as_array);
    if !id_ok
        || !revision_ok
        || entries.is_none()
        || remove.is_none()
        || entries.map(|value| value.len()).unwrap_or(0) > 100
        || remove.map(|value| value.len()).unwrap_or(0) > 100
    {
        return Err("Invalid shared write".to_string());
    }
    for id in remove.unwrap() {
        match id.as_str() {
            Some(id)
                if regex::Regex::new(r"^[A-Za-z0-9_-]{1,160}$")
                    .unwrap()
                    .is_match(id) => {}
            _ => return Err("Invalid removal ID".to_string()),
        }
    }
    serde_json::from_value(value.clone()).map_err(|_| "Invalid shared write".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::memory::project::ProjectIdentity;
    use crate::core::refinement::refinement::normalize_refinement_proposal;

    fn fixture() -> (MemoryStore, MemorySharing, std::path::PathBuf) {
        let root = std::env::temp_dir().join(format!("prime-sharing-{}", uuid::Uuid::new_v4()));
        let cwd = root.join("repo");
        let agent_dir = root.join("agent");
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::create_dir_all(&agent_dir).unwrap();
        let store = MemoryStore::new(
            &agent_dir.to_string_lossy(),
            ProjectIdentity {
                id: "project_test".to_string(),
                root: cwd.to_string_lossy().to_string(),
                aliases: Vec::new(),
            },
        )
        .unwrap();
        let sharing = MemorySharing::new(store.clone());
        (store, sharing, root)
    }

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    }

    fn create(store: &MemoryStore, id: &str) {
        let runtime = runtime();
        let proposal = normalize_refinement_proposal(&serde_json::json!({
            "summary": id, "rationale": "r", "expectedOutcome": "o",
            "edits": [{"action": "create", "kind": "memory", "id": id, "title": id, "content": format!("Content {id}")}]
        }));
        runtime
            .block_on(store.apply(
                &proposal,
                crate::core::memory::store::ApplyOptions {
                    event_id: format!("op_{id}"),
                    expected_revision: store.read().unwrap().memory.revision,
                    ..Default::default()
                },
            ))
            .unwrap();
    }

    #[test]
    fn requires_configuration_before_queueing() {
        let (store, sharing, root) = fixture();
        create(&store, "one");
        let runtime = runtime();
        let error = runtime
            .block_on(sharing.queue(&["one".to_string()], Vec::new()))
            .expect_err("not configured");
        assert_eq!(error, "Configure sharing for this project first");
        assert_eq!(sharing.cache().url, "");
        assert!(!sharing.cache().connected);
        runtime.block_on(store.configure(&serde_json::json!({"shared": null}))).unwrap();
        assert_eq!(runtime.block_on(sharing.queue(&["one".to_string()], Vec::new())).unwrap_err(),
            "Configure sharing for this project first");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn queues_a_pending_write_and_refuses_a_second_one() {
        let (store, sharing, root) = fixture();
        create(&store, "one");
        runtime().block_on(store.configure(&serde_json::json!({
            "shared": {"url": "http://127.0.0.1:8799", "tokenFile": root.join("token").to_string_lossy()}
        })))
        .unwrap();
        let runtime = runtime();
        let write = runtime
            .block_on(sharing.queue(&["one".to_string()], Vec::new()))
            .expect("queue");
        assert!(write.id.starts_with("share_"));
        assert_eq!(write.id.len(), "share_".len() + 32);
        assert_eq!(write.revision, 0);
        assert_eq!(write.entries.len(), 1);
        assert_eq!(sharing.cache().pending.len(), 1);
        let error = runtime
            .block_on(sharing.queue(&["one".to_string()], Vec::new()))
            .expect_err("pending write");
        assert_eq!(error, "Sync or resolve the pending shared write first");
        runtime.block_on(sharing.discard_pending()).unwrap();
        assert!(sharing.cache().pending.is_empty());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn rejects_host_scoped_and_unknown_memories() {
        let (store, sharing, root) = fixture();
        create(&store, "one");
        runtime()
            .block_on(store.configure(&serde_json::json!({
                "shared": {"url": "http://127.0.0.1:8799", "tokenFile": root.join("token").to_string_lossy()}
            })))
            .unwrap();
        let runtime = runtime();
        let missing = runtime
            .block_on(sharing.queue(&["nope".to_string()], Vec::new()))
            .expect_err("unknown");
        assert_eq!(missing, "Only project memories can be shared: nope");
        let proposal = normalize_refinement_proposal(&serde_json::json!({
            "summary": "host", "rationale": "r", "expectedOutcome": "o",
            "edits": [{"action": "create", "kind": "memory", "id": "host_only", "title": "h", "content": "c"}]
        }));
        runtime
            .block_on(store.apply(
                &proposal,
                crate::core::memory::store::ApplyOptions {
                    event_id: "op_host".to_string(),
                    expected_revision: 1,
                    host: true,
                    ..Default::default()
                },
            ))
            .unwrap();
        let host_error = runtime
            .block_on(sharing.queue(&["host_only".to_string()], Vec::new()))
            .expect_err("host scoped");
        assert_eq!(host_error, "Only project memories can be shared: host_only");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn reports_offline_sync_and_short_tokens() {
        let (store, sharing, root) = fixture();
        create(&store, "one");
        let token_file = root.join("token");
        std::fs::write(&token_file, "short").unwrap();
        runtime()
            .block_on(store.configure(&serde_json::json!({
                "shared": {"url": "http://127.0.0.1:8799", "tokenFile": token_file.to_string_lossy()}
            })))
            .unwrap();
        let runtime = runtime();
        let error = runtime.block_on(sharing.sync()).expect_err("short token");
        assert_eq!(
            error,
            "Shared memory token must contain at least 32 characters"
        );
        std::fs::write(&token_file, "x".repeat(40)).unwrap();
        // Loopback port 8799 has no listener: sync records the failure instead of throwing.
        let cached = runtime.block_on(sharing.sync()).expect("offline sync");
        assert!(!cached.connected);
        assert!(cached.error.is_some());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn validates_shared_writes() {
        let good = serde_json::json!({
            "id": format!("share_{}", "a".repeat(32)),
            "revision": 3,
            "entries": [],
            "remove": ["one"]
        });
        assert_eq!(validate_shared_write(&good).unwrap().revision, 3);
        let bad_id =
            serde_json::json!({"id": "share_zz", "revision": 0, "entries": [], "remove": []});
        assert_eq!(
            validate_shared_write(&bad_id).unwrap_err(),
            "Invalid shared write"
        );
        let negative = serde_json::json!({"id": format!("share_{}", "a".repeat(32)), "revision": -1, "entries": [], "remove": []});
        assert_eq!(
            validate_shared_write(&negative).unwrap_err(),
            "Invalid shared write"
        );
        let too_many = serde_json::json!({
            "id": format!("share_{}", "a".repeat(32)),
            "revision": 0,
            "entries": (0..101).map(|_| serde_json::json!({})).collect::<Vec<_>>(),
            "remove": []
        });
        assert_eq!(
            validate_shared_write(&too_many).unwrap_err(),
            "Invalid shared write"
        );
        let bad_removal = serde_json::json!({
            "id": format!("share_{}", "a".repeat(32)),
            "revision": 0,
            "entries": [],
            "remove": ["bad id!"]
        });
        assert_eq!(
            validate_shared_write(&bad_removal).unwrap_err(),
            "Invalid removal ID"
        );
    }

    #[test]
    fn ignores_a_cached_endpoint_after_the_url_changes() {
        let (store, sharing, root) = fixture();
        create(&store, "one");
        runtime()
            .block_on(store.configure(&serde_json::json!({
                "shared": {"url": "http://127.0.0.1:8799", "tokenFile": root.join("token").to_string_lossy()}
            })))
            .unwrap();
        let mut cache = sharing.cache();
        cache.revision = 7;
        write_json(&sharing.path, &serde_json::to_value(&cache).unwrap()).unwrap();
        assert_eq!(sharing.cache().revision, 7);
        runtime()
            .block_on(store.configure(&serde_json::json!({
                "shared": {"url": "http://127.0.0.1:8800", "tokenFile": root.join("token").to_string_lossy()}
            })))
            .unwrap();
        assert_eq!(sharing.cache().revision, 0);
        assert_eq!(sharing.cache().url, "http://127.0.0.1:8800");
        std::fs::remove_dir_all(&root).ok();
    }
}
