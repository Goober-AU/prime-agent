//! Port of packages/coding-agent/src/core/agent-messages.ts
//!
//! Type mapping notes:
//! - `CustomMessage<T>` is carried by `pi_agent_core::types::CustomAgentMessage::Custom`;
//!   `AgentSessionMessage` is therefore a typed view over that variant plus the
//!   validation helpers that the TypeScript declaration provides.
//! - `Map`/`Set` iteration order is observable in the roster output, so ordered
//!   `Vec` collections are used where the TypeScript sorts explicitly.
//! - `randomUUID()` -> `uuid::Uuid::new_v4()`.
//! - `AbortSignal` -> `tokio_util::sync::CancellationToken`.

use std::collections::HashMap;

use pi_agent_core::types::{AgentMessage, CustomAgentMessage, CustomMessageContent};
use serde_json::{Map, Value};

use crate::core::messages::{ASYNC_BASH_COMPLETION_CUSTOM_TYPE, HEARTBEAT_PROMPT_CUSTOM_TYPE};

/// Port of `agentFamilyRelationship`'s use of `canonicalSessionPath`: the pure
/// path canonicalisation from core/session-lease.ts, kept private here so this
/// module does not depend on another slice's file.
fn canonical_session_path(session_path: &str) -> String {
    let resolved = std::path::Path::new(session_path);
    let resolved = if resolved.is_absolute() {
        resolved.to_path_buf()
    } else {
        match std::env::current_dir() {
            Ok(cwd) => cwd.join(resolved),
            Err(_) => resolved.to_path_buf(),
        }
    };
    if let Ok(real) = std::fs::canonicalize(&resolved) {
        return real.to_string_lossy().to_string();
    }
    match resolved
        .parent()
        .and_then(|parent| std::fs::canonicalize(parent).ok())
    {
        Some(real_parent) => real_parent
            .join(resolved.file_name().unwrap_or_default())
            .to_string_lossy()
            .to_string(),
        None => resolved.to_string_lossy().to_string(),
    }
}

pub const AGENT_MESSAGE_CUSTOM_TYPE: &str = "agent_message";
pub const AGENT_MESSAGE_SKILL_NAME: &str = "agent-message";
pub const AGENT_MESSAGE_IMPORT_NAME: &str = "agent_message";
pub const AGENT_MESSAGE_SOURCE: &str = "agent_message";
pub const AGENT_MESSAGE_RECEIVED_PREVIEW_LABEL: &str = "Agent message received";
pub const DEFAULT_AGENT_MESSAGE_MAX_CHARS: usize = 16_384;
pub const DEFAULT_AGENT_MESSAGE_MAX_PENDING_PER_SESSION: usize = 20;
pub const DEFAULT_AGENT_MESSAGE_RATE_LIMIT_CAPACITY: f64 = 3.0;
pub const DEFAULT_AGENT_MESSAGE_RATE_LIMIT_REFILL_MS: f64 = 1000.0;

/// Legacy daemon wire input accepted and ignored for compatibility.
pub type AgentSessionMessageDeliveryMode = String;
pub type AgentSessionMessageDeliveryStatus = String;
pub type AgentSessionMessageRuntimeKind = String;
pub type AgentFamilyStatus = String;
pub type AgentFamilyRelationship = String;

pub const DELIVERY_MODE_AUTO: &str = "auto";
pub const DELIVERY_MODE_STEER: &str = "steer";
pub const DELIVERY_MODE_FOLLOW_UP: &str = "follow_up";

pub const DELIVERY_STATUS_DELIVERED: &str = "delivered";
pub const DELIVERY_STATUS_QUEUED: &str = "queued";

pub const RUNTIME_KIND_TOP_LEVEL: &str = "top-level";
pub const RUNTIME_KIND_SUBAGENT: &str = "subagent";

pub const FAMILY_STATUS_RUNNING: &str = "running";
pub const FAMILY_STATUS_IDLE: &str = "idle";
pub const FAMILY_STATUS_INACTIVE: &str = "inactive";

pub const FAMILY_RELATIONSHIP_PARENT: &str = "parent";
pub const FAMILY_RELATIONSHIP_SIBLING: &str = "sibling";
pub const FAMILY_RELATIONSHIP_CHILD: &str = "child";

pub const AGENT_FAMILY_REACH_ERROR: &str =
    "Agent reach is limited to parent, siblings, and children";

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSessionMessageEndpoint {
    pub active_session_id: String,
    pub session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime_kind: Option<AgentSessionMessageRuntimeKind>,
}

/// `AgentSessionMessageSender extends Partial<AgentSessionMessageEndpoint>`.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSessionMessageSender {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime_kind: Option<AgentSessionMessageRuntimeKind>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
}

pub type AgentMessageDirection = String;

pub const AGENT_MESSAGE_DIRECTION_RECEIVED: &str = "received";
pub const AGENT_MESSAGE_DIRECTION_SENT: &str = "sent";

fn trimmed(value: Option<&String>) -> Option<&str> {
    let value = value?.trim();
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

/// Format the directional role/name segment shared by received and sent agent-message UI.
pub fn format_agent_message_participant(
    direction: &AgentMessageDirection,
    role: Option<&AgentFamilyRelationship>,
    endpoint: Option<&AgentSessionMessageSender>,
) -> String {
    let empty = AgentSessionMessageSender::default();
    let normalized_endpoint = endpoint.unwrap_or(&empty);
    let name_or_id = trimmed(normalized_endpoint.session_name.as_ref())
        .or_else(|| trimmed(normalized_endpoint.active_session_id.as_ref()))
        .or_else(|| trimmed(normalized_endpoint.client_id.as_ref()))
        .or_else(|| trimmed(normalized_endpoint.session_id.as_ref()))
        .unwrap_or("unknown");
    let participant = match role {
        Some(role) => format!("{role} {name_or_id}"),
        None => name_or_id.to_string(),
    };
    let direction_word = if direction == AGENT_MESSAGE_DIRECTION_RECEIVED {
        "from"
    } else {
        "to"
    };
    format!("{direction_word} {participant}")
}

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSessionMessageAgentSummary {
    pub active_session_id: String,
    pub session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime_kind: Option<AgentSessionMessageRuntimeKind>,
    pub cwd: String,
    pub is_streaming: bool,
    pub unfinished_action_count: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_active_session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rlm_child_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_dir: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_session_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rlm_depth: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<AgentFamilyStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rlm_child_registry_status: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSessionMessageListResult {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current: Option<AgentSessionMessageEndpoint>,
    pub agents: Vec<AgentSessionMessageAgentSummary>,
}

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentFamilyCatalogEntry {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub depth: f64,
    pub status: AgentFamilyStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replied_since_task: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_session_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_path: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentFamilyRosterEntry {
    pub relationship: AgentFamilyRelationship,
    pub name: String,
    pub id: String,
    pub depth: f64,
    pub status: AgentFamilyStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replied_since_task: Option<bool>,
}

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentFamilyRosterCurrent {
    pub name: String,
    pub id: String,
    pub depth: f64,
}

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentFamilyRosterResult {
    pub current: AgentFamilyRosterCurrent,
    pub entries: Vec<AgentFamilyRosterEntry>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSessionNameScope {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_session_path: Option<String>,
    pub depth: f64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSessionNameAvailabilityInput {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_session_path: Option<String>,
    pub depth: f64,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ignore_session_id: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSessionMessagePayload {
    pub id: String,
    pub source: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from: Option<AgentSessionMessageSender>,
    /// Sender relationship from the receiver's point of view.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from_relationship: Option<AgentFamilyRelationship>,
    pub target: AgentSessionMessageEndpoint,
}

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSessionMessageDetails {
    pub id: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from: Option<AgentSessionMessageSender>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from_relationship: Option<AgentFamilyRelationship>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<AgentSessionMessageEndpoint>,
}

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSessionMessageReceipt {
    pub id: String,
    pub source: String,
    pub target: AgentSessionMessageEndpoint,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from: Option<AgentSessionMessageSender>,
    pub message: String,
    // Not named "status": the kernel host bridge envelope reserves that key.
    pub delivery_status: AgentSessionMessageDeliveryStatus,
    /// Present only for delivered messages: when the target context received it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delivered_at: Option<String>,
    /// Present only for queued messages: when it was placed behind current work.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub queued_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delivery_mode: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSessionMessageSendInput {
    pub target: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub receiver_role: Option<AgentFamilyRelationship>,
}

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSessionMessageSafetyStatus {
    pub paused: bool,
    pub max_message_chars: f64,
    pub max_pending_per_session: f64,
    pub rate_limit_capacity: f64,
    pub rate_limit_refill_ms: f64,
}

/// Structural reservation key for sibling-scoped session names. JSON encoding keeps
/// parent paths and names containing delimiter characters from colliding into one key;
/// the worker- and supervisor-side reservation maps must never diverge in encoding.
pub fn session_name_reservation_key(input: &AgentSessionNameScope, name: &str) -> String {
    let (parent_type, parent_value) = if input.depth == 0.0 {
        ("root", String::new())
    } else if let Some(parent_session_path) = &input.parent_session_path {
        ("path", canonical_session_path(parent_session_path))
    } else if let Some(parent_session_id) = &input.parent_session_id {
        ("id", parent_session_id.clone())
    } else {
        ("root", String::new())
    };
    serde_json::to_string(&serde_json::json!([
        input.depth,
        parent_type,
        parent_value,
        name
    ]))
    .unwrap_or_default()
}

pub fn format_agent_session_name_unavailable(name: &str, depth: f64) -> String {
    format!(
        "Agent name \"{name}\" is unavailable: an agent of that name already exists at depth {depth} under this parent"
    )
}

pub fn assert_agent_session_name_available(
    catalog: &[AgentFamilyCatalogEntry],
    input: &AgentSessionNameAvailabilityInput,
) -> Result<(), String> {
    let conflict = catalog.iter().any(|entry| {
        Some(entry.id.as_str()) != input.ignore_session_id.as_deref()
            && entry.name.as_deref() == Some(input.name.as_str())
            && entry.depth == input.depth
            && same_agent_session_name_parent(entry, input, catalog)
    });
    if conflict {
        return Err(format_agent_session_name_unavailable(
            &input.name,
            input.depth,
        ));
    }
    Ok(())
}

fn catalog_entry_scope(entry: &AgentFamilyCatalogEntry) -> AgentSessionNameScope {
    AgentSessionNameScope {
        parent_session_id: entry.parent_session_id.clone(),
        parent_session_path: entry.parent_session_path.clone(),
        depth: entry.depth,
    }
}

pub fn build_agent_family_roster(
    current: &AgentFamilyCatalogEntry,
    catalog: &[AgentFamilyCatalogEntry],
) -> AgentFamilyRosterResult {
    let parent = catalog
        .iter()
        .find(|entry| is_agent_family_parent(entry, current));
    let mut siblings: Vec<&AgentFamilyCatalogEntry> = catalog
        .iter()
        .filter(|entry| {
            entry.id != current.id
                && entry.depth == current.depth
                && same_agent_family_parent(entry, current, catalog)
        })
        .collect();
    let mut children: Vec<&AgentFamilyCatalogEntry> = catalog
        .iter()
        .filter(|entry| {
            entry.depth == current.depth + 1.0 && is_agent_family_parent(current, entry)
        })
        .collect();

    let name_of =
        |entry: &AgentFamilyCatalogEntry| entry.name.clone().unwrap_or_else(|| entry.id.clone());
    // `localeCompare` on the default locale orders plain identifiers like Rust's
    // byte comparison for the ASCII session names this catalogue contains.
    siblings.sort_by_key(|entry| name_of(entry));
    children.sort_by_key(|entry| name_of(entry));

    let row = |relationship: &str, entry: &AgentFamilyCatalogEntry| AgentFamilyRosterEntry {
        relationship: relationship.to_string(),
        name: name_of(entry),
        id: entry.id.clone(),
        depth: entry.depth,
        status: entry.status.clone(),
        replied_since_task: if relationship == FAMILY_RELATIONSHIP_CHILD {
            entry.replied_since_task
        } else {
            None
        },
    };

    let mut entries: Vec<AgentFamilyRosterEntry> = Vec::new();
    if let Some(parent) = parent {
        entries.push(row(FAMILY_RELATIONSHIP_PARENT, parent));
    }
    for entry in siblings {
        entries.push(row(FAMILY_RELATIONSHIP_SIBLING, entry));
    }
    for entry in children {
        entries.push(row(FAMILY_RELATIONSHIP_CHILD, entry));
    }

    AgentFamilyRosterResult {
        current: AgentFamilyRosterCurrent {
            name: name_of(current),
            id: current.id.clone(),
            depth: current.depth,
        },
        entries,
    }
}

fn same_agent_session_name_parent(
    left: &AgentSessionNameScope,
    right: &AgentSessionNameScope,
    catalog: &[AgentFamilyCatalogEntry],
) -> bool {
    if left.depth == 0.0 && right.depth == 0.0 {
        return true;
    }
    same_agent_family_parent(left, right, catalog)
}

fn same_agent_family_parent(
    left: &AgentSessionNameScope,
    right: &AgentSessionNameScope,
    catalog: &[AgentFamilyCatalogEntry],
) -> bool {
    if left.parent_session_path.is_some() && left.parent_session_path == right.parent_session_path {
        return true;
    }
    if left.parent_session_id.is_some() && left.parent_session_id == right.parent_session_id {
        return true;
    }
    let has_catalog_parent_pair =
        |parent_session_id: Option<&String>, parent_session_path: Option<&String>| {
            let (Some(parent_session_id), Some(parent_session_path)) =
                (parent_session_id, parent_session_path)
            else {
                return false;
            };
            catalog.iter().any(|entry| {
                (Some(entry.id.as_str()) == Some(parent_session_id.as_str())
                    && entry.session_path.as_deref() == Some(parent_session_path.as_str()))
                    || (entry.parent_session_id.as_deref() == Some(parent_session_id.as_str())
                        && entry.parent_session_path.as_deref()
                            == Some(parent_session_path.as_str()))
            })
        };
    if has_catalog_parent_pair(
        left.parent_session_id.as_ref(),
        right.parent_session_path.as_ref(),
    ) || has_catalog_parent_pair(
        right.parent_session_id.as_ref(),
        left.parent_session_path.as_ref(),
    ) {
        return true;
    }
    if left.depth == 0.0
        && right.depth == 0.0
        && left.parent_session_path.is_none()
        && right.parent_session_path.is_none()
        && left.parent_session_id.is_none()
        && right.parent_session_id.is_none()
    {
        return true;
    }
    // Unresolved mixed identifiers stay unrelated to avoid false name conflicts across families.
    false
}

fn is_agent_family_parent(
    parent: &AgentFamilyCatalogEntry,
    child: &AgentFamilyCatalogEntry,
) -> bool {
    (child.parent_session_path.is_some() && child.parent_session_path == parent.session_path)
        || (child.parent_session_id.is_some() && child.parent_session_id == Some(parent.id.clone()))
}

/// Pure nuclear-family policy over persisted parent-edge snapshots.
pub fn agent_family_relationship(
    current: &AgentFamilyCatalogEntry,
    target: &AgentFamilyCatalogEntry,
) -> Option<AgentFamilyRelationship> {
    if current.id == target.id {
        return None;
    }
    if is_agent_family_parent(target, current) {
        return Some(FAMILY_RELATIONSHIP_PARENT.to_string());
    }
    if is_agent_family_parent(current, target) {
        return Some(FAMILY_RELATIONSHIP_CHILD.to_string());
    }
    if current.depth == target.depth {
        let pair = [current.clone(), target.clone()];
        if same_agent_family_parent(
            &catalog_entry_scope(current),
            &catalog_entry_scope(target),
            &pair,
        ) {
            return Some(FAMILY_RELATIONSHIP_SIBLING.to_string());
        }
    }
    None
}

pub fn assert_agent_family_reach(
    current: &AgentFamilyCatalogEntry,
    target: &AgentFamilyCatalogEntry,
) -> Result<AgentFamilyRelationship, String> {
    match agent_family_relationship(current, target) {
        Some(relationship) => Ok(relationship),
        None => Err(AGENT_FAMILY_REACH_ERROR.to_string()),
    }
}

pub fn create_agent_session_message_id() -> String {
    format!("agentmsg_{}", uuid::Uuid::new_v4())
}

pub fn normalize_agent_session_message(message: &str, max_chars: usize) -> Result<String, String> {
    let trimmed = message.trim();
    if trimmed.is_empty() {
        return Err("Agent session message cannot be empty".to_string());
    }
    let length = trimmed.chars().count();
    if length > max_chars {
        return Err(format!(
            "Agent session message is too long: {length} chars exceeds {max_chars}"
        ));
    }
    Ok(trimmed.to_string())
}

pub fn assert_direct_agent_message_target(target: &str) -> Result<String, String> {
    let normalized = target.trim();
    if normalized.is_empty() {
        return Err("Agent message target cannot be empty".to_string());
    }
    let lower = normalized.to_lowercase();
    if normalized == "*" || lower == "all" || lower == "broadcast" {
        return Err("Broadcast agent messaging is not supported".to_string());
    }
    Ok(normalized.to_string())
}

pub fn assert_agent_message_queue_capacity(
    unfinished_action_count: f64,
    max_pending: usize,
) -> Result<(), String> {
    if unfinished_action_count >= max_pending as f64 {
        return Err(format!(
            "Target session has too many pending messages: {unfinished_action_count} unfinished, limit is {max_pending}"
        ));
    }
    Ok(())
}

/// `parseAgentSessionMessagePromptId`: `\n`-split lines, with the optional
/// `[from ...]` relationship line shifting the fixed offsets by one.
pub fn parse_agent_session_message_prompt_id(text: &str) -> Option<String> {
    let lines: Vec<&str> = text.split('\n').collect();
    let offset = if lines
        .first()
        .map(|line| line.starts_with("[from "))
        .unwrap_or(false)
    {
        1
    } else {
        0
    };
    if lines.get(offset).copied() != Some("Agent-to-agent message received.") {
        return None;
    }
    if lines.get(offset + 1).copied() != Some(&format!("Source: {AGENT_MESSAGE_SOURCE}")) {
        return None;
    }
    let to_line_index = if lines
        .get(offset + 2)
        .map(|line| line.starts_with("From: "))
        .unwrap_or(false)
    {
        offset + 3
    } else {
        offset + 2
    };
    if !lines
        .get(to_line_index)
        .map(|line| line.starts_with("To: "))
        .unwrap_or(false)
    {
        return None;
    }
    let candidate = lines.get(to_line_index + 1).copied().unwrap_or("");
    let id = candidate.strip_prefix("Message id: ")?;
    if !id.starts_with("agentmsg_") || id.contains('\n') {
        return None;
    }
    Some(id.to_string())
}

pub fn is_agent_session_message_prompt(text: &str) -> bool {
    parse_agent_session_message_prompt_id(text).is_some()
}

pub fn create_agent_session_message_prompt(payload: &AgentSessionMessagePayload) -> String {
    let relationship_label = payload.from_relationship.as_ref().map(|relationship| {
        if relationship == FAMILY_RELATIONSHIP_PARENT {
            format!("[from {relationship}]")
        } else {
            let sender = payload.from.as_ref();
            let name = sender
                .and_then(|sender| sender.session_name.clone())
                .or_else(|| sender.and_then(|sender| sender.session_id.clone()))
                .or_else(|| sender.and_then(|sender| sender.active_session_id.clone()))
                .unwrap_or_else(|| "unknown".to_string());
            format!(
                "[from {relationship}:{}]",
                format_agent_session_message_metadata(&name)
            )
        }
    });
    let mut lines: Vec<String> = Vec::new();
    if let Some(label) = relationship_label {
        lines.push(label);
    }
    lines.push("Agent-to-agent message received.".to_string());
    lines.push(format!("Source: {}", payload.source));
    if let Some(from) = &payload.from {
        lines.push(format!(
            "From: {}",
            format_agent_session_message_sender(from)
        ));
    }
    lines.push(format!(
        "To: {}",
        format_agent_session_message_endpoint(&payload.target)
    ));
    lines.push(format!("Message id: {}", payload.id));
    lines.push(String::new());
    lines.push(payload.message.clone());
    lines.join("\n")
}

pub fn create_agent_session_message(
    payload: &AgentSessionMessagePayload,
    timestamp: i64,
) -> CustomAgentMessage {
    let details = AgentSessionMessageDetails {
        id: payload.id.clone(),
        message: payload.message.clone(),
        from: payload.from.clone(),
        from_relationship: payload.from_relationship.clone(),
        target: Some(payload.target.clone()),
    };
    CustomAgentMessage::Custom {
        custom_type: AGENT_MESSAGE_CUSTOM_TYPE.to_string(),
        content: CustomMessageContent::Text(create_agent_session_message_prompt(payload)),
        display: true,
        details: serde_json::to_value(&details).ok(),
        timestamp,
    }
}

pub fn is_agent_session_message(message: &AgentMessage) -> bool {
    let AgentMessage::Custom(CustomAgentMessage::Custom {
        custom_type,
        details,
        ..
    }) = message
    else {
        return false;
    };
    if custom_type != AGENT_MESSAGE_CUSTOM_TYPE {
        return false;
    }
    let Some(details) = details.as_ref().and_then(Value::as_object) else {
        return false;
    };
    details.get("id").map(Value::is_string).unwrap_or(false)
        && details
            .get("message")
            .map(Value::is_string)
            .unwrap_or(false)
}

// A message that starts a new agent run (prompt-turn boundary).
pub fn starts_agent_run(message: &AgentMessage) -> bool {
    if matches!(
        message,
        AgentMessage::Message(pi_ai::types::Message::User(_))
    ) {
        return true;
    }
    if is_agent_session_message(message) {
        return true;
    }
    matches!(
        message,
        AgentMessage::Custom(CustomAgentMessage::Custom { custom_type, .. })
            if custom_type == HEARTBEAT_PROMPT_CUSTOM_TYPE
                || custom_type == ASYNC_BASH_COMPLETION_CUSTOM_TYPE
    )
}

pub fn create_agent_session_message_receipt(
    payload: &AgentSessionMessagePayload,
    status: &AgentSessionMessageDeliveryStatus,
    at: &str,
) -> AgentSessionMessageReceipt {
    AgentSessionMessageReceipt {
        id: payload.id.clone(),
        source: payload.source.clone(),
        target: payload.target.clone(),
        from: payload.from.clone(),
        message: payload.message.clone(),
        delivery_status: status.clone(),
        delivered_at: if status == DELIVERY_STATUS_DELIVERED {
            Some(at.to_string())
        } else {
            None
        },
        queued_at: if status == DELIVERY_STATUS_DELIVERED {
            None
        } else {
            Some(at.to_string())
        },
        delivery_mode: Some(DELIVERY_MODE_STEER.to_string()),
    }
}

fn format_agent_session_message_metadata(value: &str) -> String {
    let mut out = String::new();
    let mut last_was_space = false;
    for ch in value.chars() {
        let is_separator = ch.is_whitespace() || ch == ',' || ch == '[' || ch == ']';
        if is_separator {
            if !last_was_space {
                out.push(' ');
                last_was_space = true;
            }
        } else {
            out.push(ch);
            last_was_space = false;
        }
    }
    out.trim().to_string()
}

fn format_agent_session_message_sender(sender: &AgentSessionMessageSender) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(session_name) = &sender.session_name {
        let formatted = format_agent_session_message_metadata(session_name);
        if !formatted.is_empty() {
            parts.push(formatted);
        }
    }
    if let Some(active_session_id) = &sender.active_session_id {
        parts.push(format!(
            "active {}",
            format_agent_session_message_metadata(active_session_id)
        ));
    }
    if let Some(session_id) = &sender.session_id {
        parts.push(format!(
            "session {}",
            format_agent_session_message_metadata(session_id)
        ));
    }
    if let Some(client_id) = &sender.client_id {
        parts.push(format!(
            "client {}",
            format_agent_session_message_metadata(client_id)
        ));
    }
    if parts.is_empty() {
        "unknown sender".to_string()
    } else {
        parts.join(", ")
    }
}

fn format_agent_session_message_endpoint(endpoint: &AgentSessionMessageEndpoint) -> String {
    let name = endpoint
        .session_name
        .as_ref()
        .map(|name| format!("{}, ", format_agent_session_message_metadata(name)))
        .unwrap_or_default();
    format!(
        "{name}active {}, session {}",
        format_agent_session_message_metadata(&endpoint.active_session_id),
        format_agent_session_message_metadata(&endpoint.session_id)
    )
}

/// Rate limiter state for one key (`{ tokens, updatedAt }`).
#[derive(Debug, Clone, Copy, PartialEq)]
struct RateLimitBucket {
    tokens: f64,
    updated_at: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RateLimitResult {
    Ok,
    Exceeds { retry_after_ms: f64 },
}

/// Token-bucket limiter; `now` is injected so tests stay deterministic.
pub struct AgentSessionMessageRateLimiter {
    capacity: f64,
    refill_ms: f64,
    buckets: HashMap<String, RateLimitBucket>,
    now: Box<dyn Fn() -> f64 + Send + Sync>,
}

impl Default for AgentSessionMessageRateLimiter {
    fn default() -> Self {
        Self::new(
            DEFAULT_AGENT_MESSAGE_RATE_LIMIT_CAPACITY,
            DEFAULT_AGENT_MESSAGE_RATE_LIMIT_REFILL_MS,
            None,
        )
    }
}

impl AgentSessionMessageRateLimiter {
    pub fn new(
        capacity: f64,
        refill_ms: f64,
        now: Option<Box<dyn Fn() -> f64 + Send + Sync>>,
    ) -> Self {
        Self {
            capacity,
            refill_ms,
            buckets: HashMap::new(),
            now: now.unwrap_or_else(|| {
                Box::new(|| {
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|duration| duration.as_millis() as f64)
                        .unwrap_or(0.0)
                })
            }),
        }
    }

    pub fn try_consume(&mut self, key: &str) -> RateLimitResult {
        let now = (self.now)();
        let mut bucket = self.buckets.get(key).copied().unwrap_or(RateLimitBucket {
            tokens: self.capacity,
            updated_at: now,
        });
        let elapsed = f64::max(0.0, now - bucket.updated_at);
        let refilled_tokens = (elapsed / self.refill_ms).floor();
        if refilled_tokens > 0.0 {
            bucket.tokens = f64::min(self.capacity, bucket.tokens + refilled_tokens);
            bucket.updated_at += refilled_tokens * self.refill_ms;
        }
        if bucket.tokens <= 0.0 {
            self.buckets.insert(key.to_string(), bucket);
            return RateLimitResult::Exceeds {
                retry_after_ms: f64::max(1.0, bucket.updated_at + self.refill_ms - now),
            };
        }
        bucket.tokens -= 1.0;
        self.buckets.insert(key.to_string(), bucket);
        RateLimitResult::Ok
    }

    pub fn refund(&mut self, key: &str) {
        let Some(mut bucket) = self.buckets.get(key).copied() else {
            return;
        };
        bucket.tokens = f64::min(self.capacity, bucket.tokens + 1.0);
        self.buckets.insert(key.to_string(), bucket);
    }

    pub fn clear(&mut self, key: Option<&str>) {
        match key {
            Some(key) => {
                self.buckets.remove(key);
            }
            None => self.buckets.clear(),
        }
    }

    pub fn clear_matching(&mut self, predicate: impl Fn(&str) -> bool) {
        let keys: Vec<String> = self
            .buckets
            .keys()
            .filter(|key| predicate(key))
            .cloned()
            .collect();
        for key in keys {
            self.buckets.remove(&key);
        }
    }
}

/// Controller surface used by the host request handlers.
///
/// `AgentSessionMessageController` is a TypeScript interface with optional
/// members; the Rust trait keeps the same three members and lets each
/// implementation decide whether the optional ones exist.
pub trait AgentSessionMessageController: Send + Sync {
    fn roster(
        &self,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<AgentFamilyRosterResult, String>> + Send>,
    >;
    fn await_pending_child_publication(
        &self,
        selector: String,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Option<String>, String>> + Send>>;
    fn send_agent_message(
        &self,
        input: AgentSessionMessageSendInput,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<AgentSessionMessageReceipt, String>> + Send>,
    >;
}

/// `createAgentMessageHostHandlers(controller)`.
pub fn create_agent_message_host_handlers(
    controller: std::sync::Arc<dyn AgentSessionMessageController>,
) -> crate::core::kernel::shared::HostRequestHandlers {
    use crate::core::kernel::shared::{HostRequestHandler, HostRequestHandlers, KernelError};
    use std::collections::HashMap;
    use std::sync::Arc;

    let mut handlers: crate::core::kernel::shared::HostRequestHandlers = HashMap::new();

    let roster_controller = controller.clone();
    let list_agents: HostRequestHandler = Arc::new(move |_payload: Value| {
        let controller = roster_controller.clone();
        Box::pin(async move {
            match controller.roster().await {
                Ok(roster) => Ok(serde_json::to_value(roster).unwrap_or(Value::Null)),
                Err(error) => Err(KernelError::new(error)),
            }
        })
    });
    handlers.insert("agent_message.list_agents".to_string(), list_agents);

    let send_controller = controller.clone();
    let send: HostRequestHandler = Arc::new(move |payload: Value| {
        let controller = send_controller.clone();
        Box::pin(async move { handle_agent_message_send(controller, payload).await })
    });
    handlers.insert("agent_message.send".to_string(), send);

    handlers
}

async fn handle_agent_message_send(
    controller: Arc<dyn AgentSessionMessageController>,
    payload: Value,
) -> Result<Value, crate::core::kernel::shared::KernelError> {
    use crate::core::kernel::shared::KernelError;

    let object = payload.as_object().cloned().unwrap_or_default();
    let message = match object.get("message") {
        Some(Value::String(message)) => message.clone(),
        _ => {
            return Err(KernelError::new(
                "agent_message.send message must be a string",
            ))
        }
    };
    let receiver_role = object
        .get("receiver_role")
        .and_then(Value::as_str)
        .map(str::to_string);
    let receiver_name = object
        .get("receiver_name")
        .and_then(Value::as_str)
        .map(str::to_string);

    let target: String;
    if let Some(Value::String(positional)) = object.get("target") {
        if positional != "all" {
            return Err(KernelError::new(
                "positional agent_message.send targets are not supported; use receiver_role and receiver_name",
            ));
        }
        if receiver_role.is_some() || receiver_name.is_some() {
            return Err(KernelError::new(
                "agent_message.send broadcast cannot be combined with receiver_role/receiver_name",
            ));
        }
        let roster = controller.roster().await.map_err(KernelError::new)?;
        let mut receipts: Vec<Value> = Vec::with_capacity(roster.entries.len());
        for entry in &roster.entries {
            let input = AgentSessionMessageSendInput {
                target: entry.id.clone(),
                message: message.clone(),
                receiver_role: Some(entry.relationship.clone()),
            };
            match controller.send_agent_message(input).await {
                Ok(receipt) => receipts.push(serde_json::to_value(receipt).unwrap_or(Value::Null)),
                Err(error) => {
                    let mut failed = Map::new();
                    failed.insert("target".to_string(), Value::String(entry.id.clone()));
                    failed.insert("error".to_string(), Value::String(error));
                    receipts.push(Value::Object(failed));
                }
            }
        }
        return Ok(serde_json::json!({ "receipts": receipts }));
    }

    let role =
        match receiver_role.as_deref() {
            Some("parent") => FAMILY_RELATIONSHIP_PARENT.to_string(),
            Some("sibling") => FAMILY_RELATIONSHIP_SIBLING.to_string(),
            Some("child") => FAMILY_RELATIONSHIP_CHILD.to_string(),
            _ => return Err(KernelError::new(
                "agent_message.send receiver_role must be \"parent\", \"sibling\", or \"child\"",
            )),
        };
    if role == FAMILY_RELATIONSHIP_PARENT && receiver_name.is_some() {
        return Err(KernelError::new(
            "agent_message.send receiver_name must be omitted for parent messages",
        ));
    }
    if role != FAMILY_RELATIONSHIP_PARENT
        && receiver_name
            .as_ref()
            .map(|name| name.trim().is_empty())
            .unwrap_or(true)
    {
        return Err(KernelError::new(
            "agent_message.send receiver_name is required for sibling and child messages",
        ));
    }

    let selector = receiver_name.as_ref().map(|name| name.trim().to_string());
    let published_id = if role == FAMILY_RELATIONSHIP_CHILD {
        match &selector {
            Some(selector) => controller
                .await_pending_child_publication(selector.clone())
                .await
                .map_err(KernelError::new)?,
            None => None,
        }
    } else {
        None
    };
    let roster = controller.roster().await.map_err(KernelError::new)?;
    let matches: Vec<&AgentFamilyRosterEntry> = roster
        .entries
        .iter()
        .filter(|entry| {
            entry.relationship == role
                && (role == FAMILY_RELATIONSHIP_PARENT
                    || Some(entry.name.as_str()) == selector.as_deref()
                    || Some(entry.id.as_str()) == selector.as_deref()
                    || published_id.as_deref() == Some(entry.id.as_str()))
        })
        .collect();
    if matches.len() != 1 {
        return Err(KernelError::new(if matches.is_empty() {
            if role == FAMILY_RELATIONSHIP_PARENT {
                "No parent matches the current agent".to_string()
            } else {
                format!(
                    "No {role} matches {}",
                    serde_json::to_string(&receiver_name).unwrap_or_default()
                )
            }
        } else {
            format!(
                "{role} selector {} is ambiguous",
                serde_json::to_string(&receiver_name).unwrap_or_default()
            )
        }));
    }
    target = matches[0].id.clone();

    let receipt = controller
        .send_agent_message(AgentSessionMessageSendInput {
            target,
            message,
            receiver_role: Some(role),
        })
        .await
        .map_err(KernelError::new)?;
    Ok(serde_json::to_value(receipt).unwrap_or(Value::Null))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn endpoint(name: Option<&str>, active: &str, session: &str) -> AgentSessionMessageEndpoint {
        AgentSessionMessageEndpoint {
            active_session_id: active.to_string(),
            session_id: session.to_string(),
            session_name: name.map(str::to_string),
            runtime_kind: None,
        }
    }

    fn payload() -> AgentSessionMessagePayload {
        AgentSessionMessagePayload {
            id: "agentmsg_1".to_string(),
            source: AGENT_MESSAGE_SOURCE.to_string(),
            message: "hello".to_string(),
            from: Some(AgentSessionMessageSender {
                active_session_id: Some("active-1".to_string()),
                session_id: Some("session-1".to_string()),
                session_name: Some("parent".to_string()),
                ..Default::default()
            }),
            from_relationship: None,
            target: endpoint(Some("child"), "active-2", "session-2"),
        }
    }

    #[test]
    fn participant_format_falls_back_to_unknown() {
        assert_eq!(
            format_agent_message_participant(
                &AGENT_MESSAGE_DIRECTION_RECEIVED.to_string(),
                None,
                None
            ),
            "from unknown"
        );
        let sender = AgentSessionMessageSender {
            session_name: Some("  name  ".to_string()),
            ..Default::default()
        };
        assert_eq!(
            format_agent_message_participant(
                &AGENT_MESSAGE_DIRECTION_SENT.to_string(),
                Some(&FAMILY_RELATIONSHIP_CHILD.to_string()),
                Some(&sender)
            ),
            "to child name"
        );
    }

    #[test]
    fn prompt_round_trips_through_the_id_parser() {
        let prompt = create_agent_session_message_prompt(&payload());
        assert!(is_agent_session_message_prompt(&prompt));
        assert_eq!(
            parse_agent_session_message_prompt_id(&prompt).as_deref(),
            Some("agentmsg_1")
        );
    }

    #[test]
    fn prompt_with_relationship_line_keeps_the_id_offsets() {
        let mut payload = payload();
        payload.from_relationship = Some(FAMILY_RELATIONSHIP_SIBLING.to_string());
        let prompt = create_agent_session_message_prompt(&payload);
        assert!(prompt.starts_with("[from sibling:parent]"));
        assert_eq!(
            parse_agent_session_message_prompt_id(&prompt).as_deref(),
            Some("agentmsg_1")
        );
        let parent = {
            let mut payload = payload();
            payload.from_relationship = Some(FAMILY_RELATIONSHIP_PARENT.to_string());
            create_agent_session_message_prompt(&payload)
        };
        assert!(parent.starts_with("[from parent]"));
        assert_eq!(
            parse_agent_session_message_prompt_id(&parent).as_deref(),
            Some("agentmsg_1")
        );
    }

    #[test]
    fn prompt_parser_rejects_unrelated_text() {
        assert!(!is_agent_session_message_prompt("hello"));
        assert!(parse_agent_session_message_prompt_id(
            "Agent-to-agent message received.\nSource: other\nTo: x\nMessage id: agentmsg_1"
        )
        .is_none());
    }

    #[test]
    fn message_detection_uses_details_shape() {
        let message: AgentMessage = crate::core::messages::custom_message_to_agent_message(
            match create_agent_session_message(&payload(), 5) {
                CustomAgentMessage::Custom {
                    custom_type,
                    content,
                    display,
                    details,
                    timestamp,
                } => crate::core::messages::CustomMessage {
                    role: "custom".to_string(),
                    custom_type,
                    content,
                    display,
                    details,
                    timestamp,
                },
                _ => unreachable!("agent session messages are custom messages"),
            },
        );
        assert!(is_agent_session_message(&message));
        assert!(starts_agent_run(&message));
        let other = AgentMessage::Custom(CustomAgentMessage::Custom {
            custom_type: "other".to_string(),
            content: CustomMessageContent::Text("x".to_string()),
            display: true,
            details: None,
            timestamp: 1,
        });
        assert!(!is_agent_session_message(&other));
        assert!(!starts_agent_run(&other));
    }

    #[test]
    fn receipt_marks_delivered_and_queued_timestamps() {
        let delivered = create_agent_session_message_receipt(
            &payload(),
            &DELIVERY_STATUS_DELIVERED.to_string(),
            "2026-01-01T00:00:00.000Z",
        );
        assert_eq!(
            delivered.delivered_at.as_deref(),
            Some("2026-01-01T00:00:00.000Z")
        );
        assert!(delivered.queued_at.is_none());
        assert_eq!(delivered.delivery_mode.as_deref(), Some("steer"));
        let queued = create_agent_session_message_receipt(
            &payload(),
            &DELIVERY_STATUS_QUEUED.to_string(),
            "2026-01-01T00:00:00.000Z",
        );
        assert!(queued.delivered_at.is_none());
        assert!(queued.queued_at.is_some());
    }

    #[test]
    fn message_normalisation_enforces_limits() {
        assert_eq!(normalize_agent_session_message("  hi  ", 10).unwrap(), "hi");
        assert!(normalize_agent_session_message("   ", 10).is_err());
        assert_eq!(
            normalize_agent_session_message("abcd", 3).unwrap_err(),
            "Agent session message is too long: 4 chars exceeds 3"
        );
    }

    #[test]
    fn direct_target_rejects_broadcast() {
        assert_eq!(
            assert_direct_agent_message_target(" sub-1 ").unwrap(),
            "sub-1"
        );
        assert!(assert_direct_agent_message_target("").is_err());
        assert!(assert_direct_agent_message_target("*").is_err());
        assert!(assert_direct_agent_message_target("ALL").is_err());
        assert!(assert_direct_agent_message_target("Broadcast").is_err());
    }

    #[test]
    fn queue_capacity_matches_limit_message() {
        assert!(assert_agent_message_queue_capacity(19.0, 20).is_ok());
        assert_eq!(
            assert_agent_message_queue_capacity(20.0, 20).unwrap_err(),
            "Target session has too many pending messages: 20 unfinished, limit is 20"
        );
    }

    fn entry(id: &str, name: &str, depth: f64, parent_id: Option<&str>) -> AgentFamilyCatalogEntry {
        AgentFamilyCatalogEntry {
            id: id.to_string(),
            name: Some(name.to_string()),
            depth,
            status: FAMILY_STATUS_RUNNING.to_string(),
            replied_since_task: Some(false),
            parent_session_id: parent_id.map(str::to_string),
            parent_session_path: None,
            session_path: Some(format!("/sessions/{id}.jsonl")),
        }
    }

    #[test]
    fn relationship_covers_parent_child_sibling_and_none() {
        let current = entry("self", "self", 1.0, Some("parent"));
        let parent = entry("parent", "parent", 0.0, None);
        let sibling = entry("sib", "sib", 1.0, Some("parent"));
        let child = entry("kid", "kid", 2.0, Some("self"));
        assert_eq!(
            agent_family_relationship(&current, &parent).as_deref(),
            Some("parent")
        );
        assert_eq!(
            agent_family_relationship(&current, &child).as_deref(),
            Some("child")
        );
        assert_eq!(
            agent_family_relationship(&current, &sibling).as_deref(),
            Some("sibling")
        );
        assert!(agent_family_relationship(&current, &current).is_none());
        assert!(
            agent_family_relationship(&current, &entry("far", "far", 3.0, Some("other"))).is_none()
        );
        assert_eq!(
            assert_agent_family_reach(&current, &entry("far", "far", 3.0, Some("other")))
                .unwrap_err(),
            AGENT_FAMILY_REACH_ERROR
        );
    }

    #[test]
    fn roster_orders_parent_siblings_then_children() {
        let current = entry("self", "self", 1.0, Some("parent"));
        let catalog = vec![
            current.clone(),
            entry("parent", "parent", 0.0, None),
            entry("sib-b", "b", 1.0, Some("parent")),
            entry("sib-a", "a", 1.0, Some("parent")),
            entry("kid", "kid", 2.0, Some("self")),
        ];
        let roster = build_agent_family_roster(&current, &catalog);
        assert_eq!(roster.current.name, "self");
        let relationships: Vec<&str> = roster
            .entries
            .iter()
            .map(|entry| entry.relationship.as_str())
            .collect();
        assert_eq!(relationships, vec!["parent", "sibling", "sibling", "child"]);
        assert_eq!(roster.entries[1].name, "a");
        assert_eq!(roster.entries[3].replied_since_task, Some(false));
        assert!(roster.entries[0].replied_since_task.is_none());
    }

    #[test]
    fn name_availability_reports_the_conflict_message() {
        let catalog = vec![entry("self", "taken", 1.0, Some("parent"))];
        let input = AgentSessionNameAvailabilityInput {
            parent_session_id: Some("parent".to_string()),
            parent_session_path: None,
            depth: 1.0,
            name: "taken".to_string(),
            ignore_session_id: None,
        };
        assert_eq!(
            assert_agent_session_name_available(&catalog, &input).unwrap_err(),
            "Agent name \"taken\" is unavailable: an agent of that name already exists at depth 1 under this parent"
        );
        let ignoring = AgentSessionNameAvailabilityInput {
            ignore_session_id: Some("self".to_string()),
            ..input.clone()
        };
        assert!(assert_agent_session_name_available(&catalog, &ignoring).is_ok());
    }

    #[test]
    fn reservation_key_encodes_depth_parent_and_name() {
        let scope = AgentSessionNameScope {
            parent_session_id: Some("parent".to_string()),
            parent_session_path: None,
            depth: 1.0,
        };
        assert_eq!(
            session_name_reservation_key(&scope, "kid"),
            "[1,\"id\",\"parent\",\"kid\"]"
        );
        let root = AgentSessionNameScope {
            parent_session_id: None,
            parent_session_path: None,
            depth: 0.0,
        };
        assert_eq!(
            session_name_reservation_key(&root, "kid"),
            "[0,\"root\",\"\",\"kid\"]"
        );
    }

    #[test]
    fn rate_limiter_refills_and_reports_retry_after() {
        let now = std::sync::Arc::new(std::sync::Mutex::new(0.0_f64));
        let clock = now.clone();
        let mut limiter = AgentSessionMessageRateLimiter::new(
            3.0,
            1000.0,
            Some(Box::new(move || *clock.lock().unwrap())),
        );
        assert_eq!(limiter.try_consume("k"), RateLimitResult::Ok);
        assert_eq!(limiter.try_consume("k"), RateLimitResult::Ok);
        assert_eq!(limiter.try_consume("k"), RateLimitResult::Ok);
        assert_eq!(
            limiter.try_consume("k"),
            RateLimitResult::Exceeds {
                retry_after_ms: 1000.0
            }
        );
        limiter.refund("k");
        assert_eq!(limiter.try_consume("k"), RateLimitResult::Ok);
        *now.lock().unwrap() = 1000.0;
        assert_eq!(limiter.try_consume("k"), RateLimitResult::Ok);
        limiter.clear_matching(|key| key == "k");
        assert_eq!(limiter.try_consume("k"), RateLimitResult::Ok);
        limiter.clear(None);
        assert_eq!(limiter.try_consume("k"), RateLimitResult::Ok);
    }

    #[test]
    fn message_id_uses_the_agentmsg_prefix() {
        assert!(create_agent_session_message_id().starts_with("agentmsg_"));
        assert!(serde_json::to_value(AGENT_MESSAGE_CUSTOM_TYPE).is_ok());
        let _ = json!({});
    }
}
