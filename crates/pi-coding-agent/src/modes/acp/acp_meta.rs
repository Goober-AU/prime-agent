//! Port of packages/coding-agent/src/modes/acp/acp-meta.ts
//!
//! Namespaced `_meta` payloads for prime-agent capabilities that ACP has no
//! native concept for (Python cell semantics, RLM subagents, autonomous gates,
//! goals, heartbeats, continual harness state).

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Reverse-domain namespace for every prime-agent `_meta` payload.
pub const PRIME_AGENT_META_NAMESPACE: &str = "ai.primeintellect.prime-agent";

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrimeAgentSubagentMeta {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_name: Option<String>,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub depth: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_count: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrimeAgentAutonomousMeta {
    pub enabled: bool,
    pub continuations_used: i64,
    pub turns_used: i64,
    pub tokens_used: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gate_attempt: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gate_failure: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit_reason: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrimeAgentIpythonAttachmentMeta {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrimeAgentIpythonMeta {
    /// Media the cell loaded into context, as reported by the ipython tool.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attachments: Option<Vec<PrimeAgentIpythonAttachmentMeta>>,
    /// Number of diffs the cell displayed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diff_count: Option<usize>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrimeAgentGoalMeta {
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub objective: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_budget: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tokens_used: Option<f64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrimeAgentRefinementMeta {
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub changes: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrimeAgentQuiescenceMeta {
    /// Subagents that have not reached a terminal state at the observation point.
    pub outstanding_subagents: usize,
    /// Autonomous continuation slots still available at the observation point.
    pub remaining_autonomous_continuations: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrimeAgentAgentMessageMeta {
    pub tool_call_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delivery_status: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrimeAgentCwdMeta {
    /// The cwd the client asked for.
    pub requested: String,
    /// The cwd prime-agent is actually running in, fixed at startup.
    pub actual: String,
}

/// Producer-side ordering and causality for ACP updates.
///
/// `promptTurnId` is allocated when ACP accepts a prompt, never inferred from
/// whichever prompt happens to be running when an update is delivered. `0`
/// means a session-scoped event with no prompt origin (for example a heartbeat
/// change before the first prompt). `eventSequence` is connection-wide and
/// strictly increases for every update Prime Agent publishes.
pub const PHASE_EVENT: &str = "event";
pub const PHASE_RESPONSE_BOUNDARY: &str = "responseBoundary";
pub const PHASE_TERMINAL_QUIESCENCE: &str = "terminalQuiescence";

pub type PrimeAgentEventPhase = &'static str;

/// The outcome carried by a correlated response boundary and terminal envelope.
pub const OUTCOME_RESULT: &str = "result";
pub const OUTCOME_ERROR: &str = "error";

pub type PrimeAgentResponseOutcome = &'static str;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrimeAgentCompactionMeta {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tokens_before: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrimeAgentSessionMeta {
    /// Monotonically increasing ACP prompt turn which caused this update.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_turn_id: Option<i64>,
    /// Strictly increasing producer sequence, across all ACP updates.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event_sequence: Option<i64>,
    /// Whether this is ordinary work, the prompt response boundary, or final quiescence.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    /// The boundary/terminal outcome. This deliberately has only `result` and
    /// `error`: ACP's transport stop reasons (including `end_turn`) are never a
    /// causal completion signal.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
    /// Whether an accepted response boundary promises a later terminal-quiescence envelope.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminal_quiescence_expected: Option<bool>,
    /// Present when a client-requested cwd differs from the agent's real cwd.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PrimeAgentCwdMeta>,
    /// Set when the session's heartbeat or cron schedule changed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub heartbeats_changed: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub goal: Option<PrimeAgentGoalMeta>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refinement: Option<PrimeAgentRefinementMeta>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_message: Option<PrimeAgentAgentMessageMeta>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rlm_depth: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rlm_max_depth: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compaction: Option<PrimeAgentCompactionMeta>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subagents: Option<Vec<PrimeAgentSubagentMeta>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub autonomous: Option<PrimeAgentAutonomousMeta>,
    /// Observed subagent and autonomous-continuation counts at completion.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quiescence: Option<PrimeAgentQuiescenceMeta>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ipython: Option<PrimeAgentIpythonMeta>,
}

impl PrimeAgentSessionMeta {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn to_value(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }
}

/// Wrap a prime-agent payload in its reverse-domain `_meta` envelope.
pub fn prime_agent_meta(payload: PrimeAgentSessionMeta) -> Map<String, Value> {
    let mut outer = Map::new();
    outer.insert(PRIME_AGENT_META_NAMESPACE.to_string(), payload.to_value());
    outer
}

/// Convenience for the tests and callers that build `_meta` from raw keys.
pub fn prime_agent_meta_from_map(payload: IndexMap<String, Value>) -> Map<String, Value> {
    let mut inner = Map::new();
    for (key, value) in payload {
        inner.insert(key, value);
    }
    let mut outer = Map::new();
    outer.insert(PRIME_AGENT_META_NAMESPACE.to_string(), Value::Object(inner));
    outer
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_payload_in_the_namespace() {
        let mut meta = PrimeAgentSessionMeta::new();
        meta.heartbeats_changed = Some(true);
        let wrapped = prime_agent_meta(meta);
        let inner = wrapped.get(PRIME_AGENT_META_NAMESPACE).unwrap();
        assert_eq!(inner.get("heartbeatsChanged"), Some(&Value::Bool(true)));
        assert_eq!(wrapped.len(), 1);
    }

    #[test]
    fn empty_payload_serialises_as_empty_object() {
        let wrapped = prime_agent_meta(PrimeAgentSessionMeta::new());
        assert_eq!(wrapped.get(PRIME_AGENT_META_NAMESPACE).unwrap(), &Value::Object(Map::new()));
    }

    #[test]
    fn optional_fields_are_omitted() {
        let mut meta = PrimeAgentSessionMeta::new();
        meta.prompt_turn_id = Some(0);
        meta.event_sequence = Some(4);
        meta.phase = Some(PHASE_EVENT.to_string());
        let value = meta.to_value();
        assert_eq!(value.get("promptTurnId"), Some(&Value::from(0)));
        assert!(value.get("outcome").is_none());
    }
}
