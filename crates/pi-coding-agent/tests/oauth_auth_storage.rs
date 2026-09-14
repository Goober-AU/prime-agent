//! OAuth must work through the application storage, not only pi-ai's registry.
//! One test keeps process-global provider overrides isolated from library tests.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use pi_ai::utils::oauth::{
    get_oauth_provider, register_oauth_provider, reset_oauth_providers, unregister_oauth_provider,
    OAuthCredentials,
};
use pi_coding_agent::core::auth_storage::{AuthCredential, AuthStorage};
use pi_coding_agent::core::model_registry::ModelRegistry;
use serde_json::{json, Value};

fn credentials(access: &str, expires: f64) -> OAuthCredentials {
    OAuthCredentials {
        access: access.into(),
        refresh: "fixture-refresh".into(),
        expires,
        extra: json!({"accountId": "fixture-account"})
            .as_object()
            .unwrap()
            .clone(),
    }
}

fn storage(access: &str, expires: f64) -> AuthStorage {
    AuthStorage::in_memory(
        [(
            "openai-codex".into(),
            AuthCredential::OAuth {
                credentials: credentials(access, expires),
            },
        )]
        .into_iter()
        .collect(),
        None,
    )
}

#[tokio::test]
async fn application_oauth_lookup_login_refresh_and_shared_file_locking() {
    reset_oauth_providers();
    let mut auth = storage("fixture-access", f64::MAX);
    assert!(auth
        .get_oauth_providers()
        .iter()
        .any(|p| p.id == "openai-codex"));
    let resolved = auth
        .get_api_key_with_source_token("openai-codex", false)
        .await
        .unwrap();
    assert_eq!(resolved.api_key.as_deref(), Some("fixture-access"));
    assert_eq!(resolved.source_token.unwrap().source, "stored");

    // Refreshing the model catalog must retain built-in OAuth and restore MCP.
    let mut registry = ModelRegistry::in_memory(AuthStorage::in_memory(Default::default(), None));
    registry.refresh();
    assert!(get_oauth_provider("openai-codex").is_some());
    assert!(get_oauth_provider("mcp:linear").is_some());
    assert_eq!(
        auth.get_api_key("openai-codex", false)
            .await
            .unwrap()
            .as_deref(),
        Some("fixture-access")
    );

    // Register in pi-ai, exactly where the login UI and MCP integrations register.
    let refreshes = Arc::new(AtomicUsize::new(0));
    let count = refreshes.clone();
    let mut provider = get_oauth_provider("openai-codex").unwrap();
    provider.login = Arc::new(|_| Box::pin(async { Ok(credentials("fixture-login", f64::MAX)) }));
    provider.refresh_token = Arc::new(move |previous| {
        let count = count.clone();
        Box::pin(async move {
            assert_eq!(previous.refresh, "fixture-refresh");
            count.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(50)).await;
            let mut next = previous;
            next.access = "fixture-refreshed".into();
            next.refresh = "fixture-rotated".into();
            next.expires = f64::MAX;
            Ok(next)
        })
    });
    register_oauth_provider(provider);
    auth.login("openai-codex", Default::default())
        .await
        .unwrap();
    assert_eq!(
        auth.get_api_key("openai-codex", false)
            .await
            .unwrap()
            .as_deref(),
        Some("fixture-login")
    );

    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("auth.json");
    let original = json!({
        "openai-codex": {"type":"oauth","access":"fixture-expired","refresh":"fixture-refresh","expires":0,"accountId":"fixture-account"},
        "other": {"type":"api_key","key":"other-fixture"},
        "future-format": {"type":"future","opaque":"preserve-me"}
    });
    std::fs::write(&path, original.to_string()).unwrap();
    let alias = root.path().join("shared-auth.json");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&path, &alias).unwrap();
    #[cfg(not(unix))]
    let alias = path.clone();

    let mut first = AuthStorage::create(Some(path.to_string_lossy().into_owned()), None);
    let mut second = AuthStorage::create(Some(alias.to_string_lossy().into_owned()), None);
    // Independent callers must refresh a rotating token once, under the same lock.
    let (a, b) = tokio::join!(
        first.get_api_key("openai-codex", false),
        second.get_api_key("openai-codex", false),
    );
    assert_eq!(a.unwrap().as_deref(), Some("fixture-refreshed"));
    assert_eq!(b.unwrap().as_deref(), Some("fixture-refreshed"));
    assert_eq!(refreshes.load(Ordering::SeqCst), 1);
    let saved: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(saved["openai-codex"]["refresh"], "fixture-rotated");
    assert_eq!(saved["openai-codex"]["accountId"], "fixture-account");
    assert_eq!(saved["other"], original["other"]);
    assert_eq!(saved["future-format"], original["future-format"]);
    #[cfg(unix)]
    assert!(alias.is_symlink());
    assert!(!path.with_extension("json.lock").exists());

    // A real refresh failure must be recorded, without deleting stored credentials.
    let mut provider = get_oauth_provider("openai-codex").unwrap();
    provider.refresh_token = Arc::new(|_| Box::pin(async { Err("fixture failure".into()) }));
    register_oauth_provider(provider);
    let mut failed = storage("fixture-expired", 0.0);
    assert!(failed
        .get_api_key("openai-codex", false)
        .await
        .unwrap()
        .is_none());
    assert_eq!(
        failed.drain_errors(),
        ["Failed to refresh OAuth token for openai-codex"]
    );
    assert!(failed.has("openai-codex"));

    // The fallback reload recovers a successful peer refresh after an error.
    let peer_path = root.path().join("peer-auth.json");
    std::fs::write(
        &peer_path,
        json!({"openai-codex": original["openai-codex"]}).to_string(),
    )
    .unwrap();
    let destination = peer_path.clone();
    let mut provider = get_oauth_provider("openai-codex").unwrap();
    provider.refresh_token = Arc::new(move |_| {
        let destination = destination.clone();
        Box::pin(async move {
            std::fs::write(destination, json!({"openai-codex": {
                "type":"oauth","access":"fixture-peer","refresh":"fixture-rotated","expires":4102444800000u64
            }}).to_string()).unwrap();
            Err("fixture lost refresh race".into())
        })
    });
    register_oauth_provider(provider);
    let mut peer = AuthStorage::create(Some(peer_path.to_string_lossy().into_owned()), None);
    assert_eq!(
        peer.get_api_key("openai-codex", false)
            .await
            .unwrap()
            .as_deref(),
        Some("fixture-peer")
    );
    assert_eq!(peer.drain_errors().len(), 1);
    unregister_oauth_provider("openai-codex");
    assert_eq!(
        get_oauth_provider("openai-codex").unwrap().name,
        "ChatGPT Plus/Pro (Codex Subscription)"
    );
}
