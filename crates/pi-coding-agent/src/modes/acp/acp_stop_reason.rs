//! Port of packages/coding-agent/src/modes/acp/acp-stop-reason.ts

use crate::modes::agent_connection::types::{autonomous_limit_reason, AgentAutonomousStatus};

/// ACP stop reasons. `session/prompt` resolves with one of these after the agent
/// has streamed its updates.
pub const ACP_STOP_REASON_END_TURN: &str = "end_turn";
pub const ACP_STOP_REASON_MAX_TOKENS: &str = "max_tokens";
pub const ACP_STOP_REASON_MAX_TURN_REQUESTS: &str = "max_turn_requests";
pub const ACP_STOP_REASON_REFUSAL: &str = "refusal";
pub const ACP_STOP_REASON_CANCELLED: &str = "cancelled";

pub type AcpStopReason = &'static str;

/// Map a finished prime-agent turn onto an ACP stop reason.
///
/// Autonomous quality gates deliberately do NOT surface as a distinct stop
/// reason: a failing gate is a continuation inside the same prompt turn, so the
/// turn only ends once the gate loop itself is finished. What the client sees is
/// why the loop stopped, not that a gate failed mid-flight.
pub fn acp_stop_reason(cancelled: bool, autonomous: Option<&AgentAutonomousStatus>) -> AcpStopReason {
    if cancelled {
        return ACP_STOP_REASON_CANCELLED;
    }
    let status = match autonomous {
        Some(status) => status,
        None => return ACP_STOP_REASON_END_TURN,
    };
    if !status.enabled {
        return ACP_STOP_REASON_END_TURN;
    }
    let limit = autonomous_limit_reason(status);
    // Token exhaustion is the one autonomous limit ACP expresses natively. Turn,
    // continuation, and wall-clock limits all mean the agent was stopped before
    // finishing, so none of them may report as a clean end_turn.
    match limit {
        Some("maxTokens") => ACP_STOP_REASON_MAX_TOKENS,
        Some(_) => ACP_STOP_REASON_MAX_TURN_REQUESTS,
        None => ACP_STOP_REASON_END_TURN,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modes::agent_connection::types::{
        AgentAutonomousGateFailure, AgentAutonomousGateStatus, AgentAutonomousLimits,
    };

    fn status(enabled: bool, continuations_used: i64, tokens_used: i64) -> AgentAutonomousStatus {
        AgentAutonomousStatus {
            enabled,
            continuations_used: continuations_used as f64,
            turns_used: 0.0,
            tokens_used: tokens_used as f64,
            started_at: None,
            limits: AgentAutonomousLimits {
                max_continuations: 3.0,
                max_turns: 12.0,
                max_tokens: 80_000.0,
                timeout_ms: 30.0 * 60.0 * 1000.0,
            },
            gates: AgentAutonomousGateStatus {
                commands: Vec::new(),
                max_retries: 3.0,
                timeout_ms: 5.0 * 60.0 * 1000.0,
            },
            gate_attempts: Default::default(),
            last_gate_failure: None::<AgentAutonomousGateFailure>,
        }
    }

    #[test]
    fn cancelled_wins() {
        assert_eq!(acp_stop_reason(true, None), ACP_STOP_REASON_CANCELLED);
    }

    #[test]
    fn disabled_or_missing_status_is_end_turn() {
        assert_eq!(acp_stop_reason(false, None), ACP_STOP_REASON_END_TURN);
        assert_eq!(acp_stop_reason(false, Some(&status(false, 3, 0))), ACP_STOP_REASON_END_TURN);
    }

    #[test]
    fn token_limit_maps_to_max_tokens_and_others_to_max_turn_requests() {
        assert_eq!(
            acp_stop_reason(false, Some(&status(true, 0, 80_000))),
            ACP_STOP_REASON_MAX_TOKENS
        );
        assert_eq!(
            acp_stop_reason(false, Some(&status(true, 3, 0))),
            ACP_STOP_REASON_MAX_TURN_REQUESTS
        );
        assert_eq!(acp_stop_reason(false, Some(&status(true, 0, 0))), ACP_STOP_REASON_END_TURN);
    }
}
