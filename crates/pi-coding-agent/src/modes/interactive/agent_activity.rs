//! Port of packages/coding-agent/src/modes/interactive/agent-activity.ts

use pi_ai::types::AssistantMessageEvent;
use pi_agent_core::types::AgentMessage;

use super::interactive_mode_services::AgentConnectionSessionEvent;

/// `AgentActivity`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentActivity {
    Waiting,
    Thinking,
    Writing,
    WritingCode,
    Executing,
}

impl AgentActivity {
    pub fn as_str(self) -> &'static str {
        match self {
            AgentActivity::Waiting => "waiting",
            AgentActivity::Thinking => "thinking",
            AgentActivity::Writing => "writing",
            AgentActivity::WritingCode => "writing-code",
            AgentActivity::Executing => "executing",
        }
    }
}

/// `AgentActivityStatus`
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AgentActivityStatus {
    pub activity: AgentActivity,
    pub direction: Direction,
    pub tokens: f64,
}

/// `"down" | "up"`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Down,
    Up,
}

impl Direction {
    pub fn as_str(self) -> &'static str {
        match self {
            Direction::Down => "down",
            Direction::Up => "up",
        }
    }
}

/// `AGENT_ACTIVITY_LABELS`
pub fn agent_activity_label(activity: AgentActivity) -> &'static str {
    match activity {
        AgentActivity::Waiting => "Waiting",
        AgentActivity::Thinking => "Thinking",
        AgentActivity::Writing => "Writing",
        AgentActivity::WritingCode => "Writing code",
        AgentActivity::Executing => "Executing",
    }
}

const CHARS_PER_TOKEN_ESTIMATE: f64 = 4.0;

/// Port of `AgentActivityTracker`.
#[derive(Debug, Clone)]
pub struct AgentActivityTracker {
    activity: AgentActivity,
    completed_tokens: f64,
    streaming_usage_tokens: f64,
    streaming_chars: f64,
    running_tool_count: i64,
    // Providers like Anthropic only report usage at the start and end of a message, so the
    // live count leans on the character estimate in between. Keeping the reported value
    // monotonic prevents it from dipping when authoritative usage arrives at message end.
    reported_tokens: f64,
}

impl Default for AgentActivityTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl AgentActivityTracker {
    pub fn new() -> Self {
        Self {
            activity: AgentActivity::Waiting,
            completed_tokens: 0.0,
            streaming_usage_tokens: 0.0,
            streaming_chars: 0.0,
            running_tool_count: 0,
            reported_tokens: 0.0,
        }
    }

    /// Port of `handleEvent`.
    pub fn handle_event(&mut self, event: &AgentConnectionSessionEvent) {
        match event {
            AgentConnectionSessionEvent::AgentStart => {
                self.activity = AgentActivity::Waiting;
                self.running_tool_count = 0;
            }

            AgentConnectionSessionEvent::MessageStart { message } => {
                if message.role() == "user" || is_agent_session_message(message) {
                    self.reset();
                } else if message.role() == "assistant" {
                    self.activity = AgentActivity::Waiting;
                    self.streaming_usage_tokens = 0.0;
                    self.streaming_chars = 0.0;
                }
            }

            AgentConnectionSessionEvent::MessageUpdate { message, assistant_message_event } => {
                if message.role() != "assistant" {
                    return;
                }
                match assistant_message_event {
                    AssistantMessageEvent::ThinkingStart { .. } | AssistantMessageEvent::ThinkingDelta { .. } => {
                        self.activity = AgentActivity::Thinking;
                    }
                    AssistantMessageEvent::TextStart { .. } | AssistantMessageEvent::TextDelta { .. } => {
                        self.activity = AgentActivity::Writing;
                    }
                    AssistantMessageEvent::ToolCallStart { .. } | AssistantMessageEvent::ToolCallDelta { .. } => {
                        self.activity = AgentActivity::WritingCode;
                    }
                    _ => {}
                }
                if let Some(delta) = assistant_message_event_delta(assistant_message_event) {
                    self.streaming_chars += delta.chars().count() as f64;
                }
                self.streaming_usage_tokens = message_output_tokens(message);
            }

            AgentConnectionSessionEvent::MessageEnd { message } => {
                if message.role() != "assistant" {
                    return;
                }
                let output = message_output_tokens(message);
                self.completed_tokens += if output > 0.0 { output } else { self.estimated_streaming_tokens() };
                self.streaming_usage_tokens = 0.0;
                self.streaming_chars = 0.0;
                self.activity = AgentActivity::Waiting;
            }

            AgentConnectionSessionEvent::ToolExecutionStart { .. } => {
                self.running_tool_count += 1;
                self.activity = AgentActivity::Executing;
            }

            AgentConnectionSessionEvent::ToolExecutionEnd { .. } => {
                self.running_tool_count = (self.running_tool_count - 1).max(0);
                if self.running_tool_count == 0 {
                    self.activity = AgentActivity::Waiting;
                }
            }

            _ => {}
        }
        self.reported_tokens = self.reported_tokens.max(self.current_tokens());
    }

    /// Port of `getStatus`.
    pub fn get_status(&self) -> AgentActivityStatus {
        AgentActivityStatus {
            activity: self.activity,
            direction: if self.activity == AgentActivity::Waiting || self.activity == AgentActivity::Executing {
                Direction::Up
            } else {
                Direction::Down
            },
            tokens: self.reported_tokens,
        }
    }

    fn current_tokens(&self) -> f64 {
        self.completed_tokens + self.streaming_usage_tokens.max(self.estimated_streaming_tokens())
    }

    /// Port of `reset`.
    pub fn reset(&mut self) {
        self.activity = AgentActivity::Waiting;
        self.completed_tokens = 0.0;
        self.streaming_usage_tokens = 0.0;
        self.streaming_chars = 0.0;
        self.running_tool_count = 0;
        self.reported_tokens = 0.0;
    }

    fn estimated_streaming_tokens(&self) -> f64 {
        (self.streaming_chars / CHARS_PER_TOKEN_ESTIMATE).round()
    }
}

/// Port of `formatTokenCount`.
pub fn format_token_count(count: f64) -> String {
    if count < 1000.0 {
        return format_number_js(count);
    }
    if count < 10000.0 {
        return format!("{}k", js_to_fixed(count / 1000.0, 1));
    }
    if count < 1000000.0 {
        return format!("{}k", (count / 1000.0).round());
    }
    if count < 10000000.0 {
        return format!("{}M", js_to_fixed(count / 1000000.0, 1));
    }
    format!("{}M", (count / 1000000.0).round())
}

/// `count.toString()` for an integral JS number.
fn format_number_js(value: f64) -> String {
    if value.fract() == 0.0 && value.abs() < 1e21 {
        format!("{}", value as i64)
    } else {
        format!("{}", value)
    }
}

/// `(value).toFixed(digits)` for a non-negative JS number.
fn js_to_fixed(value: f64, digits: usize) -> String {
    format!("{:.*}", digits, value)
}

/// Stand-in for `isAgentSessionMessage(message)` (core/agent-messages.ts, other slice).
fn is_agent_session_message(message: &AgentMessage) -> bool {
    match message {
        AgentMessage::Custom(custom) => match custom {
            pi_agent_core::types::CustomAgentMessage::Custom { custom_type, details, .. } => {
                custom_type == "agent_message"
                    && details
                        .as_ref()
                        .and_then(|value| value.get("id"))
                        .map(|value| value.is_string())
                        .unwrap_or(false)
                    && details
                        .as_ref()
                        .and_then(|value| value.get("message"))
                        .map(|value| value.is_string())
                        .unwrap_or(false)
            }
            _ => false,
        },
        _ => false,
    }
}

fn message_output_tokens(message: &AgentMessage) -> f64 {
    match message {
        AgentMessage::Message(pi_ai::types::Message::Assistant(assistant)) => assistant.usage.output,
        _ => 0.0,
    }
}

/// `"delta" in streamEvent ? streamEvent.delta : undefined`
fn assistant_message_event_delta(event: &AssistantMessageEvent) -> Option<&str> {
    match event {
        AssistantMessageEvent::TextDelta { delta, .. }
        | AssistantMessageEvent::ThinkingDelta { delta, .. }
        | AssistantMessageEvent::ToolCallDelta { delta, .. } => Some(delta.as_str()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_token_count_thresholds() {
        assert_eq!(format_token_count(0.0), "0");
        assert_eq!(format_token_count(999.0), "999");
        assert_eq!(format_token_count(1234.0), "1.2k");
        assert_eq!(format_token_count(12345.0), "12k");
        assert_eq!(format_token_count(1_234_567.0), "1.2M");
        assert_eq!(format_token_count(12_345_678.0), "12M");
    }

    #[test]
    fn labels_match_typescript() {
        assert_eq!(agent_activity_label(AgentActivity::WritingCode), "Writing code");
        assert_eq!(AgentActivity::Writing.as_str(), "writing");
    }

    #[test]
    fn tracker_reports_up_while_waiting_and_executing() {
        let mut tracker = AgentActivityTracker::new();
        assert_eq!(tracker.get_status().direction, Direction::Up);
        tracker.handle_event(&AgentConnectionSessionEvent::ToolExecutionStart {
            tool_call_id: "t".into(),
            tool_name: "bash".into(),
            args: serde_json::Value::Null,
        });
        let status = tracker.get_status();
        assert_eq!(status.activity, AgentActivity::Executing);
        assert_eq!(status.direction, Direction::Up);
        tracker.handle_event(&AgentConnectionSessionEvent::ToolExecutionEnd {
            tool_call_id: "t".into(),
            result: serde_json::Value::Null,
            is_error: false,
        });
        assert_eq!(tracker.get_status().activity, AgentActivity::Waiting);
    }

    #[test]
    fn tracker_is_monotonic_across_message_end() {
        let mut tracker = AgentActivityTracker::new();
        let mut assistant = pi_ai::types::AssistantMessage::default();
        assistant.usage.output = 100.0;
        let message = AgentMessage::Message(pi_ai::types::Message::Assistant(assistant.clone()));
        tracker.handle_event(&AgentConnectionSessionEvent::MessageEnd { message });
        assert_eq!(tracker.get_status().tokens, 100.0);

        assistant.usage.output = 0.0;
        let message = AgentMessage::Message(pi_ai::types::Message::Assistant(assistant));
        tracker.handle_event(&AgentConnectionSessionEvent::MessageEnd { message });
        assert_eq!(tracker.get_status().tokens, 100.0);
    }
}
