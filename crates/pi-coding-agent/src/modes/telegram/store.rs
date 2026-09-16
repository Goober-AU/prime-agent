//! Port of packages/coding-agent/src/modes/telegram/store.ts

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::utils::atomic_file::{write_file_atomic_sync, WriteFileAtomicOptions};

/// `TelegramConnectionSettings`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TelegramConnectionSettings {
    pub version: f64,
    pub enabled: bool,
    pub bot_token: String,
    pub bot_id: f64,
    pub bot_username: String,
    pub daemon_socket: String,
    pub cwd: String,
    pub session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub paired_user_id: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pairing: Option<TelegramPairing>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TelegramPairing {
    pub hash: String,
    pub expires_at: f64,
}

/// `TelegramDelivery`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TelegramDelivery {
    pub id: String,
    pub chat_id: f64,
    pub text: String,
}

/// `TelegramState`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TelegramState {
    pub bot_id: f64,
    pub offset: f64,
    pub outbox: Vec<TelegramDelivery>,
    pub inbox: Vec<TelegramInboxItem>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_assistant_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interrupted_update: Option<f64>,
}

impl TelegramState {
    /// `{ botId, offset: 0, outbox: [], inbox: [] }`.
    pub fn empty(bot_id: f64) -> Self {
        Self {
            bot_id,
            offset: 0.0,
            outbox: Vec::new(),
            inbox: Vec::new(),
            last_assistant_key: None,
            interrupted_update: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TelegramInboxItem {
    pub id: f64,
    pub text: String,
}

/// `TelegramWorkerStatus`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TelegramWorkerStatus {
    pub instance_id: String,
    pub pid: f64,
    pub updated_at: f64,
    pub phase: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

pub const TELEGRAM_WORKER_PHASES: [&str; 5] = ["starting", "pairing", "running", "stopped", "error"];

/// `isRecord(value)`.
pub fn is_record(value: &serde_json::Value) -> bool {
    value.is_object()
}

/// `isTelegramId(value)`.
pub fn is_telegram_id(value: &serde_json::Value) -> bool {
    value
        .as_f64()
        .map(|number| number.fract() == 0.0 && number > 0.0 && number.abs() < 9_007_199_254_740_992.0)
        .unwrap_or(false)
}

/// `isTelegramUpdateId(value)`.
pub fn is_telegram_update_id(value: &serde_json::Value) -> bool {
    value
        .as_f64()
        .map(|number| {
            number.fract() == 0.0
                && number >= 0.0
                && number < 9_007_199_254_740_991.0
        })
        .unwrap_or(false)
}

/// `validBotToken(token)`.
pub fn valid_bot_token(token: &str) -> bool {
    static TOKEN: once_cell::sync::Lazy<regex::Regex> = once_cell::sync::Lazy::new(|| {
        regex::Regex::new(r"^\d{5,20}:[A-Za-z0-9_-]{20,200}$").expect("static regex")
    });
    TOKEN.is_match(token)
}

/// `pairingHash(code)`.
pub fn pairing_hash(code: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(code.as_bytes());
    let digest = hasher.finalize();
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// `createPairing(now)`.
pub fn create_pairing(now: i64) -> (String, TelegramPairing) {
    let code = base64_url(&random_bytes(24));
    let pairing = TelegramPairing {
        hash: pairing_hash(&code),
        expires_at: (now + 10 * 60_000) as f64,
    };
    (code, pairing)
}

/// `randomBytes(24).toString("base64url")`.
fn random_bytes(length: usize) -> Vec<u8> {
    use rand::RngCore;
    let mut bytes = vec![0u8; length];
    rand::thread_rng().fill_bytes(&mut bytes);
    bytes
}

fn base64_url(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// `class TelegramStore`.
pub struct TelegramStore {
    pub agent_dir: String,
    pub directory: String,
}

impl TelegramStore {
    pub fn new(agent_dir: &str) -> Self {
        Self {
            agent_dir: agent_dir.to_string(),
            directory: Path::new(agent_dir).join("telegram").to_string_lossy().to_string(),
        }
    }

    /// `path(name)`.
    pub fn path(&self, name: &str) -> String {
        PathBuf::from(&self.directory)
            .join(name)
            .to_string_lossy()
            .to_string()
    }

    /// `read(name)`.
    pub fn read(&self, name: &str) -> Result<Option<serde_json::Value>, String> {
        let path = self.path(name);
        match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text).map(Some).map_err(|_| {
                format!("Cannot read Telegram {name}. Restore or remove that file before reconnecting.")
            }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(_) => Err(format!(
                "Cannot read Telegram {name}. Restore or remove that file before reconnecting."
            )),
        }
    }

    /// `write(name, value)`.
    pub fn write(&self, name: &str, value: &serde_json::Value) -> Result<(), String> {
        std::fs::create_dir_all(&self.directory).map_err(|error| error.to_string())?;
        set_directory_mode(&self.directory);
        let text = format!(
            "{}\n",
            serde_json::to_string_pretty(value).map_err(|error| error.to_string())?
        );
        write_file_atomic_sync(
            &self.path(name),
            &text,
            WriteFileAtomicOptions {
                mode: Some(0o600),
                fsync: true,
                ..Default::default()
            },
        )
        .map_err(|error| error.to_string())
    }

    /// `settings()`.
    pub fn settings(&self) -> Result<Option<TelegramConnectionSettings>, String> {
        let Some(value) = self.read("connection.json")? else {
            return Ok(None);
        };
        let settings: TelegramConnectionSettings = serde_json::from_value(value.clone())
            .map_err(|_| invalid_settings())?;
        let pairing_ok = match &settings.pairing {
            None => true,
            Some(pairing) => {
                static HASH: once_cell::sync::Lazy<regex::Regex> =
                    once_cell::sync::Lazy::new(|| regex::Regex::new(r"^[a-f0-9]{64}$").expect("static regex"));
                HASH.is_match(&pairing.hash)
                    && pairing.expires_at.fract() == 0.0
                    && pairing.expires_at.abs() < 9_007_199_254_740_992.0
            }
        };
        let valid = settings.version == 1.0
            && valid_bot_token(&settings.bot_token)
            && is_telegram_id(&serde_json::json!(settings.bot_id))
            && valid_bot_username(&settings.bot_username)
            && !settings.daemon_socket.is_empty()
            && !settings.session_id.is_empty()
            && settings
                .session_file
                .as_ref()
                .map(|_| true)
                .unwrap_or(true)
            && settings
                .paired_user_id
                .map(|id| is_telegram_id(&serde_json::json!(id)))
                .unwrap_or(true)
            && pairing_ok;
        if !valid {
            return Err(invalid_settings());
        }
        Ok(Some(settings))
    }

    /// `state(botId)`.
    pub fn state(&self, bot_id: f64) -> Result<TelegramState, String> {
        let Some(value) = self.read("state.json")? else {
            return Ok(TelegramState::empty(bot_id));
        };
        if is_record(&value) && value.get("botId").and_then(serde_json::Value::as_f64) != Some(bot_id) {
            return Ok(TelegramState::empty(bot_id));
        }
        let state: TelegramState =
            serde_json::from_value(value.clone()).map_err(|_| invalid_state())?;
        let offset_ok = value
            .get("offset")
            .map(|offset| {
                offset
                    .as_f64()
                    .map(|number| number.fract() == 0.0 && number >= 0.0 && number.abs() < 9_007_199_254_740_992.0)
                    .unwrap_or(false)
            })
            .unwrap_or(false);
        let outbox_ok = value
            .get("outbox")
            .and_then(serde_json::Value::as_array)
            .map(|outbox| {
                outbox.len() <= 1000
                    && outbox.iter().all(|item| {
                        is_record(item)
                            && item.get("id").map(|id| id.is_string()).unwrap_or(false)
                            && is_telegram_id(item.get("chatId").unwrap_or(&serde_json::Value::Null))
                            && item
                                .get("text")
                                .and_then(serde_json::Value::as_str)
                                .map(|text| crate::modes::telegram::api::utf16_len(text) <= 4000)
                                .unwrap_or(false)
                    })
            })
            .unwrap_or(false);
        let inbox_ok = value
            .get("inbox")
            .and_then(serde_json::Value::as_array)
            .map(|inbox| {
                inbox.len() <= 100
                    && inbox.iter().all(|item| {
                        is_record(item)
                            && is_telegram_update_id(item.get("id").unwrap_or(&serde_json::Value::Null))
                            && item
                                .get("text")
                                .and_then(serde_json::Value::as_str)
                                .map(|text| crate::modes::telegram::api::utf16_len(text) <= 16384)
                                .unwrap_or(false)
                    })
            })
            .unwrap_or(false);
        let interrupted_ok = match value.get("interruptedUpdate") {
            None => true,
            Some(update) => is_telegram_update_id(update),
        };
        if !offset_ok || !outbox_ok || !inbox_ok || !interrupted_ok {
            return Err(invalid_state());
        }
        Ok(state)
    }

    /// `status()`.
    pub fn status(&self) -> Result<Option<TelegramWorkerStatus>, String> {
        let Some(value) = self.read("worker.json")? else {
            return Ok(None);
        };
        if !is_record(&value) {
            return Ok(None);
        }
        let Ok(status) = serde_json::from_value::<TelegramWorkerStatus>(value.clone()) else {
            return Ok(None);
        };
        if !is_telegram_id(&serde_json::json!(status.pid))
            || !TELEGRAM_WORKER_PHASES.contains(&status.phase.as_str())
        {
            return Ok(None);
        }
        Ok(Some(status))
    }
}

fn invalid_settings() -> String {
    "Invalid Telegram connection settings. Run /telegram disconnect, then /telegram setup.".to_string()
}

fn invalid_state() -> String {
    "Invalid Telegram delivery state. Disconnect Telegram before repairing state.json.".to_string()
}

fn valid_bot_username(username: &str) -> bool {
    static USERNAME: once_cell::sync::Lazy<regex::Regex> =
        once_cell::sync::Lazy::new(|| regex::Regex::new(r"^[A-Za-z0-9_]{1,64}$").expect("static regex"));
    USERNAME.is_match(username)
}

#[cfg(unix)]
fn set_directory_mode(path: &str) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700));
}

#[cfg(not(unix))]
fn set_directory_mode(_path: &str) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bot_tokens_are_validated_by_shape() {
        assert!(valid_bot_token("123456789:AAAAAAAAAAAAAAAAAAAA"));
        assert!(!valid_bot_token("nope"));
        assert!(!valid_bot_token("12345:short"));
    }

    #[test]
    fn telegram_ids_are_positive_safe_integers() {
        assert!(is_telegram_id(&serde_json::json!(1)));
        assert!(!is_telegram_id(&serde_json::json!(0)));
        assert!(!is_telegram_id(&serde_json::json!(1.5)));
        assert!(!is_telegram_id(&serde_json::json!("1")));
        assert!(is_telegram_update_id(&serde_json::json!(0)));
        assert!(!is_telegram_update_id(&serde_json::json!(-1)));
    }

    #[test]
    fn pairing_hashes_are_hex_sha256() {
        let (code, pairing) = create_pairing(1000);
        assert_eq!(pairing.hash.len(), 64);
        assert_eq!(pairing.hash, pairing_hash(&code));
        assert_eq!(pairing.expires_at, 601_000.0);
        assert!(!code.contains('='));
    }

    #[test]
    fn a_missing_store_directory_reads_as_absent() {
        let dir = tempfile::tempdir().unwrap();
        let store = TelegramStore::new(dir.path().to_str().unwrap());
        assert!(store.read("connection.json").unwrap().is_none());
        assert!(store.settings().unwrap().is_none());
        assert!(store.status().unwrap().is_none());
        let state = store.state(7.0).unwrap();
        assert_eq!(state.bot_id, 7.0);
        assert_eq!(state.offset, 0.0);
    }

    #[test]
    fn settings_round_trip_through_the_store() {
        let dir = tempfile::tempdir().unwrap();
        let store = TelegramStore::new(dir.path().to_str().unwrap());
        let settings = TelegramConnectionSettings {
            version: 1.0,
            enabled: true,
            bot_token: "123456789:AAAAAAAAAAAAAAAAAAAA".to_string(),
            bot_id: 5.0,
            bot_username: "prime_bot".to_string(),
            daemon_socket: "/tmp/socket".to_string(),
            cwd: "/work".to_string(),
            session_id: "s1".to_string(),
            session_file: None,
            paired_user_id: None,
            pairing: Some(TelegramPairing {
                hash: pairing_hash("code"),
                expires_at: 1_000.0,
            }),
        };
        store
            .write("connection.json", &serde_json::to_value(&settings).unwrap())
            .unwrap();
        assert_eq!(store.settings().unwrap(), Some(settings));
    }

    #[test]
    fn invalid_settings_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let store = TelegramStore::new(dir.path().to_str().unwrap());
        store
            .write(
                "connection.json",
                &serde_json::json!({"version": 2, "enabled": true, "botToken": "bad"}),
            )
            .unwrap();
        assert_eq!(
            store.settings().unwrap_err(),
            "Invalid Telegram connection settings. Run /telegram disconnect, then /telegram setup."
        );
    }

    #[test]
    fn state_for_another_bot_starts_fresh() {
        let dir = tempfile::tempdir().unwrap();
        let store = TelegramStore::new(dir.path().to_str().unwrap());
        store
            .write(
                "state.json",
                &serde_json::json!({"botId": 1, "offset": 9, "outbox": [], "inbox": []}),
            )
            .unwrap();
        let state = store.state(2.0).unwrap();
        assert_eq!(state.offset, 0.0);
        let state = store.state(1.0).unwrap();
        assert_eq!(state.offset, 9.0);
    }

    #[test]
    fn invalid_state_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let store = TelegramStore::new(dir.path().to_str().unwrap());
        store
            .write(
                "state.json",
                &serde_json::json!({"botId": 1, "offset": -1, "outbox": [], "inbox": []}),
            )
            .unwrap();
        assert_eq!(
            store.state(1.0).unwrap_err(),
            "Invalid Telegram delivery state. Disconnect Telegram before repairing state.json."
        );
    }

    #[test]
    fn an_invalid_worker_status_reads_as_absent() {
        let dir = tempfile::tempdir().unwrap();
        let store = TelegramStore::new(dir.path().to_str().unwrap());
        store
            .write(
                "worker.json",
                &serde_json::json!({"instanceId": "i", "pid": 1, "updatedAt": 0, "phase": "bogus"}),
            )
            .unwrap();
        assert!(store.status().unwrap().is_none());
    }
}
