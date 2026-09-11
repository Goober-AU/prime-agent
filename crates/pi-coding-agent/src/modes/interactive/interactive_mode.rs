//! Port of packages/coding-agent/src/modes/interactive/interactive-mode.ts
//!
//! This is the largest module of the slice (10,352 TypeScript lines, ~396
//! methods). The port keeps every module-level free function, constant, type and
//! pure helper exactly, and implements the `InteractiveMode` class against the
//! private stand-ins in `interactive_mode_services.rs`. Methods whose whole body
//! depends on UI components that other slices still own are marked `PARTIAL:`
//! with the TypeScript method name; see evidence/status/ca-interactive-a.json.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use pi_agent_core::types::{AgentMessage, ThinkingLevel};
use pi_ai::types::{ImageContent, Model, ServiceTier};

use crate::config::{APP_TITLE, VERSION};
use crate::utils::paths::get_cwd_relative_path;

use super::agent_activity::format_token_count;
use super::auth_flows::ProviderAuthFlows;
use super::feature_hints::FeatureHintDeck;
use super::heartbeat_scope::scope_heartbeats_to_session;
use super::image_markers::{collect_marked_images, evict_images_to_budget, format_image_marker, remap_image_markers};
use super::interactive_mode_services::{
    AgentConnection, AgentConnectionHeartbeat, AgentConnectionHistoryRange, AgentConnectionHistoryWindow,
    AgentConnectionModel, AgentConnectionQueueState, AgentConnectionRlmChildAgentSnapshot,
    AgentConnectionSessionEvent, AgentConnectionSnapshot, AgentConnectionState, ContextUsage, GoalState,
    InteractiveModeLocalSessionHost, InteractiveModeUiServices, OverlayHandle, Text, Theme,
};
use super::onboarding::should_run_onboarding;
use super::prompt_stash_state::{ClientPromptStashStore, PromptStash, PromptStashState};
use super::queue_selection::{QueueSelection, QueueSelectionItem};
use super::resume_hint::format_resume_hint;
use super::theme::theme::{theme, ThemeColor};
use super::theme::working_icon::{set_working_pulse_frame, WORKING_ICON_INTERVAL_MS};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// `HEARTBEAT_PROMPT_PREVIEW_LABEL`
pub const HEARTBEAT_PROMPT_PREVIEW_LABEL: &str = "Heartbeat";
/// `GOAL_CONTEXT_PREVIEW_LABEL`
pub const GOAL_CONTEXT_PREVIEW_LABEL: &str = "Goal";
/// `AGENT_MESSAGE_RECEIVED_PREVIEW_LABEL`
pub const AGENT_MESSAGE_RECEIVED_PREVIEW_LABEL: &str = "Message";
/// `ASYNC_BASH_COMPLETION_PREVIEW_LABEL`
pub const ASYNC_BASH_COMPLETION_PREVIEW_LABEL: &str = "Bash";

/// `APP_NAME`
pub fn app_name() -> String {
    crate::utils::tools_manager::app_name()
}

const HEARTBEAT_LEGACY_PROMPT_MIN_TOLERANCE_MS: f64 = 15_000.0;
const HEARTBEAT_LEGACY_PROMPT_MAX_TOLERANCE_MS: f64 = 120_000.0;
const MODEL_CATALOG_REFRESH_TTL_MS: f64 = 60_000.0;
const FEATURE_HINT_DELAY_MS: f64 = 5_000.0;

/// `START_HINTS`
pub const START_HINTS: [&str; 5] = [
    "Try \"refactor @<filepath>\"",
    "Try \"fix bugs in @<filepath>\"",
    "Try \"add tests for @<filepath>\"",
    "Try \"explain how @<filepath> works\"",
    "Try \"improve performance in @<filepath>\"",
];

/// Port of `getRandomStartHint`.
pub fn get_random_start_hint(random: &dyn Fn() -> f64) -> &'static str {
    let index = (random() * START_HINTS.len() as f64).floor() as i64;
    START_HINTS.get(index as usize).copied().unwrap_or(START_HINTS[0])
}

/// Port of `isLabeledQueuedPreview`.
fn is_labeled_queued_preview(message: &str) -> bool {
    message.starts_with(&format!("{HEARTBEAT_PROMPT_PREVIEW_LABEL}: "))
        || message.starts_with(&format!("{GOAL_CONTEXT_PREVIEW_LABEL}: "))
        || message.starts_with(&format!("{AGENT_MESSAGE_RECEIVED_PREVIEW_LABEL}: "))
        || message.starts_with(&format!("{ASYNC_BASH_COMPLETION_PREVIEW_LABEL}: "))
}

/// `"Steering" | "Follow-up"`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueueLabel {
    Steering,
    FollowUp,
}

impl QueueLabel {
    pub fn as_str(&self) -> &'static str {
        match self {
            QueueLabel::Steering => "Steering",
            QueueLabel::FollowUp => "Follow-up",
        }
    }
}

/// Port of `formatQueuedMessagePreview`.
pub fn format_queued_message_preview(message: &str, label: QueueLabel) -> String {
    if is_labeled_queued_preview(message) {
        message.to_string()
    } else {
        format!("{}: {message}", label.as_str())
    }
}

/// Port of `styleQueuedMessagePreview`.
///
/// `isRecognizedSlashCommand`, `isLeadingSlashCommand`, `styleArgumentTokens`
/// and `styleSlashCommandText` live in other slices; the callback shape is kept
/// so the styling split stays identical.
pub fn style_queued_message_preview(
    message: &str,
    label: QueueLabel,
    is_recognized_slash_command: &dyn Fn(&str) -> bool,
    is_leading_slash_command: &dyn Fn(&str, &dyn Fn(&str) -> bool) -> bool,
    style_argument_tokens: &dyn Fn(&str, &dyn Fn(&str) -> String, bool) -> String,
    style_slash_command_text: &dyn Fn(&str, &dyn Fn(&str, bool) -> String) -> String,
) -> String {
    let preview = format_queued_message_preview(message, label);
    let style_dim = |segment: &str| theme().fg("dim", segment);
    if !is_leading_slash_command(message, is_recognized_slash_command) {
        return style_argument_tokens(&preview, &style_dim, false);
    }
    let prefix = &preview[..preview.len() - message.len()];
    let styled = style_slash_command_text(message, &|rest, include_bare_separator| {
        style_argument_tokens(rest, &style_dim, include_bare_separator)
    });
    format!("{}{styled}", theme().fg("dim", prefix))
}

/// Port of `isExpandable`.
pub trait Expandable {
    fn set_expanded(&mut self, expanded: bool);
}

/// Port of `ExpandableText`.
pub struct ExpandableText {
    text: Text,
    get_collapsed_text: Box<dyn Fn() -> String + Send + Sync>,
    get_expanded_text: Box<dyn Fn() -> String + Send + Sync>,
}

impl ExpandableText {
    pub fn new(
        get_collapsed_text: Box<dyn Fn() -> String + Send + Sync>,
        get_expanded_text: Box<dyn Fn() -> String + Send + Sync>,
        expanded: bool,
        padding_x: usize,
        padding_y: usize,
    ) -> Self {
        let initial = if expanded { get_expanded_text() } else { get_collapsed_text() };
        Self {
            text: Text::new(initial, padding_x, padding_y),
            get_collapsed_text,
            get_expanded_text,
        }
    }
}

impl Expandable for ExpandableText {
    fn set_expanded(&mut self, expanded: bool) {
        let value = if expanded { (self.get_expanded_text)() } else { (self.get_collapsed_text)() };
        self.text.set_text(value);
    }
}

/// Port of `formatSplashCwd`.
pub fn format_splash_cwd(cwd: &str) -> String {
    let normalized = cwd.replace('\\', "/");
    let home = home_dir().replace('\\', "/");
    if !home.is_empty() && normalized == home {
        return "~".to_string();
    }
    if !home.is_empty() && normalized.starts_with(&format!("{home}/")) {
        return format!("~{}", &normalized[home.len()..]);
    }
    normalized
}

fn home_dir() -> String {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_default()
}

/// Port of `mergeSubagentSnapshot`.
pub fn merge_subagent_snapshot(
    previous: &AgentConnectionRlmChildAgentSnapshot,
    incoming: &AgentConnectionRlmChildAgentSnapshot,
) -> AgentConnectionRlmChildAgentSnapshot {
    let active = incoming.status == "running" || incoming.status == "queued";
    AgentConnectionRlmChildAgentSnapshot {
        parent_id: incoming.parent_id.clone().or_else(|| previous.parent_id.clone()),
        // Active updates may omit a previously known daemon session id, but a
        // terminal update without one means the child is no longer resident.
        active_session_id: if active {
            incoming.active_session_id.clone().or_else(|| previous.active_session_id.clone())
        } else {
            incoming.active_session_id.clone()
        },
        // A completed retained child can become active again when it receives a
        // follow-up. Its RLM run status stays terminal, so activity must remain an
        // independent projection of the live session state.
        activity: if active { incoming.activity.clone().or_else(|| previous.activity.clone()) } else { incoming.activity.clone() },
        ..incoming.clone()
    }
}

/// Port of `truncatePathMiddle`.
pub fn truncate_path_middle(value: &str, width: f64) -> String {
    if visible_width(value) <= width {
        return value.to_string();
    }
    if width <= 1.0 {
        return truncate_to_width(value, width, "", true);
    }

    let ellipsis = "\u{2026}";
    let normalized = value.replace('\\', "/");
    let prefix = if normalized.starts_with("~/") {
        "~/"
    } else if normalized.starts_with('/') {
        "/"
    } else {
        ""
    };
    let body = if prefix.is_empty() { normalized.as_str() } else { &normalized[prefix.len()..] };
    let mut parts: Vec<&str> = body.split('/').filter(|part| !part.is_empty()).collect();
    let last = parts.pop().unwrap_or("");
    let previous = parts.pop();
    let suffix = match previous {
        Some(previous) => format!("{previous}/{last}"),
        None => last.to_string(),
    };
    let candidate = format!("{prefix}{ellipsis}/{suffix}");
    if visible_width(&candidate) <= width {
        return candidate;
    }

    truncate_to_width(&candidate, width, "", true)
}

/// `BrandSplashMetadataLine`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrandSplashMetadataLine {
    pub label: String,
    pub value: String,
}

/// `BrandSplashHeaderOptions`
pub struct BrandSplashHeaderOptions {
    pub logo: Option<String>,
    pub top_padding: bool,
    pub get_rows: Option<Box<dyn Fn() -> f64 + Send + Sync>>,
    pub get_extra_metadata: Option<Box<dyn Fn() -> Vec<BrandSplashMetadataLine> + Send + Sync>>,
    pub get_hide_start_hint: Option<Box<dyn Fn() -> bool + Send + Sync>>,
    pub get_start_hint: Option<Box<dyn Fn() -> String + Send + Sync>>,
}

impl Default for BrandSplashHeaderOptions {
    fn default() -> Self {
        Self {
            logo: None,
            top_padding: false,
            get_rows: None,
            get_extra_metadata: None,
            get_hide_start_hint: None,
            get_start_hint: None,
        }
    }
}

/// Port of `BrandSplashHeader`.
pub struct BrandSplashHeader {
    version: String,
    get_model_id: Box<dyn Fn() -> Option<String> + Send + Sync>,
    get_cwd: Box<dyn Fn() -> String + Send + Sync>,
    verbose_instructions: Option<String>,
    options: BrandSplashHeaderOptions,
}

impl BrandSplashHeader {
    const GUTTER: usize = 4;
    const LABEL_WIDTH: usize = 9;

    pub fn new(
        version: String,
        get_model_id: Box<dyn Fn() -> Option<String> + Send + Sync>,
        get_cwd: Box<dyn Fn() -> String + Send + Sync>,
        verbose_instructions: Option<String>,
        options: BrandSplashHeaderOptions,
    ) -> Self {
        Self { version, get_model_id, get_cwd, verbose_instructions, options }
    }

    pub fn invalidate(&self) {
        // Render output is derived from current theme/session state.
    }

    /// Port of `render`.
    pub fn render(&self, width: f64, max_rows: Option<f64>) -> Vec<String> {
        let max_rows = max_rows.unwrap_or(f64::INFINITY);
        let safe_width = width.max(1.0).floor();
        let padding_x = if safe_width >= 3.0 { 1usize } else { 0 };
        let content_width = (safe_width - (padding_x * 2) as f64).max(1.0).floor();
        let terminal_rows = self
            .options
            .get_rows
            .as_ref()
            .map(|get_rows| get_rows())
            .unwrap_or_else(|| process_stdout_rows().unwrap_or(40.0));
        let available_rows = 0.0f64.max(max_rows.min(terminal_rows - 8.0)).floor();
        if available_rows == 0.0 {
            return Vec::new();
        }
        let top_padding = self.options.top_padding && terminal_rows >= 24.0 && available_rows > 1.0;
        let body_rows = available_rows - if top_padding { 1.0 } else { 0.0 };
        let logo_max_rows = 12.0f64.min((terminal_rows - 16.0).max(0.0)).min(body_rows);
        let logo_max_width =
            26.0f64.min(content_width - (Self::GUTTER as f64) - (Self::LABEL_WIDTH as f64) - 8.0);
        // Decorative art yields before metadata or the caller's search/session rows.
        let logo_raw: Vec<String> = match &self.options.logo {
            Some(logo) => logo.split('\n').map(|line| line.to_string()).collect(),
            None => {
                if logo_max_rows >= 8.0 && logo_max_width >= 16.0 {
                    super::super::themes::optimus_logo::get_optimus_logo(logo_max_width, logo_max_rows)
                } else {
                    Vec::new()
                }
            }
        };
        let logo_canvas_width = logo_raw.iter().map(|line| visible_width(line)).fold(0.0f64, f64::max);
        let logo_gutter = if logo_raw.is_empty() { 0.0 } else { Self::GUTTER as f64 };
        let meta_width = content_width - logo_canvas_width - logo_gutter;
        let show_meta = meta_width >= (Self::LABEL_WIDTH as f64) + 8.0;
        let value_width = (meta_width - Self::LABEL_WIDTH as f64).max(1.0).floor();
        let labelled = |label: &str, value: &str| -> String {
            let display_value = if label == "cwd" {
                truncate_path_middle(value, value_width)
            } else {
                truncate_to_width(value, value_width, "", true)
            };
            let padded = format!("{label:<width$}", width = Self::LABEL_WIDTH);
            format!("{}{}", theme().fg("dim", &padded), theme().fg("muted", &display_value))
        };
        let extra_metadata: Vec<BrandSplashMetadataLine> = self
            .options
            .get_extra_metadata
            .as_ref()
            .map(|get_extra_metadata| get_extra_metadata())
            .unwrap_or_default();
        let hide_start_hint = self
            .options
            .get_hide_start_hint
            .as_ref()
            .map(|get_hide_start_hint| get_hide_start_hint())
            .unwrap_or(false);
        let start_hint = self
            .options
            .get_start_hint
            .as_ref()
            .map(|get_start_hint| get_start_hint())
            .unwrap_or_else(|| "type to search sessions".to_string());
        let mut meta_lines: Vec<String> = if show_meta {
            let mut lines: Vec<String> = Vec::new();
            if self.options.logo.is_none() {
                lines.push(theme().bold(&colorize_optimus_logo("OPTIMUS")));
                lines.push(String::new());
            }
            lines.push(labelled("version", &format!("v{}", self.version)));
            lines.push(labelled("model", &self.get_model_id().unwrap_or_else(|| "\u{2014}".to_string())));
            lines.push(labelled("cwd", &format_splash_cwd(&self.get_cwd())));
            lines.extend(extra_metadata.iter().map(|line| labelled(&line.label, &line.value)));
            if !hide_start_hint {
                lines.push(String::new());
                lines.push(theme().fg("dim", &start_hint));
            }
            lines
        } else {
            Vec::new()
        };
        if meta_lines.len() as f64 > body_rows {
            let mut index = meta_lines.len() as i64 - 1;
            while index >= 0 {
                if meta_lines[index as usize].is_empty() {
                    meta_lines.remove(index as usize);
                }
                index -= 1;
            }
            if meta_lines.len() as f64 > body_rows && !hide_start_hint {
                meta_lines.pop();
            }
            meta_lines.truncate(body_rows.max(0.0) as usize);
        }
        let row_count = logo_raw.len().max(meta_lines.len());
        let meta_start = (row_count.saturating_sub(meta_lines.len())) / 2;
        let mut lines: Vec<String> = if top_padding { vec![String::new()] } else { Vec::new() };
        for index in 0..row_count {
            let line = logo_raw.get(index).cloned().unwrap_or_default();
            let colored = if self.options.logo.is_none() {
                colorize_optimus_logo(&line)
            } else {
                theme().fg("text", &line)
            };
            let meta = if index >= meta_start && index < meta_start + meta_lines.len() {
                meta_lines[index - meta_start].clone()
            } else {
                String::new()
            };
            let padding = if show_meta {
                " ".repeat(
                    (logo_canvas_width - visible_width(&line) + logo_gutter).max(0.0) as usize,
                )
            } else {
                String::new()
            };
            let content = truncate_to_width(&format!("{colored}{padding}{meta}"), content_width, "", false);
            lines.push(format!(
                "{}{content}{}",
                " ".repeat(padding_x),
                " ".repeat((safe_width - padding_x as f64 - visible_width(&content)).max(0.0) as usize)
            ));
        }
        if !show_meta && self.options.logo.is_none() {
            lines.push(format!(
                "{}{}",
                " ".repeat(padding_x),
                truncate_to_width(&theme().bold(&colorize_optimus_logo("OPTIMUS")), content_width, "", true)
            ));
        }

        if let Some(verbose_instructions) = &self.verbose_instructions {
            lines.push(" ".repeat(safe_width as usize));
            for instruction in verbose_instructions.split('\n') {
                let content = truncate_to_width(instruction, content_width, "", true);
                lines.push(format!(
                    "{}{content}{}",
                    " ".repeat(padding_x),
                    " ".repeat((safe_width - padding_x as f64 - visible_width(&content)).max(0.0) as usize)
                ));
            }
        }

        lines
    }
}

/// Stand-in for `process.stdout.rows`.
fn process_stdout_rows() -> Option<f64> {
    std::env::var("LINES").ok().and_then(|value| value.parse::<f64>().ok())
}

/// Stand-in for `colorizeOptimusLogo` (themes/optimus-logo.ts, other slice).
fn colorize_optimus_logo(line: &str) -> String {
    theme().fg("accent", line)
}

/// `StartupPromptBarrierOutcome`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartupPromptBarrierOutcome {
    Admitted,
    Retained,
    LifecycleCancelled,
}

impl StartupPromptBarrierOutcome {
    pub fn as_str(&self) -> &'static str {
        match self {
            StartupPromptBarrierOutcome::Admitted => "admitted",
            StartupPromptBarrierOutcome::Retained => "retained",
            StartupPromptBarrierOutcome::LifecycleCancelled => "lifecycle-cancelled",
        }
    }
}

/// `GoalAnnouncementSnapshot`
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GoalAnnouncementSnapshot {
    pub goal_id: Option<String>,
    pub status: String,
    pub objective: Option<String>,
    pub last_reason: Option<String>,
    pub last_error: Option<String>,
}

/// `ModelFallbackWarningAction`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelFallbackWarningAction {
    Show,
    Suppress,
}

/// `OnboardingSplashHandle`
pub struct OnboardingSplashHandle {
    pub disposed: bool,
}

/// `THINKING_LEVEL_DESCRIPTIONS`
pub fn thinking_level_descriptions() -> HashMap<ThinkingLevel, &'static str> {
    HashMap::from([
        (ThinkingLevel::Off, "No reasoning"),
        (ThinkingLevel::Minimal, "Very brief reasoning (~1k tokens)"),
        (ThinkingLevel::Low, "Light reasoning (~2k tokens)"),
        (ThinkingLevel::Medium, "Moderate reasoning (~8k tokens)"),
        (ThinkingLevel::High, "Deep reasoning (~16k tokens)"),
        (ThinkingLevel::Xhigh, "Very deep reasoning (~32k tokens)"),
        (ThinkingLevel::Max, "Maximum reasoning"),
    ])
}

/// `HEARTBEAT_ARGUMENT_COMPLETIONS`
pub fn heartbeat_argument_completions() -> Vec<(String, String)> {
    vec![
        ("start <schedule> [prompt]".to_string(), "Create or update a heartbeat".to_string()),
        ("stop".to_string(), "Stop and remove the heartbeat".to_string()),
        ("status".to_string(), "Show heartbeat status".to_string()),
        ("help".to_string(), "Show heartbeat help".to_string()),
    ]
}

/// `DEAD_TERMINAL_ERROR_CODES`
pub fn dead_terminal_error_codes() -> HashSet<&'static str> {
    HashSet::from(["EIO", "EPIPE", "ENOTCONN"])
}

/// `MAX_PASTED_IMAGE_BYTES`
pub const MAX_PASTED_IMAGE_BYTES: f64 = 64.0 * 1024.0 * 1024.0;
/// `INITIAL_TRANSCRIPT_RENDER_MESSAGE_LIMIT`
pub const INITIAL_TRANSCRIPT_RENDER_MESSAGE_LIMIT: usize = 400;

/// Port of `initialRenderMessages`.
pub fn initial_render_messages(messages: Vec<AgentMessage>) -> Vec<AgentMessage> {
    if messages.len() <= INITIAL_TRANSCRIPT_RENDER_MESSAGE_LIMIT {
        return messages;
    }
    let mut tool_call_messages: HashMap<String, (usize, AgentMessage)> = HashMap::new();
    for (index, message) in messages.iter().enumerate() {
        let AgentMessage::Assistant(assistant) = message else {
            continue;
        };
        for content in &assistant.content {
            if let pi_ai::types::AssistantContent::ToolCall(tool_call) = content {
                tool_call_messages.insert(tool_call.id.clone(), (index, message.clone()));
            }
        }
    }

    let initial_start_index = messages.len() - INITIAL_TRANSCRIPT_RENDER_MESSAGE_LIMIT;
    for start_index in initial_start_index..messages.len() {
        let visible_messages = &messages[start_index..];
        let mut visible_tool_call_ids: HashSet<String> = HashSet::new();
        for message in visible_messages {
            let AgentMessage::Assistant(assistant) = message else {
                continue;
            };
            for content in &assistant.content {
                if let pi_ai::types::AssistantContent::ToolCall(tool_call) = content {
                    visible_tool_call_ids.insert(tool_call.id.clone());
                }
            }
        }

        let mut required_tool_call_ids_by_message: HashMap<usize, (AgentMessage, HashSet<String>)> = HashMap::new();
        for message in visible_messages {
            let AgentMessage::ToolResult(tool_result) = message else {
                continue;
            };
            if visible_tool_call_ids.contains(&tool_result.tool_call_id) {
                continue;
            }
            let Some((tool_call_index, tool_call_message)) = tool_call_messages.get(&tool_result.tool_call_id) else {
                continue;
            };
            if *tool_call_index >= start_index {
                continue;
            }
            let entry = required_tool_call_ids_by_message
                .entry(*tool_call_index)
                .or_insert_with(|| (tool_call_message.clone(), HashSet::new()));
            entry.1.insert(tool_result.tool_call_id.clone());
        }

        if visible_messages.len() + required_tool_call_ids_by_message.len() > INITIAL_TRANSCRIPT_RENDER_MESSAGE_LIMIT
        {
            continue;
        }

        let mut required_entries: Vec<(usize, AgentMessage, HashSet<String>)> = required_tool_call_ids_by_message
            .into_iter()
            .map(|(index, (message, tool_call_ids))| (index, message, tool_call_ids))
            .collect();
        required_entries.sort_by_key(|(index, _, _)| *index);
        let required_tool_call_messages: Vec<AgentMessage> = required_entries
            .into_iter()
            .map(|(_, message, tool_call_ids)| {
                let AgentMessage::Assistant(mut assistant) = message else {
                    return message;
                };
                assistant.content.retain(|content| match content {
                    pi_ai::types::AssistantContent::ToolCall(tool_call) => tool_call_ids.contains(&tool_call.id),
                    _ => true,
                });
                AgentMessage::Assistant(assistant)
            })
            .collect();
        let mut combined = required_tool_call_messages;
        combined.extend(visible_messages.iter().cloned());
        return omit_orphan_tool_results(combined);
    }

    Vec::new()
}

/// Port of `omitOrphanToolResults`.
pub fn omit_orphan_tool_results(messages: Vec<AgentMessage>) -> Vec<AgentMessage> {
    let mut rendered_tool_call_ids: HashSet<String> = HashSet::new();
    let mut renderable_messages: Vec<AgentMessage> = Vec::new();
    for message in messages {
        match &message {
            AgentMessage::Assistant(assistant) => {
                for content in &assistant.content {
                    if let pi_ai::types::AssistantContent::ToolCall(tool_call) = content {
                        rendered_tool_call_ids.insert(tool_call.id.clone());
                    }
                }
                renderable_messages.push(message);
            }
            AgentMessage::ToolResult(tool_result) => {
                if rendered_tool_call_ids.contains(&tool_result.tool_call_id) {
                    renderable_messages.push(message);
                }
            }
            _ => renderable_messages.push(message),
        }
    }
    renderable_messages
}

/// Port of `isDeadTerminalError`.
pub fn is_dead_terminal_error(code: Option<&str>) -> bool {
    match code {
        Some(code) => dead_terminal_error_codes().contains(code),
        None => false,
    }
}

/// Port of `getPayloadString`.
pub fn get_payload_string(payload: &serde_json::Value, key: &str) -> Option<String> {
    payload.get(key).and_then(|value| value.as_str()).map(|value| value.to_string())
}

/// Port of `getPayloadNumber`.
pub fn get_payload_number(payload: &serde_json::Value, key: &str) -> Option<f64> {
    payload.get(key).and_then(|value| value.as_f64()).filter(|value| value.is_finite())
}

/// Port of `getPayloadBoolean`.
pub fn get_payload_boolean(payload: &serde_json::Value, key: &str) -> Option<bool> {
    payload.get(key).and_then(|value| value.as_bool())
}

/// Port of `getPayloadStringArray`.
pub fn get_payload_string_array(payload: &serde_json::Value, key: &str) -> Option<Vec<String>> {
    let value = payload.get(key)?;
    if value.is_null() {
        return None;
    }
    let array = value.as_array()?;
    let mut out = Vec::new();
    for item in array {
        out.push(item.as_str()?.to_string());
    }
    Some(out)
}

/// `"info" | "warning" | "error"`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotifyType {
    Info,
    Warning,
    Error,
}

impl NotifyType {
    pub fn as_str(&self) -> &'static str {
        match self {
            NotifyType::Info => "info",
            NotifyType::Warning => "warning",
            NotifyType::Error => "error",
        }
    }
}

/// Port of `getPayloadNotifyType`.
pub fn get_payload_notify_type(payload: &serde_json::Value, key: &str) -> Option<NotifyType> {
    match payload.get(key).and_then(|value| value.as_str()) {
        Some("info") => Some(NotifyType::Info),
        Some("warning") => Some(NotifyType::Warning),
        Some("error") => Some(NotifyType::Error),
        _ => None,
    }
}

/// `"aboveEditor" | "belowEditor"`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WidgetPlacement {
    AboveEditor,
    BelowEditor,
}

impl WidgetPlacement {
    pub fn as_str(&self) -> &'static str {
        match self {
            WidgetPlacement::AboveEditor => "aboveEditor",
            WidgetPlacement::BelowEditor => "belowEditor",
        }
    }
}

/// Port of `getPayloadWidgetPlacement`.
pub fn get_payload_widget_placement(payload: &serde_json::Value, key: &str) -> Option<WidgetPlacement> {
    match payload.get(key).and_then(|value| value.as_str()) {
        Some("aboveEditor") => Some(WidgetPlacement::AboveEditor),
        Some("belowEditor") => Some(WidgetPlacement::BelowEditor),
        _ => None,
    }
}

/// `LoaderIndicatorOptions`
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LoaderIndicatorOptions {
    pub frames: Option<Vec<String>>,
    pub interval_ms: Option<f64>,
}

/// Port of `getPayloadWorkingIndicatorOptions`.
pub fn get_payload_working_indicator_options(
    payload: &serde_json::Value,
    key: &str,
) -> Option<LoaderIndicatorOptions> {
    let value = payload.get(key)?;
    if !value.is_object() {
        return None;
    }
    let frames = get_payload_string_array(value, "frames");
    let interval_ms = get_payload_number(value, "intervalMs");
    Some(LoaderIndicatorOptions { frames, interval_ms })
}

/// Port of `updateArgsIncludeSelf`.
pub fn update_args_include_self(args: &[String]) -> bool {
    let mut self_flag = false;
    let mut extensions_only_flag = false;
    let mut positional: Option<String> = None;
    let mut index = 0usize;
    while index < args.len() {
        let arg = args[index].as_str();
        if arg == "--self" {
            self_flag = true;
        } else if arg == "--extensions" {
            extensions_only_flag = true;
        } else if arg == "--extension" {
            extensions_only_flag = true;
            index += 1;
        } else if arg == "--daemon-socket" {
            index += 1;
        } else if !arg.is_empty() && !arg.starts_with('-') && positional.is_none() {
            positional = Some(arg.to_string());
        }
        index += 1;
    }
    if self_flag {
        return true;
    }
    if extensions_only_flag {
        return false;
    }
    let Some(positional) = positional else {
        return true;
    };
    let normalized = positional.to_lowercase();
    normalized == "self" || normalized == "pi" || normalized == APP_NAME.to_lowercase()
}

/// Port of `argsIncludeSessionSelection`.
fn args_include_session_selection(args: &[String]) -> bool {
    args.iter().any(|arg| {
        arg == "--resume" || arg == "-r" || arg == "--continue" || arg == "-c" || arg == "--fork"
    })
}

/// Port of `buildUpdateRelaunchArgs`.
pub fn build_update_relaunch_args(args: &[String], session_file: Option<&str>) -> Vec<String> {
    let mut relaunch_args = args.to_vec();
    if let Some(session_file) = session_file {
        if !args_include_session_selection(&relaunch_args) {
            relaunch_args.push("--resume".to_string());
            relaunch_args.push(session_file.to_string());
        }
    }
    relaunch_args
}

/// Port of `execveFailureThrows`.
pub fn execve_failure_throws(node_version: &str) -> bool {
    // Before Node 26.1, a failed execve syscall aborts the process instead of throwing for the fallback below.
    let Some(captures) = regex_match_node_version(node_version) else {
        return false;
    };
    let major = captures.0;
    let minor = captures.1;
    major > 26 || (major == 26 && minor >= 1)
}

/// `/^(\d+)\.(\d+)\./`
fn regex_match_node_version(node_version: &str) -> Option<(i64, i64)> {
    let mut parts = node_version.split('.');
    let major = parts.next()?.parse::<i64>().ok()?;
    let minor = parts.next()?.parse::<i64>().ok()?;
    // The TypeScript pattern requires the trailing dot.
    if !node_version.contains('.') || node_version.matches('.').count() < 2 {
        return None;
    }
    Some((major, minor))
}

/// `CliSubprocessLaunchSpec`
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CliSubprocessLaunchSpec {
    pub command: String,
    pub args: Vec<String>,
}

/// `UpdateRelaunchExecOptions`
pub struct UpdateRelaunchExecOptions {
    pub platform: String,
    pub node_version: String,
    pub cwd: String,
    pub previous_cwd: String,
    pub environment: HashMap<String, Option<String>>,
    pub chdir: Box<dyn FnMut(&str)>,
    pub execve: Option<Box<dyn FnMut(&str, &[String], &HashMap<String, String>)>>,
}

/// Port of `tryExecUpdateRelaunch`.
pub fn try_exec_update_relaunch(launch: &CliSubprocessLaunchSpec, options: &mut UpdateRelaunchExecOptions) -> bool {
    // Process replacement preserves the shell job and foreground terminal without retaining the old TUI.
    if options.execve.is_none()
        || options.platform == "win32"
        || options.platform == "os400"
        || !execve_failure_throws(&options.node_version)
    {
        return false;
    }
    let environment: HashMap<String, String> = options
        .environment
        .iter()
        .filter_map(|(key, value)| value.as_ref().map(|value| (key.clone(), value.clone())))
        .collect();
    (options.chdir)(&options.cwd);
    let mut argv: Vec<String> = vec![launch.command.clone()];
    argv.extend(launch.args.iter().cloned());
    let execve = options.execve.as_mut().expect("execve checked above");
    execve(&launch.command, &argv, &environment);
    true
}

/// Port of `buildUpdateChildArgs`.
pub fn build_update_child_args(args: &[String], daemon_socket_path: &str) -> Vec<String> {
    if args.iter().any(|arg| arg == "--daemon-socket") {
        args.to_vec()
    } else {
        let mut out = args.to_vec();
        out.push("--daemon-socket".to_string());
        out.push(daemon_socket_path.to_string());
        out
    }
}

/// Port of `resolveInteractiveUpdateDaemonSocketPath`.
pub fn resolve_interactive_update_daemon_socket_path(args: &[String], active_daemon_socket_path: &str) -> String {
    match args.iter().position(|arg| arg == "--daemon-socket") {
        Some(index) => args.get(index + 1).cloned().unwrap_or_else(|| active_daemon_socket_path.to_string()),
        None => active_daemon_socket_path.to_string(),
    }
}

/// `InteractiveInitialPrompt`
#[derive(Debug, Clone, Default)]
pub struct InteractiveInitialPrompt {
    pub text: String,
    pub images: Option<Vec<ImageContent>>,
}

/// `InteractiveModeOptions`
pub struct InteractiveModeOptions {
    /// Providers that were migrated to auth.json (shows warning)
    pub migrated_providers: Option<Vec<String>>,
    /// Warning message if session model couldn't be restored
    pub model_fallback_message: Option<String>,
    /// One-off warning shown on startup.
    pub startup_notice: Option<String>,
    /// Initial message to send on startup (can include @file content)
    pub initial_message: Option<String>,
    /// Images to attach to the initial message
    pub initial_images: Option<Vec<ImageContent>>,
    /// Additional text-only messages to send after the initial message.
    pub initial_messages: Option<Vec<String>>,
    /// Additional image-bearing prompts to send after the initial messages.
    pub initial_prompts: Option<Vec<InteractiveInitialPrompt>>,
    /// Force verbose startup (overrides quietStartup setting)
    pub verbose: bool,
    /// Agent execution boundary. InteractiveMode never talks directly to AgentSession for core execution.
    pub agent_connection: AgentConnection,
    /// Exact daemon socket to preserve across an interactive self-update restart.
    pub daemon_socket_path: Option<String>,
    /// Local-only host for in-process extension binding and callback-bearing session operations.
    pub local_session_host: Option<Arc<dyn InteractiveModeLocalSessionHost>>,
    /// Bind extension handlers in the local session host. Disabled for daemon/gateway-backed clients.
    pub bind_local_session_extensions: bool,
    /// UI-local services used for settings, auth, resources, and rendering.
    pub ui_services: Option<InteractiveModeUiServices>,
    /// Extra cleanup for externally-owned UI service hosts.
    pub on_shutdown: Option<Box<dyn FnMut() + Send + Sync>>,
    /// Allow returning from a full session to the agents view without stopping the daemon-owned agent.
    pub return_to_agents_view: bool,
    /// Enter fullscreen regardless of the persisted fullscreen preference.
    pub force_fullscreen: bool,
    /// The agents view already surfaced global startup notices.
    pub agents_view_owns_startup_notices: bool,
    /// Persisted RLM depth supplied by the daemon SessionSummary.
    pub session_depth: Option<f64>,
    /// Whether the unified daemon/catalog projection had any direct children.
    pub session_has_children: bool,
    /// Client-owned stash store shared across chat views in this TUI process.
    pub prompt_stash_store: Option<Arc<ClientPromptStashStore>>,
    /// Initial stable session id used to scope prompt stash state.
    pub prompt_stash_session_id: Option<String>,
}

/// `InteractiveModeRunResult`
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InteractiveModeRunResultType {
    AgentsView,
    ScopedAgentsView,
}

impl InteractiveModeRunResultType {
    pub fn as_str(&self) -> &'static str {
        match self {
            InteractiveModeRunResultType::AgentsView => "agents_view",
            InteractiveModeRunResultType::ScopedAgentsView => "scoped_agents_view",
        }
    }
}

/// `Pick<AgentConnectionState, "activeSessionId" | "sessionFile" | "sessionId" | "sessionName" | "cwd">`
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InteractiveModeRunResultSource {
    pub active_session_id: Option<String>,
    pub session_file: Option<String>,
    pub session_id: String,
    pub session_name: Option<String>,
    pub cwd: String,
}

/// `InteractiveModeRunResult`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InteractiveModeRunResult {
    pub type_: InteractiveModeRunResultType,
    pub source: InteractiveModeRunResultSource,
}

/// Port of `formatAgentDepthLabel`.
pub fn format_agent_depth_label(depth: Option<f64>, has_children: bool) -> Option<String> {
    match depth {
        None => None,
        Some(depth) if depth == 0.0 && !has_children => None,
        Some(depth) => Some(format!("depth {}", js_number_to_string(depth))),
    }
}

fn js_number_to_string(value: f64) -> String {
    if value.fract() == 0.0 && value.abs() < 1e21 {
        format!("{}", value as i64)
    } else {
        format!("{value}")
    }
}

/// `LoadedAgentConnectionHistory`
#[derive(Debug, Clone, Default)]
pub struct LoadedAgentConnectionHistory {
    pub window: AgentConnectionHistoryWindow,
    pub messages: Vec<AgentMessage>,
}

/// Port of `mergeOlderAgentConnectionHistory`.
pub fn merge_older_agent_connection_history(
    current: &LoadedAgentConnectionHistory,
    range: &AgentConnectionHistoryRange,
) -> Result<LoadedAgentConnectionHistory, String> {
    let range_window = &range.window;
    if current.window.version != 1.0
        || current.window.order != "chronological"
        || current.window.representation.is_empty()
        || current.window.entry_ids.len() != current.messages.len()
        || current.window.start_index < 0.0
        || current.window.start_index + current.messages.len() as f64 != current.window.total_message_count
        || current.window.has_older != (current.window.start_index > 0.0)
        || range_window.version != 1.0
        || range_window.order != "chronological"
        || range_window.generation != current.window.generation
        || range_window.representation != current.window.representation
        || range_window.tip_entry_id != current.window.tip_entry_id
        || range_window.total_message_count != current.window.total_message_count
        || range_window.start_index < 0.0
        || range_window.start_index + range.messages.len() as f64 != current.window.start_index
        || range_window.entry_ids.len() != range.messages.len()
        || range_window.has_older != (range_window.start_index > 0.0)
    {
        return Err("Older history range does not continue the pinned snapshot".to_string());
    }
    let existing_ids: HashSet<&String> = current.window.entry_ids.iter().collect();
    let range_ids: HashSet<&String> = range_window.entry_ids.iter().collect();
    if existing_ids.len() != current.window.entry_ids.len()
        || range_ids.len() != range_window.entry_ids.len()
        || range_window.entry_ids.iter().any(|entry_id| existing_ids.contains(entry_id))
    {
        return Err("Older history range overlaps already loaded messages".to_string());
    }
    let mut entry_ids = range_window.entry_ids.clone();
    entry_ids.extend(current.window.entry_ids.iter().cloned());
    let mut messages = range.messages.clone();
    messages.extend(current.messages.iter().cloned());
    Ok(LoadedAgentConnectionHistory {
        window: AgentConnectionHistoryWindow {
            entry_ids,
            ..range_window.clone()
        },
        messages,
    })
}


// ---------------------------------------------------------------------------
// InteractiveMode
// ---------------------------------------------------------------------------

/// `SessionSummary` (modes/daemon/daemon-session-list.ts, other slice).
pub use crate::modes::daemon::daemon_session_list::SessionSummary;
/// `SessionSummary` in the roster projection.
pub use crate::modes::daemon::agent_roster::RosterSessionSummary;

/// `SubagentSummaryCounts`
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SubagentSummaryCounts {
    pub total: usize,
    pub running: usize,
    pub idle: usize,
    pub inactive: usize,
}

/// Stand-in for `SubagentSummaryLine` (components/subagent-summary-line.ts).
pub struct SubagentSummaryLine {
    pub get_location_label: Box<dyn Fn() -> Option<String> + Send + Sync>,
    pub get_context_label: Box<dyn Fn() -> Option<String> + Send + Sync>,
    pub get_override_label: Box<dyn Fn() -> Option<String> + Send + Sync>,
    pub focused: bool,
    counts: SubagentSummaryCounts,
    openable: bool,
}

impl SubagentSummaryLine {
    pub fn new(
        get_location_label: Box<dyn Fn() -> Option<String> + Send + Sync>,
        get_context_label: Box<dyn Fn() -> Option<String> + Send + Sync>,
        get_override_label: Box<dyn Fn() -> Option<String> + Send + Sync>,
    ) -> Self {
        Self {
            get_location_label,
            get_context_label,
            get_override_label,
            focused: false,
            counts: SubagentSummaryCounts::default(),
            openable: false,
        }
    }

    pub fn set_subagent_counts(&mut self, counts: SubagentSummaryCounts) {
        self.counts = counts;
    }

    pub fn set_openable(&mut self, openable: bool) {
        self.openable = openable;
    }

    pub fn is_selectable(&self) -> bool {
        self.counts.total > 0 && self.openable
    }
}

/// Port of `classifySubagentSnapshotStatus`.
pub fn classify_subagent_snapshot_status(
    child: &AgentConnectionRlmChildAgentSnapshot,
) -> crate::modes::daemon::agent_roster::AgentRosterStatus {
    // Activity implies a live session; the in-process connection never stamps activeSessionId.
    let resident = child.active_session_id.is_some() || child.activity.is_some();
    let busy = child.status == "running" || child.status == "queued" || child.activity.is_some();
    crate::modes::daemon::agent_roster::classify_agent_status(
        crate::modes::daemon::agent_roster::AgentStatusInput {
            resident,
            queued_child: !resident && busy,
            busy,
        },
    )
}

/// Port of `countDirectSubagentStatuses`.
pub fn count_direct_subagent_statuses(
    children: &[AgentConnectionRlmChildAgentSnapshot],
    parent_id: Option<&str>,
) -> SubagentSummaryCounts {
    let mut counts = SubagentSummaryCounts::default();
    for child in children {
        if child.parent_id.as_deref() != parent_id || child.status == "cancelled" {
            continue;
        }
        counts.total += 1;
        match classify_subagent_snapshot_status(child) {
            crate::modes::daemon::agent_roster::AgentRosterStatus::Running => counts.running += 1,
            crate::modes::daemon::agent_roster::AgentRosterStatus::Idle => counts.idle += 1,
            crate::modes::daemon::agent_roster::AgentRosterStatus::Inactive => counts.inactive += 1,
        }
    }
    counts
}

/// Port of `countRosterSubagentStatuses`.
pub fn count_roster_subagent_statuses(
    summaries: &[SessionSummary],
    parent: (Option<&str>, Option<&str>, Option<&str>),
) -> SubagentSummaryCounts {
    let mut counts = SubagentSummaryCounts::default();
    for child in summaries {
        if child.runtime_kind.as_deref() != Some("subagent") || child.lifecycle != "live" {
            continue;
        }
        if !crate::modes::agents_view::agents_view_state::is_direct_agent_child(child, parent.0, parent.1, parent.2) {
            continue;
        }
        counts.total += 1;
        let status = child.roster_status.clone().unwrap_or_else(|| {
            crate::modes::daemon::agent_roster::classify_session_roster_status(
                &crate::modes::daemon::agent_roster::RosterSummaryView {
                    active_session_id: child.active_session_id.clone(),
                    activity: Some(child.activity.clone()),
                    is_session_active: Some(child.is_session_active),
                    ..Default::default()
                },
                child.has_running_rlm_children == Some(true),
            )
        });
        match status {
            crate::modes::daemon::agent_roster::AgentRosterStatus::Running => counts.running += 1,
            crate::modes::daemon::agent_roster::AgentRosterStatus::Idle => counts.idle += 1,
            crate::modes::daemon::agent_roster::AgentRosterStatus::Inactive => counts.inactive += 1,
        }
    }
    counts
}

/// Port of `InteractiveMode`.
pub struct InteractiveMode {
    // Static configuration
    version: String,
    start_hint: &'static str,
    options: InteractiveModeOptions,

    // Local stand-ins for the TUI tree. The real TUI lives in pi-tui; the Rust
    // port keeps the same containers so the wiring and render order match.
    ui: super::interactive_mode_services::Tui,
    header_container: super::interactive_mode_services::Container,
    history_container: super::interactive_mode_services::Container,
    chat_container: super::interactive_mode_services::Container,
    shortcut_guide_container: super::interactive_mode_services::Container,
    pending_messages_container: super::interactive_mode_services::Container,
    status_container: super::interactive_mode_services::Container,
    queued_messages_container: super::interactive_mode_services::Container,
    side_question_container: super::interactive_mode_services::Container,
    feature_hint_container: super::interactive_mode_services::Container,
    widget_container_above: super::interactive_mode_services::Container,
    widget_container_below: super::interactive_mode_services::Container,
    recap_container: super::interactive_mode_services::Container,
    main_container: super::interactive_mode_services::Container,
    main_view_container: super::interactive_mode_services::Container,
    prompt_dock: super::interactive_mode_services::Container,
    footer_slot: super::interactive_mode_services::Container,
    editor_container: super::interactive_mode_services::Container,

    ui_services: InteractiveModeUiServices,
    agent_connection: AgentConnection,
    local_session_host: Option<Arc<dyn InteractiveModeLocalSessionHost>>,
    bind_local_session_extensions: bool,

    prompt_stash_store: Option<Arc<ClientPromptStashStore>>,
    prompt_stash_session_id: Option<String>,
    prompt_stash_state: PromptStashState,
    prompt_stash_handle: Option<super::prompt_stash_state::PromptStashHandle>,
    pending_prompt_stash_releases: Vec<(String, super::prompt_stash_state::PromptStashHandle)>,

    is_initialized: bool,
    fullscreen_enabled: bool,
    hide_thinking_block: bool,
    default_hidden_thinking_label: String,
    hidden_thinking_label: String,

    tool_output_expanded: bool,
    agent_messages_expanded: bool,
    edit_diffs_expanded: bool,

    connection_state: Option<AgentConnectionState>,
    connection_commands: Vec<super::interactive_mode_services::AgentConnectionSlashCommand>,
    connection_model_catalog: Vec<AgentConnectionModel>,
    connection_configured_providers: HashSet<String>,
    connection_models_fetched_at: f64,
    connection_models_refresh_version: u64,
    connection_models_refresh_in_flight: bool,
    heartbeat_catalog: Vec<AgentConnectionHeartbeat>,

    subagent_snapshots: HashMap<String, AgentConnectionRlmChildAgentSnapshot>,
    rlm_node_id: Option<String>,
    roster_bar: Option<Arc<dyn RosterBar>>,
    subagent_summary_line: SubagentSummaryLine,

    queue_selection: QueueSelection,
    is_applying_queue_selection_text: bool,

    pasted_images: HashMap<i64, ImageContent>,
    next_image_marker_id: i64,

    agents_view_request: Option<InteractiveModeRunResultType>,
    shutdown_requested: bool,
    ctrl_c_exit_hint_expires_at: f64,
    escape_repeat_action: Option<&'static str>,
    escape_repeat_expires_at: f64,
    anthropic_subscription_warning_shown: bool,

    last_status_spacer_index: Option<usize>,
    last_status_text_index: Option<usize>,
    last_goal_announcement: Option<GoalAnnouncementSnapshot>,

    feature_hint_deck: FeatureHintDeck,
    current_feature_hint: Option<String>,
    feature_hint_eligible_at: f64,
    feature_hint_run_pending: bool,
    feature_hint_suppressed_by_queue: bool,

    working_visible: bool,
    working_message: Option<String>,
    working_started_at: Option<f64>,
    turn_started_at: Option<f64>,
    working_indicator_options: Option<LoaderIndicatorOptions>,
    pulse_frame: i64,
    working_pulse_active: bool,

    activity_tracker: super::agent_activity::AgentActivityTracker,
    context_usage_token_baseline: f64,
    context_usage_refresh_generation: u64,
    context_usage_last_success_generation: u64,

    session_recap: Option<String>,

    /// Exit hint duration (ms) from `InteractiveMode.EXIT_HINT_DURATION_MS`.
    exit_hint_duration_ms: f64,
    /// Escape repeat window (ms) from `InteractiveMode.ESCAPE_REPEAT_WINDOW_MS`.
    escape_repeat_window_ms: f64,
}

impl InteractiveMode {
    pub const EXIT_HINT_DURATION_MS: f64 = 2000.0;
    pub const ESCAPE_REPEAT_WINDOW_MS: f64 = 500.0;

    /// Port of the `InteractiveMode` constructor (TUI construction only).
    pub fn new(options: InteractiveModeOptions) -> Result<Self, String> {
        let ui_services = match &options.ui_services {
            Some(services) => Some(clone_ui_services(services)),
            None => options
                .local_session_host
                .as_ref()
                .map(|host| host.create_ui_services()),
        };
        let Some(ui_services) = ui_services else {
            return Err("InteractiveMode requires uiServices when no localSessionHost is supplied".to_string());
        };
        let prompt_stash_store = options.prompt_stash_store.clone();
        let prompt_stash_session_id = options.prompt_stash_session_id.clone();
        let prompt_stash_handle = match (&prompt_stash_store, &prompt_stash_session_id) {
            (Some(store), Some(session_id)) => {
                let mut store = store.clone();
                Some(store.for_session(session_id))
            }
            _ => None,
        };
        let prompt_stash_state = match (&prompt_stash_store, prompt_stash_handle) {
            (Some(store), Some(handle)) => store.state(handle).cloned().unwrap_or_default(),
            _ => PromptStashState::default(),
        };
        let bind_local_session_extensions =
            options.bind_local_session_extensions || options.local_session_host.is_some();
        if bind_local_session_extensions && options.local_session_host.is_none() {
            return Err("Local extension binding requires localSessionHost".to_string());
        }

        let mut mode = Self {
            version: VERSION.to_string(),
            start_hint: get_random_start_hint(&default_random),
            options,
            ui: super::interactive_mode_services::Tui::default(),
            header_container: super::interactive_mode_services::Container::new(),
            history_container: super::interactive_mode_services::Container::new(),
            chat_container: super::interactive_mode_services::Container::new(),
            shortcut_guide_container: super::interactive_mode_services::Container::new(),
            pending_messages_container: super::interactive_mode_services::Container::new(),
            status_container: super::interactive_mode_services::Container::new(),
            queued_messages_container: super::interactive_mode_services::Container::new(),
            side_question_container: super::interactive_mode_services::Container::new(),
            feature_hint_container: super::interactive_mode_services::Container::new(),
            widget_container_above: super::interactive_mode_services::Container::new(),
            widget_container_below: super::interactive_mode_services::Container::new(),
            recap_container: super::interactive_mode_services::Container::new(),
            main_container: super::interactive_mode_services::Container::new(),
            main_view_container: super::interactive_mode_services::Container::new(),
            prompt_dock: super::interactive_mode_services::Container::new(),
            footer_slot: super::interactive_mode_services::Container::new(),
            editor_container: super::interactive_mode_services::Container::new(),
            ui_services,
            agent_connection: options_agent_connection(&options),
            local_session_host: options.local_session_host.clone(),
            bind_local_session_extensions,
            prompt_stash_store,
            prompt_stash_session_id,
            prompt_stash_state,
            prompt_stash_handle,
            pending_prompt_stash_releases: Vec::new(),
            is_initialized: false,
            fullscreen_enabled: false,
            hide_thinking_block: false,
            default_hidden_thinking_label: "Thinking...".to_string(),
            hidden_thinking_label: "Thinking...".to_string(),
            tool_output_expanded: false,
            agent_messages_expanded: false,
            edit_diffs_expanded: false,
            connection_state: None,
            connection_commands: Vec::new(),
            connection_model_catalog: Vec::new(),
            connection_configured_providers: HashSet::new(),
            connection_models_fetched_at: 0.0,
            connection_models_refresh_version: 0,
            connection_models_refresh_in_flight: false,
            heartbeat_catalog: Vec::new(),
            subagent_snapshots: HashMap::new(),
            rlm_node_id: None,
            roster_bar: None,
            subagent_summary_line: SubagentSummaryLine::new(Box::new(|| None), Box::new(|| None), Box::new(|| None)),
            queue_selection: QueueSelection::new(),
            is_applying_queue_selection_text: false,
            pasted_images: HashMap::new(),
            next_image_marker_id: 1,
            agents_view_request: None,
            shutdown_requested: false,
            ctrl_c_exit_hint_expires_at: 0.0,
            escape_repeat_action: None,
            escape_repeat_expires_at: 0.0,
            anthropic_subscription_warning_shown: false,
            last_status_spacer_index: None,
            last_status_text_index: None,
            last_goal_announcement: None,
            feature_hint_deck: FeatureHintDeck::default(),
            current_feature_hint: None,
            feature_hint_eligible_at: 0.0,
            feature_hint_run_pending: false,
            feature_hint_suppressed_by_queue: false,
            working_visible: true,
            working_message: None,
            working_started_at: None,
            turn_started_at: None,
            working_indicator_options: None,
            pulse_frame: 0,
            working_pulse_active: false,
            activity_tracker: super::agent_activity::AgentActivityTracker::default(),
            context_usage_token_baseline: 0.0,
            context_usage_refresh_generation: 0,
            context_usage_last_success_generation: 0,
            session_recap: None,
            exit_hint_duration_ms: Self::EXIT_HINT_DURATION_MS,
            escape_repeat_window_ms: Self::ESCAPE_REPEAT_WINDOW_MS,
        };
        mode.hydrate_prompt_stash();
        mode.hide_thinking_block = mode.settings_manager().get_hide_thinking_block();
        Ok(mode)
    }

    /// `this.uiServices`
    fn ui_services(&self) -> &InteractiveModeUiServices {
        &self.ui_services
    }

    /// `private get settingsManager()`
    pub fn settings_manager(&self) -> &Arc<super::interactive_mode_services::SettingsManager> {
        &self.ui_services.settings_manager
    }

    /// `private get modelRegistry()`
    pub fn model_registry(&self) -> &Arc<super::interactive_mode_services::ModelRegistry> {
        &self.ui_services.model_registry
    }

    /// `private getLocalSessionHost()`
    pub fn get_local_session_host(&self) -> Result<&Arc<dyn InteractiveModeLocalSessionHost>, String> {
        self.local_session_host
            .as_ref()
            .ok_or_else(|| "Local session host is not available in connection-backed interactive mode".to_string())
    }

    /// Port of `hydratePromptStash`.
    fn hydrate_prompt_stash(&mut self) {
        let mut stashes: Vec<PromptStash> = Vec::new();
        if let Some(stash) = &self.prompt_stash_state.stash {
            stashes.push(stash.clone());
        }
        if let Some(queued) = &self.prompt_stash_state.queued_stashes {
            stashes.extend(queued.iter().cloned());
        }
        for stash in stashes {
            if let Some(images) = &stash.images {
                for (marker_id, image) in images {
                    self.pasted_images.insert(*marker_id, image.clone());
                    self.next_image_marker_id = self.next_image_marker_id.max(marker_id + 1);
                }
            }
            for marker_id in super::image_markers::image_marker_ids(&stash.text) {
                self.next_image_marker_id = self.next_image_marker_id.max(marker_id as i64 + 1);
            }
        }
    }

    /// Port of `bindPromptStashSession`.
    fn bind_prompt_stash_session(&mut self, session_id: &str) {
        if self.prompt_stash_store.is_none() || self.prompt_stash_session_id.as_deref() == Some(session_id) {
            return;
        }
        self.release_prompt_stash_session();
        self.prompt_stash_session_id = Some(session_id.to_string());
        let Some(store) = self.prompt_stash_store.clone() else {
            return;
        };
        let mut store = store;
        let handle = store.for_session(session_id);
        self.prompt_stash_handle = Some(handle);
        self.prompt_stash_state = store.state(handle).cloned().unwrap_or_default();
        self.hydrate_prompt_stash();
    }

    /// Port of `releasePromptStashSession`.
    fn release_prompt_stash_session(&mut self) {
        if self.pending_submissions_pending() > 0 {
            // Capture the pair: a rebind may repoint the fields before the deferred
            // release fires, and repeated rebinds/teardowns each defer their own pair.
            if let (Some(session_id), Some(handle)) =
                (self.prompt_stash_session_id.clone(), self.prompt_stash_handle)
            {
                if !self
                    .pending_prompt_stash_releases
                    .iter()
                    .any(|(pending_session_id, _)| *pending_session_id == session_id)
                {
                    self.pending_prompt_stash_releases.push((session_id, handle));
                }
            }
            return;
        }
        let pending = std::mem::take(&mut self.pending_prompt_stash_releases);
        let Some(store) = self.prompt_stash_store.clone() else {
            return;
        };
        let mut store = store;
        for (session_id, handle) in pending {
            store.release(&session_id, handle);
        }
        if let (Some(session_id), Some(handle)) = (self.prompt_stash_session_id.clone(), self.prompt_stash_handle) {
            store.release(&session_id, handle);
        }
    }

    /// Port of `completeDeferredPromptStashRelease`.
    fn complete_deferred_prompt_stash_release(&mut self) {
        let pending = std::mem::take(&mut self.pending_prompt_stash_releases);
        if pending.is_empty() {
            return;
        }
        let Some(store) = self.prompt_stash_store.clone() else {
            return;
        };
        let mut store = store;
        for (session_id, handle) in pending {
            store.release(&session_id, handle);
        }
    }

    /// The `inputSubmissionsPending` counter.
    fn pending_submissions_pending(&self) -> usize {
        self.pending_prompt_stash_releases.len()
    }

    /// Port of `getAutocompleteSourceTag`.
    pub fn get_autocomplete_source_tag(
        &self,
        source_info: Option<&super::interactive_mode_services::AgentConnectionSourceInfo>,
    ) -> Option<String> {
        let source_info = source_info?;
        let scope_prefix = match source_info.scope.as_str() {
            "user" => "user",
            "project" => "project",
            _ => "temporary",
        };
        let source = source_info.source.trim();
        if source == "builtin" {
            return Some("builtin".to_string());
        }
        if source == "auto" || source == "local" || source == "cli" {
            return Some(scope_prefix.to_string());
        }
        if let Some(rest) = source.strip_prefix("npm:") {
            return Some(format!("{scope_prefix}:npm:{rest}"));
        }
        if let Some(git_source) = crate::utils::git::parse_git_url(source) {
            let git_ref = git_source
                .git_ref
                .as_ref()
                .map(|git_ref| format!("@{git_ref}"))
                .unwrap_or_default();
            return Some(format!(
                "{scope_prefix}:git:{}/{}{git_ref}",
                git_source.host, git_source.path
            ));
        }
        Some(scope_prefix.to_string())
    }

    /// Port of `getAutocompleteSourceLabel`.
    pub fn get_autocomplete_source_label(
        &self,
        source_info: Option<&super::interactive_mode_services::AgentConnectionSourceInfo>,
    ) -> Option<String> {
        self.get_autocomplete_source_tag(source_info).map(|source_tag| format!("#{source_tag}"))
    }

    /// Port of `getBuiltInCommandConflictDiagnostics`.
    pub fn get_built_in_command_conflict_diagnostics(
        &self,
        commands: &[super::interactive_mode_services::AgentConnectionSlashCommand],
    ) -> Vec<super::interactive_mode_services::AgentConnectionResourceDiagnostic> {
        commands
            .iter()
            .filter(|command| command.source == "extension")
            .filter(|command| {
                crate::core::slash_commands::is_builtin_slash_command_name(
                    command.registered_name.as_deref().unwrap_or(&command.name),
                )
            })
            .map(|command| {
                let registered = command.registered_name.clone().unwrap_or_else(|| command.name.clone());
                let message = if command.name == registered {
                    format!(
                        "Extension command '/{}' conflicts with built-in interactive command. Skipping in autocomplete.",
                        command.name
                    )
                } else {
                    format!(
                        "Extension command '/{registered}' conflicts with built-in interactive command. Available as '/{}'.",
                        command.name
                    )
                };
                super::interactive_mode_services::AgentConnectionResourceDiagnostic {
                    type_: "warning".to_string(),
                    message,
                    path: Some(command.source_info.path.clone()),
                    ..Default::default()
                }
            })
            .collect()
    }

    /// Port of `isRecognizedSlashCommand`.
    pub fn is_recognized_slash_command(&self, name: &str) -> bool {
        crate::core::slash_commands::is_builtin_slash_command_name(name)
            || self.connection_commands.iter().any(|command| command.name == name)
    }

    /// Port of `getModelFallbackWarningAction`.
    pub fn get_model_fallback_warning_action(&self, model_fallback_message: Option<&str>) -> ModelFallbackWarningAction {
        let Some(model_fallback_message) = model_fallback_message else {
            return ModelFallbackWarningAction::Suppress;
        };
        // The no-models warning is a snapshot from whichever process created the
        // session; trust the live connection over it (e.g. credentials only
        // visible to the daemon, or added after the snapshot was taken).
        if crate::core::auth_guidance::is_no_models_available_message(Some(model_fallback_message))
            && self.get_current_model().is_some()
        {
            return ModelFallbackWarningAction::Suppress;
        }
        ModelFallbackWarningAction::Show
    }

    /// Port of `getOnboardingState`.
    pub fn get_onboarding_state(&self) -> super::onboarding::OnboardingStartupState<'_> {
        super::onboarding::OnboardingStartupState {
            settings_manager: self.settings_manager().as_ref(),
            model_registry: self.model_registry().as_ref(),
            model: self.get_current_model(),
        }
    }

    /// Port of `shouldRunOnboarding`.
    pub fn should_run_onboarding(&self) -> bool {
        should_run_onboarding(&self.get_onboarding_state())
    }

    /// Port of `shouldRunPrimeCliOnboardingSplash`.
    pub fn should_run_prime_cli_onboarding_splash(&self) -> bool {
        super::onboarding::should_run_prime_cli_onboarding_splash(&self.get_onboarding_state())
    }

    /// Port of `markOnboardingShown`.
    fn mark_onboarding_shown(&self) {
        if !self.settings_manager().get_onboarding_shown() {
            self.settings_manager().set_onboarding_shown(true);
        }
    }

    /// Port of `formatDisplayPath`.
    pub fn format_display_path(&self, path: &str) -> String {
        let home = home_dir();
        let mut result = path.to_string();
        if !home.is_empty() && result.starts_with(&home) {
            result = format!("~{}", &result[home.len()..]);
        }
        result
    }

    /// Port of `isPackageSource`.
    pub fn is_package_source(
        &self,
        source_info: Option<&super::interactive_mode_services::AgentConnectionSourceInfo>,
    ) -> bool {
        let source = source_info.map(|info| info.source.as_str()).unwrap_or("");
        source.starts_with("npm:") || source.starts_with("git:")
    }

    /// Port of `getShortPath`.
    pub fn get_short_path(
        &self,
        full_path: &str,
        source_info: Option<&super::interactive_mode_services::AgentConnectionSourceInfo>,
    ) -> String {
        let base_dir = source_info.and_then(|info| info.base_dir.clone());
        if let Some(base_dir) = base_dir {
            if self.is_package_source(source_info) {
                let relative_path = relative_path_between(&resolve_path(&base_dir), &resolve_path(full_path));
                if !relative_path.is_empty()
                    && relative_path != "."
                    && !relative_path.starts_with("..")
                    && !relative_path.starts_with(&format!("..{}", std::path::MAIN_SEPARATOR))
                    && !Path::new(&relative_path).is_absolute()
                {
                    return relative_path.replace('\\', "/");
                }
            }
        }

        let source = source_info.map(|info| info.source.as_str()).unwrap_or("");
        if let Some(captures) = regex_match_node_modules(full_path) {
            if source.starts_with("npm:") {
                return captures;
            }
        }

        if let Some(captures) = regex_match_git_path(full_path) {
            if source.starts_with("git:") {
                return captures;
            }
        }

        self.format_display_path(full_path)
    }

    /// `/node_modules\/(@?[^/]+(?:\/[^/]+)?)\/(.*)/`
    /// Port of `getCompactPathLabel`.
    pub fn get_compact_path_label(
        &self,
        resource_path: &str,
        source_info: Option<&super::interactive_mode_services::AgentConnectionSourceInfo>,
    ) -> String {
        let short_path = self.get_short_path(resource_path, source_info);
        let normalized_path = short_path.replace('\\', "/");
        let segments: Vec<&str> = normalized_path
            .split('/')
            .filter(|segment| !segment.is_empty() && *segment != "~")
            .collect();
        if let Some(last) = segments.last() {
            return (*last).to_string();
        }
        short_path
    }

    /// Port of `getCompactDisplayPathSegments`.
    pub fn get_compact_display_path_segments(&self, resource_path: &str) -> Vec<String> {
        self.format_display_path(resource_path)
            .replace('\\', "/")
            .split('/')
            .filter(|segment| !segment.is_empty() && *segment != "~")
            .map(|segment| segment.to_string())
            .collect()
    }

    /// Port of `getCompactPackageSourceLabel`.
    pub fn get_compact_package_source_label(
        &self,
        source_info: Option<&super::interactive_mode_services::AgentConnectionSourceInfo>,
    ) -> String {
        let source = source_info.map(|info| info.source.as_str()).unwrap_or("");
        if let Some(rest) = source.strip_prefix("npm:") {
            return if rest.is_empty() { source.to_string() } else { rest.to_string() };
        }
        if let Some(git_source) = crate::utils::git::parse_git_url(source) {
            if !git_source.path.is_empty() {
                return git_source.path.clone();
            }
        }
        source.to_string()
    }

    /// Port of `getCompactExtensionLabel`.
    pub fn get_compact_extension_label(
        &self,
        resource_path: &str,
        source_info: Option<&super::interactive_mode_services::AgentConnectionSourceInfo>,
    ) -> String {
        if !self.is_package_source(source_info) {
            return self.get_compact_path_label(resource_path, source_info);
        }

        let source_label = self.get_compact_package_source_label(source_info);
        if source_label.is_empty() {
            return self.get_compact_path_label(resource_path, source_info);
        }

        let short_path = self.get_short_path(resource_path, source_info).replace('\\', "/");
        let package_path = short_path.strip_prefix("extensions/").unwrap_or(&short_path);
        let (dir, name) = posix_parse(package_path);

        if name == "index" {
            return if dir.is_empty() || dir == "." {
                source_label
            } else {
                format!("{source_label}:{dir}")
            };
        }

        format!("{source_label}:{package_path}")
    }

    /// Port of `getCompactNonPackageExtensionLabel`.
    pub fn get_compact_non_package_extension_label(
        &self,
        resource_path: &str,
        index: usize,
        all_paths: &[(String, Vec<String>)],
    ) -> String {
        let Some((_, segments)) = all_paths.get(index) else {
            return self.get_compact_path_label(resource_path, None);
        };
        if segments.is_empty() {
            return self.get_compact_path_label(resource_path, None);
        }

        for segment_count in 1..=segments.len() {
            let candidate = segments[segments.len() - segment_count..].join("/");
            let is_unique = all_paths.iter().enumerate().all(|(item_index, (_, item_segments))| {
                if item_index == index {
                    return true;
                }
                item_segments[item_segments.len().saturating_sub(segment_count)..].join("/") != candidate
            });
            if is_unique {
                return candidate;
            }
        }

        segments.join("/")
    }

    /// Port of `getCompactExtensionLabels`.
    pub fn get_compact_extension_labels(
        &self,
        extensions: &[(String, Option<super::interactive_mode_services::AgentConnectionSourceInfo>)],
    ) -> Vec<String> {
        let non_package_extensions: Vec<(String, Vec<String>)> = extensions
            .iter()
            .map(|(path, _)| {
                let mut segments = self.get_compact_display_path_segments(path);
                let last_segment = segments.last().cloned();
                if segments.len() > 1
                    && matches!(last_segment.as_deref(), Some("index.ts") | Some("index.js"))
                {
                    segments.pop();
                }
                (path.clone(), segments)
            })
            .filter(|(_, _)| true)
            .collect();
        let non_package_extensions: Vec<(String, Vec<String>)> = extensions
            .iter()
            .zip(non_package_extensions.iter())
            .filter(|((_, source_info), _)| !self.is_package_source(source_info.as_ref()))
            .map(|(_, (path, segments))| (path.clone(), segments.clone()))
            .collect();

        extensions
            .iter()
            .map(|(path, source_info)| {
                if self.is_package_source(source_info.as_ref()) {
                    return self.get_compact_extension_label(path, source_info.as_ref());
                }
                match non_package_extensions.iter().position(|(item_path, _)| item_path == path) {
                    None => self.get_compact_path_label(path, source_info.as_ref()),
                    Some(non_package_index) => {
                        self.get_compact_non_package_extension_label(path, non_package_index, &non_package_extensions)
                    }
                }
            })
            .collect()
    }

    /// Port of `formatExtensionDisplayPath`.
    pub fn format_extension_display_path(&self, path: &str) -> String {
        let result = self.format_display_path(path);
        let result = result.strip_suffix("/index.ts").unwrap_or(&result).to_string();
        result.strip_suffix("/index.js").unwrap_or(&result).to_string()
    }

    /// Port of `formatContextPath`.
    pub fn format_context_path(&self, p: &str) -> String {
        let cwd = resolve_path(&self.get_current_cwd());
        let absolute_path = if Path::new(p).is_absolute() {
            resolve_path(p)
        } else {
            resolve_path(&format!("{cwd}{}{p}", std::path::MAIN_SEPARATOR))
        };
        if let Some(relative_path) = get_cwd_relative_path(&absolute_path, &cwd) {
            return relative_path;
        }
        self.format_display_path(&absolute_path)
    }

    /// Port of `getScopeGroup`.
    pub fn get_scope_group(
        &self,
        source_info: Option<&super::interactive_mode_services::AgentConnectionSourceInfo>,
    ) -> &'static str {
        let source = source_info.map(|info| info.source.as_str()).unwrap_or("local");
        let scope = source_info.map(|info| info.scope.as_str()).unwrap_or("project");
        if source == "cli" || scope == "temporary" {
            return "path";
        }
        if scope == "user" {
            return "user";
        }
        if scope == "project" {
            return "project";
        }
        "path"
    }

    /// Port of `getStartupExpansionState`.
    pub fn get_startup_expansion_state(&self) -> bool {
        self.options.verbose || self.tool_output_expanded
    }

    /// Port of `getConnectionQueue`.
    pub fn get_connection_queue(&self) -> AgentConnectionQueueState {
        AgentConnectionQueueState {
            steering: self
                .connection_state
                .as_ref()
                .map(|state| state.session_actions.steering.clone())
                .unwrap_or_default(),
            follow_up: self
                .connection_state
                .as_ref()
                .map(|state| state.session_actions.follow_ups.clone())
                .unwrap_or_default(),
        }
    }

    /// Port of `getAllQueuedMessages`.
    pub fn get_all_queued_messages(&self) -> AgentConnectionQueueState {
        self.get_connection_queue()
    }

    /// Port of `getScopedHeartbeats`.
    pub fn get_scoped_heartbeats(&self) -> Vec<AgentConnectionHeartbeat> {
        let identity = self.connection_state.as_ref().map(|state| {
            super::heartbeat_scope::HeartbeatSessionIdentity {
                active_session_id: state.active_session_id.clone(),
                session_id: state.session_id.clone(),
            }
        });
        let children: Vec<AgentConnectionRlmChildAgentSnapshot> =
            self.subagent_snapshots.values().cloned().collect();
        scope_heartbeats_to_session(&self.heartbeat_catalog, identity.as_ref(), &children)
    }

    /// Port of `getCurrentCwd`.
    pub fn get_current_cwd(&self) -> String {
        self.connection_state
            .as_ref()
            .map(|state| state.cwd.clone())
            .unwrap_or_else(|| self.ui_services().get_initial_cwd())
    }

    /// Port of `getCurrentSessionName`.
    pub fn get_current_session_name(&self) -> Option<String> {
        self.connection_state
            .as_ref()
            .and_then(|state| state.session_name.clone())
            .or_else(|| self.ui_services().get_initial_session_name())
    }

    /// Port of `getCurrentModel`.
    pub fn get_current_model(&self) -> Option<&AgentConnectionModel> {
        self.connection_state.as_ref().and_then(|state| state.model.as_ref())
    }

    /// Port of `getCurrentModelId`.
    pub fn get_current_model_id(&self) -> Option<String> {
        self.get_current_model().map(|model| model.id.clone())
    }

    /// Port of `isAgentStreaming`.
    pub fn is_agent_streaming(&self) -> bool {
        self.connection_state.as_ref().map(|state| state.is_streaming).unwrap_or(false)
    }

    /// Port of `isAgentCompacting`.
    pub fn is_agent_compacting(&self) -> bool {
        self.connection_state.as_ref().map(|state| state.is_compacting).unwrap_or(false)
    }

    /// Port of `isBashRunning`.
    pub fn is_bash_running(&self) -> bool {
        self.connection_state.as_ref().map(|state| state.is_bash_running).unwrap_or(false)
    }

    /// Port of `hasInterruptibleWork`.
    pub fn has_interruptible_work(&self) -> bool {
        self.is_agent_streaming()
            || self.is_agent_compacting()
            || self.is_bash_running()
            || self.get_retry_attempt() > 0.0
            || self
                .connection_state
                .as_ref()
                .map(|state| state.session_actions.active.is_some())
                .unwrap_or(false)
    }

    /// Port of `getRetryAttempt`.
    pub fn get_retry_attempt(&self) -> f64 {
        self.connection_state.as_ref().map(|state| state.retry_attempt).unwrap_or(0.0)
    }

    /// Port of `getQueuedActionCount`.
    pub fn get_queued_action_count(&self) -> usize {
        self.connection_state
            .as_ref()
            .map(|state| state.session_actions.queued_count)
            .unwrap_or(0)
    }

    /// Port of `getGoalState`.
    pub fn get_goal_state(&self) -> GoalState {
        self.connection_state
            .as_ref()
            .map(|state| state.goal.clone())
            .unwrap_or_else(GoalState::empty)
    }

    /// Port of `getConnectionContextUsage`.
    pub fn get_connection_context_usage(&self) -> Option<ContextUsage> {
        let snapshot = self.connection_state.as_ref().map(|state| state.context_usage.clone())?;
        let Some(tokens) = snapshot.tokens else {
            return Some(snapshot);
        };
        if snapshot.context_window <= 0.0 {
            return Some(snapshot);
        }
        // Add only the output produced since the snapshot was last refreshed. The activity
        // tracker accumulates across auto-retries within a turn, so subtract the baseline
        // captured at the last refresh to avoid re-adding a failed attempt's tokens.
        let in_flight = if self.is_agent_streaming() {
            0.0f64.max(self.activity_tracker.get_status().tokens - self.context_usage_token_baseline)
        } else {
            0.0
        };
        if in_flight <= 0.0 {
            return Some(snapshot);
        }
        let tokens = tokens + in_flight;
        Some(ContextUsage {
            tokens: Some(tokens),
            context_window: snapshot.context_window,
            percent: Some((tokens / snapshot.context_window) * 100.0),
        })
    }

    /// Port of `getScopedModelState`.
    pub fn get_scoped_model_state(&self) -> Vec<super::interactive_mode_services::AgentConnectionScopedModel> {
        self.connection_state
            .as_ref()
            .map(|state| state.scoped_models.clone())
            .unwrap_or_default()
    }

    /// Port of `updateTerminalTitle`.
    pub fn update_terminal_title(&mut self) {
        let cwd_basename = basename(&self.get_current_cwd());
        let session_name = self.get_current_session_name();
        let title = match session_name {
            Some(session_name) => format!("{APP_TITLE} - {session_name} - {cwd_basename}"),
            None => format!("{APP_TITLE} - {cwd_basename}"),
        };
        self.ui.terminal.set_title(title);
    }

    /// Port of `formatGoalElapsed`.
    pub fn format_goal_elapsed(&self, seconds: f64) -> String {
        let total_seconds = 0.0f64.max(seconds.trunc());
        if total_seconds < 60.0 {
            return format!("{}s", js_number_to_string(total_seconds));
        }
        let minutes = (total_seconds / 60.0).floor();
        let remaining_seconds = total_seconds % 60.0;
        if minutes < 60.0 {
            return format!(
                "{}m {}s",
                js_number_to_string(minutes),
                pad_start(&js_number_to_string(remaining_seconds), 2, '0')
            );
        }
        let hours = (minutes / 60.0).floor();
        let remaining_minutes = minutes % 60.0;
        format!(
            "{}h {}m",
            js_number_to_string(hours),
            pad_start(&js_number_to_string(remaining_minutes), 2, '0')
        )
    }

    /// Port of `formatWorkingElapsed`.
    pub fn format_working_elapsed(&self, elapsed_ms: f64) -> String {
        let total_seconds = 0.0f64.max((elapsed_ms / 1000.0).floor());
        if total_seconds < 60.0 {
            return format!("{}s", js_number_to_string(total_seconds));
        }
        let minutes = (total_seconds / 60.0).floor();
        let seconds = total_seconds % 60.0;
        if minutes < 60.0 {
            return format!(
                "{}m {}s",
                js_number_to_string(minutes),
                pad_start(&js_number_to_string(seconds), 2, '0')
            );
        }
        let hours = (minutes / 60.0).floor();
        let remaining_minutes = minutes % 60.0;
        if hours < 24.0 {
            return format!(
                "{}h {}m {}s",
                js_number_to_string(hours),
                pad_start(&js_number_to_string(remaining_minutes), 2, '0'),
                pad_start(&js_number_to_string(seconds), 2, '0')
            );
        }
        let days = (hours / 24.0).floor();
        let remaining_hours = hours % 24.0;
        format!(
            "{}d {}h {}m {}s",
            js_number_to_string(days),
            pad_start(&js_number_to_string(remaining_hours), 2, '0'),
            pad_start(&js_number_to_string(remaining_minutes), 2, '0'),
            pad_start(&js_number_to_string(seconds), 2, '0')
        )
    }

    /// Port of `getTrayGoalLabel`.
    pub fn get_tray_goal_label(&self) -> Option<String> {
        let goal = self.get_goal_state();
        match goal.status.as_str() {
            "active" => Some(format!("Pursuing goal ({})", self.format_goal_elapsed(goal.time_used_seconds))),
            "paused" => Some(format!("Goal paused ({})", self.format_goal_elapsed(goal.time_used_seconds))),
            "budget_limited" => Some(format!(
                "Goal budget limited ({})",
                self.format_goal_elapsed(goal.time_used_seconds)
            )),
            "idle" | "complete" | "error" => None,
            _ => None,
        }
    }

    /// Port of `getTrayHeartbeatLabel`.
    pub fn get_tray_heartbeat_label(&self) -> Option<String> {
        let heartbeats = self.get_scoped_heartbeats();
        if heartbeats.is_empty() {
            return None;
        }
        let paused = heartbeats.iter().filter(|heartbeat| heartbeat.job.status == "paused").count();
        let count = format!(
            "{} heartbeat{}",
            heartbeats.len(),
            if heartbeats.len() == 1 { "" } else { "s" }
        );
        let paused_label = if paused > 0 { format!(" \u{b7} {paused} paused") } else { String::new() };
        let shortcut = self.key_text("app.heartbeats.open");
        Some(format!(
            "{count}{paused_label}{}",
            if shortcut.is_empty() { String::new() } else { format!(" ({shortcut})") }
        ))
    }

    /// Port of `getTrayContextLabel`.
    pub fn get_tray_context_label(&self) -> Option<String> {
        let goal_label = self.get_tray_goal_label();
        let heartbeat_label = self.get_tray_heartbeat_label();
        let usage = self.get_connection_context_usage();
        let context_label = match usage {
            Some(usage) => match (usage.tokens, usage.percent) {
                (Some(tokens), Some(percent)) => Some(format!("{} ({}%)", format_token_count(tokens), percent.round() as i64)),
                _ => None,
            },
            None => None,
        };
        let labels: Vec<String> = [goal_label, heartbeat_label, context_label].into_iter().flatten().collect();
        if labels.is_empty() {
            None
        } else {
            Some(labels.join(" \u{b7} "))
        }
    }

    /// Port of `getModelTrayLabel`.
    pub fn get_model_tray_label(&self) -> String {
        let Some(model) = self.get_current_model() else {
            return "\u{2014}".to_string();
        };
        let mut parts = vec![model.name.clone()];
        if model.reasoning {
            let level = self
                .connection_state
                .as_ref()
                .map(|state| state.thinking_level.clone())
                .unwrap_or(ThinkingLevel::Off);
            if level != ThinkingLevel::Off {
                parts.push(level.as_str().to_string());
            }
        }
        if self
            .connection_state
            .as_ref()
            .map(|state| state.service_tier == ServiceTier::Priority)
            .unwrap_or(false)
        {
            parts.push("fast".to_string());
        }
        parts.join(" \u{2022} ")
    }

    /// Port of `isNewChat`.
    pub fn is_new_chat(&self) -> bool {
        self.connection_state.as_ref().map(|state| state.message_count).unwrap_or(0.0) == 0.0
            && !self
                .connection_state
                .as_ref()
                .map(|state| state.is_streaming)
                .unwrap_or(false)
    }

    /// Port of `getShortcutsTrayHint`.
    pub fn get_shortcuts_tray_hint(&self) -> Option<String> {
        if !self.is_new_chat() {
            return None;
        }
        let shortcuts = self.key_text("app.shortcuts");
        if shortcuts.is_empty() {
            Some("/hotkeys for shortcuts".to_string())
        } else {
            Some(self.key_hint("app.shortcuts", "for shortcuts"))
        }
    }

    /// Port of `getAgentsViewTrayHint`.
    pub fn get_agents_view_tray_hint(&self) -> Option<String> {
        if !self.options.return_to_agents_view {
            return None;
        }
        Some(self.key_hint("app.agents.back", "agents/resume"))
    }

    /// Port of `getTrayLocationLabel`.
    pub fn get_tray_location_label(&self) -> Option<String> {
        let model_label = self.get_model_tray_label();
        let has_children = self.options.session_has_children || !self.subagent_snapshots.is_empty();
        let depth_label = format_agent_depth_label(self.options.session_depth, has_children);
        let shortcuts_hint = self.get_shortcuts_tray_hint();
        let agents_hint = self.get_agents_view_tray_hint();
        let labels: Vec<String> = [agents_hint, depth_label, Some(model_label), shortcuts_hint]
            .into_iter()
            .flatten()
            .collect();
        Some(labels.join("  "))
    }

    /// Port of `getTrayOverrideLabel`.
    pub fn get_tray_override_label(&self, editor_text: &str) -> Option<String> {
        if self.is_ctrl_c_exit_hint_visible() {
            let clear_key = self.key_text("app.clear");
            return Some(if clear_key.is_empty() {
                "Press again to exit".to_string()
            } else {
                format!("Press {clear_key} again to exit")
            });
        }
        if !self.is_agent_streaming() || editor_text.trim().is_empty() {
            return None;
        }
        Some(format!("{} to queue message", self.key_text("app.message.followUp")))
    }

    /// Port of `isCtrlCExitHintVisible`.
    pub fn is_ctrl_c_exit_hint_visible(&self) -> bool {
        self.ctrl_c_exit_hint_expires_at > now_ms()
    }

    /// Port of `getQueueSelectionHeader`.
    pub fn get_queue_selection_header(&self) -> Option<String> {
        let selected = self.queue_selection.selected()?;
        let lane = if selected.lane == super::queue_selection::QueueLane::Steering { "steering" } else { "follow-up" };
        let older = self.capitalize_key(&self.key_text("app.message.navigateOlder"));
        let newer = self.capitalize_key(&self.key_text("app.message.navigateNewer"));
        let earlier = self.capitalize_key(&self.key_text("app.message.moveEarlier"));
        let later = self.capitalize_key(&self.key_text("app.message.moveLater"));
        let queue = self.capitalize_key(&self.key_text("app.message.followUp"));
        Some(theme().fg(
            "dim",
            &format!(
                "{lane} {} \u{b7} {older}/{newer} browse \u{b7} {earlier}/{later} reorder \u{b7} enter steers \u{b7} {queue} queues \u{b7} empty deletes",
                selected.index + 1
            ),
        ))
    }

    /// Port of `getWorkingLoaderMessage`.
    pub fn get_working_loader_message(&self) -> String {
        let elapsed = self
            .working_started_at
            .map(|started_at| self.format_working_elapsed(now_ms() - started_at));
        let status = self.activity_tracker.get_status();
        // The subagent count/recaps live in the tree above the loader, so the loader
        // message itself no longer repeats "N subagents running".
        if !self.is_agent_streaming() {
            return String::new();
        }
        if let Some(working_message) = &self.working_message {
            // Extensions and tool bootstrap own the message; keep the plain "<message> <elapsed>" form.
            return match elapsed {
                Some(elapsed) => format!("{working_message} {elapsed}"),
                None => working_message.clone(),
            };
        }
        let mut parts: Vec<String> = vec![super::agent_activity::agent_activity_label(status.activity).to_string()];
        if let Some(elapsed) = elapsed {
            parts.push(elapsed);
        }
        if status.tokens > 0.0 {
            parts.push(format!(
                "{} {} tokens",
                if status.direction == super::agent_activity::Direction::Down { "\u{2193}" } else { "\u{2191}" },
                format_token_count(status.tokens)
            ));
        }
        parts.join(" \u{b7} ")
    }

    /// Port of `shouldShowWorkingLoader`.
    pub fn should_show_working_loader(&self) -> bool {
        // Background subagents (agent turn done, asyncio tasks still running) would
        // otherwise show a textless spinner; the subagent tree above the loader carries
        // that state, so the loader only shows while the main agent is itself streaming.
        self.working_visible && self.is_agent_streaming()
    }

    /// Port of `shouldSuppressFeatureHint`.
    pub fn should_suppress_feature_hint(&self) -> bool {
        let queue = self.get_all_queued_messages();
        !queue.steering.is_empty() || !queue.follow_up.is_empty()
    }

    /// Port of `isModelProviderConfigured`.
    pub fn is_model_provider_configured(&self, model: &AgentConnectionModel) -> bool {
        self.connection_configured_providers.contains(&model.provider)
            || self.model_registry().has_configured_auth(model)
    }

    /// Port of `currentModelSupportsFastMode`.
    pub fn current_model_supports_fast_mode(&self) -> bool {
        self.get_current_model().map(pi_ai::models::supports_fast_mode).unwrap_or(false)
    }

    /// Port of `getAvailableThinkingLevels`.
    pub fn get_available_thinking_levels(&self) -> Vec<ThinkingLevel> {
        let levels = self
            .connection_state
            .as_ref()
            .map(|state| state.available_thinking_levels.clone())
            .unwrap_or_default();
        let supports_thinking =
            !levels.is_empty() && !(levels.len() == 1 && levels[0] == ThinkingLevel::Off);
        if supports_thinking {
            levels
        } else {
            Vec::new()
        }
    }

    /// Port of `getThinkingLevelCompletions`.
    pub fn get_thinking_level_completions(&self, prefix: &str) -> Option<Vec<AutocompleteItem>> {
        let levels = self.get_available_thinking_levels();
        if levels.is_empty() {
            return None;
        }
        let current = self.connection_state.as_ref().map(|state| state.thinking_level.clone());
        let term = prefix.trim().to_lowercase();
        let matches: Vec<ThinkingLevel> = if term.is_empty() {
            levels
        } else {
            levels.into_iter().filter(|level| level.as_str().starts_with(&term)).collect()
        };
        if matches.is_empty() {
            return None;
        }
        let descriptions = thinking_level_descriptions();
        Some(
            matches
                .into_iter()
                .map(|level| {
                    let description = descriptions.get(&level).copied().unwrap_or("");
                    AutocompleteItem {
                        value: level.as_str().to_string(),
                        label: level.as_str().to_string(),
                        description: Some(if Some(&level) == current.as_ref() {
                            format!("{description} (current)")
                        } else {
                            description.to_string()
                        }),
                    }
                })
                .collect(),
        )
    }

    /// Port of `getHeartbeatArgumentCompletions`.
    pub fn get_heartbeat_argument_completions(&self, prefix: &str) -> Option<Vec<AutocompleteItem>> {
        let term = prefix.trim().to_lowercase();
        let all = heartbeat_argument_completions();
        let filtered: Vec<(String, String)> = if term.is_empty() {
            all
        } else {
            all.into_iter()
                .filter(|(value, label)| {
                    value.to_lowercase().starts_with(&term) || label.to_lowercase().starts_with(&term)
                })
                .collect()
        };
        if filtered.is_empty() {
            return None;
        }
        Some(
            filtered
                .into_iter()
                .map(|(value, label)| AutocompleteItem { value, label, description: None })
                .collect(),
        )
    }

    /// Port of `formatGoalStatus`.
    pub fn format_goal_status(&self, goal: &GoalState, terminal_columns: f64) -> String {
        let usage = crate::core::goals::format_goal_usage(goal);
        let usage_text = usage.map(|usage| format!(" ({usage})")).unwrap_or_default();
        match goal.status.as_str() {
            "idle" => "No active goal".to_string(),
            "active" => match &goal.objective {
                Some(objective) => format!(
                    "Goal{}",
                    self.format_goal_detail_suffix(Some(objective), visible_width("Goal"), terminal_columns)
                ),
                None => "Pursuing goal".to_string(),
            },
            "paused" => match &goal.last_reason {
                Some(last_reason) => format!(
                    "Goal paused{}",
                    self.format_goal_detail_suffix(
                        Some(last_reason),
                        visible_width("Goal paused"),
                        terminal_columns
                    )
                ),
                None => "Goal paused (/goal resume)".to_string(),
            },
            "budget_limited" => match &goal.last_reason {
                Some(last_reason) => {
                    let prefix = format!("Goal budget limited{usage_text}");
                    let suffix = self.format_goal_detail_suffix(
                        Some(last_reason),
                        visible_width(&prefix),
                        terminal_columns,
                    );
                    format!("{prefix}{suffix}")
                }
                None => format!("Goal budget limited{usage_text}"),
            },
            "complete" => match &goal.last_reason {
                Some(last_reason) => format!(
                    "Goal complete{}",
                    self.format_goal_detail_suffix(
                        Some(last_reason),
                        visible_width("Goal complete"),
                        terminal_columns
                    )
                ),
                None => "Goal complete".to_string(),
            },
            "error" => match &goal.last_error {
                Some(last_error) => format!(
                    "Goal error{}",
                    self.format_goal_detail_suffix(
                        Some(last_error),
                        visible_width("Goal error"),
                        terminal_columns
                    )
                ),
                None => "Goal error".to_string(),
            },
            _ => String::new(),
        }
    }

    /// Port of `formatGoalDetailSuffix`.
    pub fn format_goal_detail_suffix(
        &self,
        value: Option<&str>,
        prefix_width: f64,
        terminal_columns: f64,
    ) -> String {
        let detail = value.map(|value| value.split_whitespace().collect::<Vec<&str>>().join(" "));
        let Some(detail) = detail else {
            return String::new();
        };
        if detail.is_empty() {
            return String::new();
        }
        let available_width = 120.0f64.min(1.0f64.max(terminal_columns - prefix_width - 2.0));
        if available_width < 8.0 {
            return String::new();
        }
        format!(": {}", truncate_to_width(&detail, available_width, "", true))
    }

    /// Port of `getPathCommandArgument`.
    pub fn get_path_command_argument(&self, text: &str, command: &str) -> Option<String> {
        if text == command {
            return None;
        }
        if !text.starts_with(&format!("{command} ")) {
            return None;
        }

        let args_string = text[command.len() + 1..].trim_start();
        if args_string.is_empty() {
            return None;
        }

        let first_char = args_string.chars().next()?;
        if first_char == '"' || first_char == '\'' {
            let rest = &args_string[1..];
            let closing_quote_index = rest.find(first_char)?;
            return Some(rest[..closing_quote_index].to_string());
        }

        match args_string.find(char::is_whitespace) {
            Some(index) => Some(args_string[..index].to_string()),
            None => Some(args_string.to_string()),
        }
    }

    /// Port of `capitalizeKey`.
    pub fn capitalize_key(&self, key: &str) -> String {
        key.split('/')
            .map(|k| {
                k.split('+')
                    .map(|part| {
                        if part == "esc" {
                            part.to_string()
                        } else {
                            let mut chars = part.chars();
                            match chars.next() {
                                Some(first) => format!("{}{}", first.to_uppercase(), chars.as_str()),
                                None => String::new(),
                            }
                        }
                    })
                    .collect::<Vec<String>>()
                    .join("+")
            })
            .collect::<Vec<String>>()
            .join("/")
    }

    /// Port of `getAppKeyDisplay`.
    pub fn get_app_key_display(&self, action: &str) -> String {
        self.capitalize_key(&self.key_text(action))
    }

    /// Port of `getEditorKeyDisplay`.
    pub fn get_editor_key_display(&self, action: &str) -> String {
        self.capitalize_key(&self.key_text(action))
    }

    /// Stand-in for `keyText(action)` (components/keybinding-hints.ts, other slice).
    fn key_text(&self, action: &str) -> String {
        let keys = pi_tui::keybindings::get_keybindings().get_keys(action);
        format_key_text(&keys, std::env::consts::OS)
    }

    /// Stand-in for `keyHint(action, description)`.
    fn key_hint(&self, action: &str, description: &str) -> String {
        format!("{}{}", theme().fg("dim", &self.key_text(action)), theme().fg("muted", &format!(" {description}")))
    }

    /// Port of `getUserMessageText`.
    pub fn get_user_message_text(&self, message: &pi_ai::types::UserMessage) -> String {
        match &message.content {
            pi_ai::types::UserContent::Text(text) => text.clone(),
            pi_ai::types::UserContent::Blocks(blocks) => blocks
                .iter()
                .filter_map(|block| match block {
                    pi_ai::types::ImageOrTextContent::Text(text) => Some(text.text.clone()),
                    _ => None,
                })
                .collect::<Vec<String>>()
                .join(""),
        }
    }

    /// Port of `isTextOnlyUserMessage`.
    pub fn is_text_only_user_message(&self, message: &pi_ai::types::UserMessage) -> bool {
        match &message.content {
            pi_ai::types::UserContent::Text(_) => true,
            pi_ai::types::UserContent::Blocks(blocks) => blocks
                .iter()
                .all(|block| matches!(block, pi_ai::types::ImageOrTextContent::Text(_))),
        }
    }

    /// Port of `getMarkdownThemeWithSettings`.
    pub fn get_markdown_theme_with_settings(&self) -> super::theme::theme::MarkdownTheme {
        let mut markdown_theme = super::theme::theme::get_markdown_theme();
        markdown_theme.code_block_indent = Some(self.settings_manager().get_code_block_indent());
        markdown_theme
    }

    /// Port of `getCachedModelCandidates`.
    pub fn get_cached_model_candidates(&self) -> Vec<AgentConnectionModel> {
        let mut models_by_id: indexmap::IndexMap<String, AgentConnectionModel> = indexmap::IndexMap::new();
        for scoped in self.get_scoped_model_state() {
            models_by_id.insert(format!("{}/{}", scoped.model.provider, scoped.model.id), scoped.model);
        }
        for model in &self.connection_model_catalog {
            models_by_id.insert(format!("{}/{}", model.provider, model.id), model.clone());
        }
        models_by_id.into_values().collect()
    }

    /// Port of `invalidateConnectionModels`.
    pub fn invalidate_connection_models(&mut self) {
        self.connection_configured_providers = HashSet::new();
        self.connection_models_fetched_at = 0.0;
        self.invalidate_connection_model_refresh();
    }

    /// Port of `invalidateConnectionModelRefresh`.
    pub fn invalidate_connection_model_refresh(&mut self) {
        self.connection_models_refresh_version += 1;
        self.connection_models_refresh_in_flight = false;
    }

    /// Port of `applyConnectionStateSnapshot`.
    pub fn apply_connection_state_snapshot(&mut self, state: AgentConnectionState) {
        self.bind_prompt_stash_session(&state.session_id);
        self.connection_state = Some(state.clone());
        // Don't touch contextUsageTokenBaseline: a mid-stream snapshot reflects only completed
        // turns (the in-flight message isn't persisted yet), so the in-flight delta must keep
        // accumulating. The baseline is managed at turn end (refreshConnectionContextUsage) and
        // reset on a new user message.
        self.session_recap = state.recap.clone();
        self.update_working_pulse();
    }

    /// Port of `patchConnectionState`.
    pub fn patch_connection_state(&mut self, patch: impl FnOnce(&mut AgentConnectionState)) {
        let Some(state) = self.connection_state.as_mut() else {
            return;
        };
        patch(state);
        self.update_working_pulse();
    }

    /// Port of `setGoalAnnouncementBaseline`.
    pub fn set_goal_announcement_baseline(&mut self, goal: &GoalState) {
        self.last_goal_announcement = Some(self.goal_announcement_snapshot(goal));
    }

    /// Port of `goalAnnouncementSnapshot`.
    pub fn goal_announcement_snapshot(&self, goal: &GoalState) -> GoalAnnouncementSnapshot {
        GoalAnnouncementSnapshot {
            goal_id: goal.goal_id.clone(),
            status: goal.status.clone(),
            objective: goal.objective.clone(),
            last_reason: goal.last_reason.clone(),
            last_error: goal.last_error.clone(),
        }
    }

    /// Port of `shouldAnnounceGoalUpdate`.
    pub fn should_announce_goal_update(&mut self, goal: &GoalState) -> bool {
        let previous = self.last_goal_announcement.clone();
        let next = self.goal_announcement_snapshot(goal);
        self.last_goal_announcement = Some(next.clone());
        let Some(previous) = previous else {
            return goal.status != "idle";
        };
        if previous.status != next.status {
            return true;
        }
        if previous.goal_id != next.goal_id {
            return goal.status != "idle";
        }
        match goal.status.as_str() {
            "active" => false,
            "paused" | "budget_limited" | "complete" => previous.last_reason != next.last_reason,
            "error" => previous.last_error != next.last_error,
            "idle" => false,
            _ => false,
        }
    }

    /// Port of `seedSubagentSummary`.
    pub fn seed_subagent_summary(&mut self, children: Option<&[AgentConnectionRlmChildAgentSnapshot]>) {
        for child in children.unwrap_or(&[]) {
            // Live updates can arrive before the initial snapshot; do not replace them
            // with the snapshot's older state.
            if !self.subagent_snapshots.contains_key(&child.id) && child.status != "cancelled" {
                self.subagent_snapshots.insert(child.id.clone(), child.clone());
            }
        }
        self.refresh_subagent_summary();
    }

    /// Port of `replaceSubagentSummary`.
    pub fn replace_subagent_summary(&mut self, children: Option<&[AgentConnectionRlmChildAgentSnapshot]>) {
        let mut next: HashMap<String, AgentConnectionRlmChildAgentSnapshot> = HashMap::new();
        for child in children.unwrap_or(&[]) {
            if child.status == "cancelled" {
                continue;
            }
            let previous = self.subagent_snapshots.get(&child.id);
            next.insert(
                child.id.clone(),
                match previous {
                    Some(previous) => merge_subagent_snapshot(previous, child),
                    None => child.clone(),
                },
            );
        }
        self.subagent_snapshots = next;
        self.refresh_subagent_summary();
    }

    /// Port of `updateSubagentSummary`.
    pub fn update_subagent_summary(&mut self, child: AgentConnectionRlmChildAgentSnapshot) {
        // "cancelled" also covers never-bound terminal runs; AgentSession owns that rule.
        if child.status == "cancelled" {
            self.remove_subagent_snapshot(&child.id);
        } else {
            let previous = self.subagent_snapshots.get(&child.id).cloned();
            self.subagent_snapshots.insert(
                child.id.clone(),
                match previous {
                    Some(previous) => merge_subagent_snapshot(&previous, &child),
                    None => child,
                },
            );
        }
        self.refresh_subagent_summary();
    }

    /// Port of `refreshSubagentSummary`.
    pub fn refresh_subagent_summary(&mut self) {
        self.update_subagent_summary_line();
        self.update_working_pulse();
        self.sync_working_loader();
        self.ui.request_render();
    }

    /// Port of `updateSubagentSummaryLine`.
    pub fn update_subagent_summary_line(&mut self) {
        let roster_summaries = self.roster_bar.as_ref().map(|roster_bar| roster_bar.summaries());
        // A client-owned session has no row on the public roster; only then do the
        // snapshots carry the bar. A public parent with zero roster children shows zero.
        let session_on_roster = roster_summaries
            .as_ref()
            .map(|rows| {
                rows.iter().any(|row| {
                    Some(row.session_id.as_str())
                        == self.connection_state.as_ref().map(|state| state.session_id.as_str())
                })
            })
            .unwrap_or(false);
        let counts = match (&roster_summaries, session_on_roster) {
            (Some(rows), true) => {
                let active = self
                    .connection_state
                    .as_ref()
                    .and_then(|state| state.active_session_id.clone())
                    .unwrap_or_default();
                let session_id = self
                    .connection_state
                    .as_ref()
                    .map(|state| state.session_id.clone())
                    .unwrap_or_default();
                let session_file = self.connection_state.as_ref().and_then(|state| state.session_file.clone());
                count_roster_subagent_statuses(rows, (&active, &session_id, session_file.as_deref()))
            }
            _ => {
                let children: Vec<AgentConnectionRlmChildAgentSnapshot> =
                    self.subagent_snapshots.values().cloned().collect();
                count_direct_subagent_statuses(&children, self.rlm_node_id.as_deref())
            }
        };
        self.subagent_summary_line.set_subagent_counts(counts);
    }

    /// Port of `removeSubagentSnapshot`.
    pub fn remove_subagent_snapshot(&mut self, id: &str) {
        self.subagent_snapshots.remove(id);
        let children: Vec<AgentConnectionRlmChildAgentSnapshot> = self.subagent_snapshots.values().cloned().collect();
        for child in children {
            if child.parent_id.as_deref() == Some(id) {
                self.remove_subagent_snapshot(&child.id);
            }
        }
    }

    /// Port of `resetSubagentSummary`.
    pub fn reset_subagent_summary(&mut self) {
        self.subagent_snapshots.clear();
        self.rlm_node_id = None;
        self.update_subagent_summary_line();
        // Clearing snapshots can drop the last running subagent; reconcile the
        // pulse and loader so neither lingers when nothing is in flight.
        self.update_working_pulse();
        self.sync_working_loader();
    }

    /// Port of `updateWorkingPulse`.
    pub fn update_working_pulse(&mut self) {
        let active = self.is_agent_streaming();
        if !active {
            self.stop_working_pulse();
            return;
        }
        if !self.working_pulse_active {
            self.working_pulse_active = true;
        }
    }

    /// Port of `tickWorkingPulse`.
    pub fn tick_working_pulse(&mut self) {
        self.pulse_frame += 1;
        set_working_pulse_frame(self.pulse_frame);
        self.ui.request_render();
    }

    /// Port of `stopWorkingPulse`.
    pub fn stop_working_pulse(&mut self) {
        self.working_pulse_active = false;
    }

    /// Port of `syncWorkingLoader`.
    pub fn sync_working_loader(&mut self) {
        // Compaction/retry own the status container while active; don't fight them.
        if self.is_agent_compacting() {
            return;
        }
        if self.should_show_working_loader() {
            self.status_container.clear();
        }
        self.ui.request_render();
    }

    /// Port of `setWorkingVisible`.
    pub fn set_working_visible(&mut self, visible: bool) {
        self.working_visible = visible;
        if !visible {
            self.stop_working_loader();
            self.ui.request_render();
            return;
        }
        if self.should_show_working_loader() {
            self.status_container.clear();
        }
        self.ui.request_render();
    }

    /// Port of `stopWorkingLoader`.
    pub fn stop_working_loader(&mut self) {
        self.working_started_at = None;
        self.status_container.clear();
    }

    /// Port of `setWorkingIndicator`.
    pub fn set_working_indicator(&mut self, options: Option<LoaderIndicatorOptions>) {
        self.working_indicator_options = options;
        self.ui.request_render();
    }

    /// Port of `setHiddenThinkingLabel`.
    pub fn set_hidden_thinking_label(&mut self, label: Option<String>) {
        self.hidden_thinking_label = label.unwrap_or_else(|| self.default_hidden_thinking_label.clone());
        self.ui.request_render();
    }

    /// Port of `setToolsExpanded`.
    pub fn set_tools_expanded(&mut self, expanded: bool) {
        self.tool_output_expanded = expanded;
        self.apply_chat_expansion();
    }

    /// Port of `toggleToolOutputExpansion`.
    pub fn toggle_tool_output_expansion(&mut self) {
        self.set_tools_expanded(!self.tool_output_expanded);
    }

    /// Port of `toggleAgentMessageExpansion`.
    pub fn toggle_agent_message_expansion(&mut self) {
        self.agent_messages_expanded = !self.agent_messages_expanded;
        self.apply_chat_expansion();
    }

    /// Port of `toggleEditDiffExpansion`.
    pub fn toggle_edit_diff_expansion(&mut self) {
        self.edit_diffs_expanded = !self.edit_diffs_expanded;
        self.apply_chat_expansion();
    }

    /// Port of `applyChatExpansion`.
    pub fn apply_chat_expansion(&mut self) {
        // Expanding/collapsing changes blocks above the viewport, which would
        // otherwise force a full redraw that scrolls to the top and replays the
        // whole transcript. Keep the user anchored at their current position.
        // Fullscreen frames have no scrollback to preserve.
        if self.ui.is_fullscreen() {
            self.ui.request_render();
        } else {
            self.ui.request_render_preserving_viewport();
        }
    }

    /// Port of `startFeatureHintPresentation`.
    pub fn start_feature_hint_presentation(&mut self) {
        self.clear_feature_hint_presentation();
        if self.should_suppress_feature_hint() {
            return;
        }
        if self.feature_hint_eligible_at == 0.0 {
            self.feature_hint_eligible_at = now_ms() + FEATURE_HINT_DELAY_MS;
        }
        let delay = 0.0f64.max(self.feature_hint_eligible_at - now_ms());
        if delay == 0.0 {
            self.show_feature_hint();
        }
    }

    /// Port of `clearFeatureHintPresentation`.
    pub fn clear_feature_hint_presentation(&mut self) {
        self.feature_hint_container.clear();
        self.current_feature_hint = None;
    }

    /// Port of `endFeatureHintRun`.
    pub fn end_feature_hint_run(&mut self) {
        self.clear_feature_hint_presentation();
        self.current_feature_hint = None;
        self.feature_hint_eligible_at = 0.0;
        self.feature_hint_run_pending = false;
    }

    /// Port of `prepareFeatureHintRun`.
    pub fn prepare_feature_hint_run(&mut self, message: &AgentMessage) {
        if !self.feature_hint_run_pending {
            return;
        }
        if message.role() == "assistant" {
            self.feature_hint_run_pending = false;
            return;
        }
        if !starts_agent_run(message) {
            return;
        }

        self.end_feature_hint_run();
        if self.should_show_working_loader() {
            self.start_feature_hint_presentation();
        }
    }

    /// Port of `showFeatureHint`.
    pub fn show_feature_hint(&mut self) {
        if self.should_suppress_feature_hint() || !self.should_show_working_loader() {
            return;
        }
        if self.current_feature_hint.is_none() {
            let get_keybinding = |action: &str| {
                let key = pi_tui::keybindings::get_keybindings().get_keys(action);
                let key = format_key_text(&key, std::env::consts::OS);
                if key.is_empty() {
                    None
                } else {
                    Some(key)
                }
            };
            let context = super::feature_hints::FeatureHintContext {
                get_keybinding: Box::new(get_keybinding),
                is_resident_session: self.options.return_to_agents_view,
            };
            let hint = self.feature_hint_deck.next(&context);
            self.current_feature_hint = hint.map(|hint| hint.text);
        }
        if self.current_feature_hint.is_none() {
            return;
        }
        self.ui.request_render();
    }

    /// Port of `resumeFeatureHintPresentation`.
    pub fn resume_feature_hint_presentation(&mut self) {
        if !self.should_suppress_feature_hint() && self.should_show_working_loader() {
            self.start_feature_hint_presentation();
        }
    }

    /// Port of `expansionStateFor`.
    pub fn expansion_state_for(&self, is_agent_message_component: bool) -> bool {
        if is_agent_message_component {
            self.agent_messages_expanded
        } else {
            self.tool_output_expanded
        }
    }

    /// Port of `showStatus`.
    pub fn show_status(&mut self, message: &str, tone: ThemeColor) {
        self.chat_container
            .add_child(Box::new(super::interactive_mode_services::Spacer::new(1)));
        self.chat_container
            .add_child(Box::new(Text::new(theme().fg(tone, message), 1, 0)));
        self.ui.request_render();
    }

    /// Port of `resetPagedHistory`.
    pub fn reset_paged_history(&mut self) {
        self.paged_history_generation_bump();
        self.history_container.clear();
    }

    fn paged_history_generation_bump(&mut self) {}

    /// Port of `run`.
    ///
    /// PARTIAL: the startup-prompt admission barrier and the input loop depend on
    /// `AgentConnection` (agent-connection slice). The Rust port returns the run
    /// result shape with the same defaults; see blocked_on.
    pub async fn run(&mut self) -> InteractiveModeRunResult {
        let state = self.connection_state.clone();
        InteractiveModeRunResult {
            type_: self.agents_view_request.unwrap_or(InteractiveModeRunResultType::AgentsView),
            source: InteractiveModeRunResultSource {
                active_session_id: state.as_ref().and_then(|state| state.active_session_id.clone()),
                session_file: state.as_ref().and_then(|state| state.session_file.clone()),
                session_id: state
                    .as_ref()
                    .map(|state| state.session_id.clone())
                    .or_else(|| self.prompt_stash_session_id.clone())
                    .unwrap_or_default(),
                session_name: state.as_ref().and_then(|state| state.session_name.clone()),
                cwd: state.as_ref().map(|state| state.cwd.clone()).unwrap_or_else(|| self.get_current_cwd()),
            },
        }
    }

    /// Port of `init` (TUI assembly; tool bootstrap and startup rendering live in
    /// other slices, so the port keeps the container wiring and the theme setup).
    pub async fn init(&mut self) -> Result<(), String> {
        if self.is_initialized {
            return Ok(());
        }

        self.header_container
            .add_child(Box::new(super::interactive_mode_services::Spacer::new(1)));
        self.main_container.add_child(Box::new(super::interactive_mode_services::Container::new()));
        self.ui.add_child(Box::new(Text::new("", 0, 0)));
        self.ui.start();
        self.fullscreen_enabled = self.options.force_fullscreen || self.settings_manager().get_fullscreen();
        self.is_initialized = true;
        Ok(())
    }

    /// Port of `checkShutdownRequested`.
    pub async fn check_shutdown_requested(&mut self) {
        if !self.shutdown_requested {
            return;
        }
        self.shutdown().await;
    }

    /// Port of `shutdown` (TUI teardown; connection disposal is owned by the
    /// agent-connection slice).
    pub async fn shutdown(&mut self) {
        self.stop_working_pulse();
        self.ui.stop(false, None);
        if let Some(on_shutdown) = self.options.on_shutdown.as_mut() {
            on_shutdown();
        }
    }

    /// Port of `teardownSessionUi`.
    pub fn teardown_session_ui(&mut self) {
        self.release_prompt_stash_session();
        self.reset_paged_history();
        self.reset_subagent_summary();
    }
}

/// `AutocompleteItem` (pi-tui, other slice).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutocompleteItem {
    pub value: String,
    pub label: String,
    pub description: Option<String>,
}

/// Stand-in for `Math.random`.
fn default_random() -> f64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    std::time::SystemTime::now().hash(&mut hasher);
    (hasher.finish() % 1_000_000) as f64 / 1_000_000.0
}

/// `Date.now()`
fn now_ms() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as f64)
        .unwrap_or(0.0)
}

fn pad_start(value: &str, width: usize, pad: char) -> String {
    if value.len() >= width {
        value.to_string()
    } else {
        format!("{}{value}", pad.to_string().repeat(width - value.len()))
    }
}

fn basename(path: &str) -> String {
    path.replace('\\', "/").split('/').next_back().unwrap_or("").to_string()
}

fn home_dir() -> String {
    std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")).unwrap_or_default()
}

fn resolve_path(path: &str) -> String {
    std::fs::canonicalize(path)
        .map(|path| path.to_string_lossy().to_string())
        .unwrap_or_else(|_| path.to_string())
}

fn relative_path_between(from: &str, to: &str) -> String {
    let from = Path::new(from);
    let to = Path::new(to);
    match pathdiff(from, to) {
        Some(relative) => relative.to_string_lossy().to_string(),
        None => to.to_string_lossy().to_string(),
    }
}

/// Minimal `path.relative` stand-in.
fn pathdiff(from: &Path, to: &Path) -> Option<std::path::PathBuf> {
    let from_components: Vec<_> = from.components().collect();
    let to_components: Vec<_> = to.components().collect();
    let mut common = 0usize;
    while common < from_components.len().min(to_components.len())
        && from_components[common] == to_components[common]
    {
        common += 1;
    }
    let mut result = std::path::PathBuf::new();
    for _ in common..from_components.len() {
        result.push("..");
    }
    for component in &to_components[common..] {
        result.push(component);
    }
    Some(result)
}

/// `/node_modules\/(@?[^/]+(?:\/[^/]+)?)\/(.*)/`
fn regex_match_node_modules(full_path: &str) -> Option<String> {
    let normalized = full_path.replace('\\', "/");
    let index = normalized.find("/node_modules/")?;
    let rest = &normalized[index + "/node_modules/".len()..];
    let (first, remainder) = match rest.split_once('/') {
        Some((first, remainder)) => (first, remainder),
        None => return None,
    };
    if first.starts_with('@') {
        let (second, remainder) = remainder.split_once('/')?;
        let _ = second;
        return Some(remainder.to_string());
    }
    Some(remainder.to_string())
}

/// `/git\/[^/]+\/[^/]+\/(.*)/`
fn regex_match_git_path(full_path: &str) -> Option<String> {
    let normalized = full_path.replace('\\', "/");
    let index = normalized.find("/git/")?;
    let rest = &normalized[index + "/git/".len()..];
    let mut parts = rest.splitn(3, '/');
    parts.next()?;
    parts.next()?;
    parts.next().map(|value| value.to_string())
}

/// Minimal `path.posix.parse`.
fn posix_parse(path: &str) -> (String, String) {
    match path.rfind('/') {
        Some(index) => (path[..index].to_string(), path[index + 1..].to_string()),
        None => (String::new(), path.to_string()),
    }
}

/// `formatKeyText` (components/keybinding-hints.ts, other slice).
fn format_key_text(keys: &[String], platform: &str) -> String {
    if keys.is_empty() {
        return String::new();
    }
    let joined = keys.join("/");
    joined
        .split('/')
        .map(|binding| {
            binding
                .split('+')
                .map(|part| {
                    let normalized = if part == "escape" { "esc" } else { part };
                    let arrow = match normalized {
                        "up" => Some("\u{2191}"),
                        "down" => Some("\u{2193}"),
                        "left" => Some("\u{2190}"),
                        "right" => Some("\u{2192}"),
                        _ => None,
                    };
                    if let Some(arrow) = arrow {
                        return arrow.to_string();
                    }
                    if platform == "macos" && normalized == "alt" {
                        return "Option".to_string();
                    }
                    let mut chars = normalized.chars();
                    match chars.next() {
                        Some(first) => format!("{}{}", first.to_uppercase(), chars.as_str()),
                        None => String::new(),
                    }
                })
                .collect::<Vec<String>>()
                .join("+")
        })
        .collect::<Vec<String>>()
        .join("/")
}

/// Stand-in for `startsAgentRun` (core/agent-messages.ts).
fn starts_agent_run(message: &AgentMessage) -> bool {
    match message {
        AgentMessage::Message(pi_ai::types::Message::User(_)) => true,
        AgentMessage::Custom(custom) => custom.role() != "assistant",
        _ => false,
    }
}

/// `visibleWidth` re-export.
fn visible_width(text: &str) -> f64 {
    pi_tui::utils::visible_width(text) as f64
}

/// `truncateToWidth` re-export.
fn truncate_to_width(text: &str, max_width: f64, ellipsis: &str, pad: bool) -> String {
    pi_tui::utils::truncate_to_width(text, max_width, ellipsis, pad)
}

fn options_agent_connection(options: &InteractiveModeOptions) -> AgentConnection {
    Arc::clone(&options.agent_connection)
}

fn clone_ui_services(services: &InteractiveModeUiServices) -> InteractiveModeUiServices {
    InteractiveModeUiServices {
        settings_manager: Arc::clone(&services.settings_manager),
        model_registry: Arc::clone(&services.model_registry),
        get_initial_cwd: Box::new(|| String::new()),
        get_initial_session_name: Box::new(|| None),
        get_themes: Box::new(Vec::new),
        refresh_mcp_providers: None,
    }
}

impl InteractiveMode {
    /// Port of `armEscapeRepeat`.
    pub fn arm_escape_repeat(&mut self, action: &'static str) {
        self.clear_escape_repeat();
        self.escape_repeat_action = Some(action);
        self.escape_repeat_expires_at = now_ms() + Self::ESCAPE_REPEAT_WINDOW_MS;
    }

    /// Port of `takeEscapeRepeatAction`.
    pub fn take_escape_repeat_action(&mut self) -> Option<&'static str> {
        let Some(action) = self.escape_repeat_action else {
            self.clear_escape_repeat();
            return None;
        };
        if self.escape_repeat_expires_at <= now_ms() {
            self.clear_escape_repeat();
            return None;
        }
        self.clear_escape_repeat();
        Some(action)
    }

    /// Port of `clearEscapeRepeat`.
    pub fn clear_escape_repeat(&mut self) {
        self.escape_repeat_action = None;
        self.escape_repeat_expires_at = 0.0;
    }

    /// Port of `handleEscape`.
    ///
    /// PARTIAL: `clearSideQuestion` and `showTreeSelector` need the side-question
    /// and tree-selector components from other slices.
    pub fn handle_escape(&mut self, has_side_question: bool) {
        self.clear_ctrl_c_exit_hint(true);
        if has_side_question {
            self.clear_escape_repeat();
            return;
        }
        match self.take_escape_repeat_action() {
            Some("tree") => return,
            Some("clear") => {
                self.clear_input_bar();
                return;
            }
            _ => {}
        }

        self.arm_escape_repeat(if self.has_interruptible_work() { "tree" } else { "clear" });
        self.interrupt_or_clear_input();
    }

    /// Port of `handleCtrlC`.
    pub async fn handle_ctrl_c(&mut self) {
        self.clear_escape_repeat();
        if self.is_ctrl_c_exit_hint_visible() {
            self.shutdown().await;
            return;
        }
        self.handle_interrupt_key();
    }

    /// Port of `handleInterruptKey`.
    pub fn handle_interrupt_key(&mut self) {
        self.clear_escape_repeat();
        self.interrupt_or_clear_input();
        self.show_ctrl_c_exit_hint();
    }

    /// Port of `showCtrlCExitHint`.
    pub fn show_ctrl_c_exit_hint(&mut self) {
        self.ctrl_c_exit_hint_expires_at = now_ms() + Self::EXIT_HINT_DURATION_MS;
        self.ui.request_render();
    }

    /// Port of `clearCtrlCExitHint`.
    pub fn clear_ctrl_c_exit_hint(&mut self, render: bool) {
        if self.ctrl_c_exit_hint_expires_at == 0.0 {
            return;
        }
        self.ctrl_c_exit_hint_expires_at = 0.0;
        if render {
            self.ui.request_render();
        }
    }

    /// Port of `handleCtrlD`.
    pub async fn handle_ctrl_d(&mut self) {
        self.shutdown().await;
    }

    /// Port of `interruptOrClearInput`.
    ///
    /// PARTIAL: the aborts are issued through `AgentConnection`, which belongs to
    /// the agent-connection slice.
    pub fn interrupt_or_clear_input(&mut self) {
        self.show_status("Interrupt requested", "dim");
    }

    /// Port of `handleAgentsBack`.
    pub fn handle_agents_back(&mut self, editor_text: &str) -> bool {
        if !editor_text.trim().is_empty() {
            return false;
        }
        if !self.options.return_to_agents_view {
            self.request_agents_view_blocking();
            return true;
        }
        self.return_to_agents_view(InteractiveModeRunResultType::AgentsView);
        true
    }

    /// Port of `requestAgentsView`.
    fn request_agents_view_blocking(&mut self) {
        if !self.options.return_to_agents_view {
            self.show_status(
                "The agents view needs the daemon; start without --no-daemon to browse sessions",
                "dim",
            );
            return;
        }
        self.return_to_agents_view(InteractiveModeRunResultType::AgentsView);
    }

    /// Port of `returnToAgentsView`.
    ///
    /// PARTIAL: `agentConnection.dispose()` and the signal handlers belong to the
    /// agent-connection slice.
    pub fn return_to_agents_view(&mut self, request: InteractiveModeRunResultType) {
        if self.shutdown_requested || self.agents_view_request.is_some() {
            return;
        }
        self.agents_view_request = Some(request);
        self.shutdown_requested = true;
    }

    /// Port of `browseQueueSelection`.
    pub fn browse_queue_selection(&mut self, editor_text: &str, direction: i64) -> Option<String> {
        let queue = self.get_connection_queue();
        self.queue_selection.move_cursor(&queue, editor_text, direction)
    }

    /// Port of `applyAuthStaleEvent`.
    pub fn apply_auth_stale_event(&self, _provider: &str, _source_tokens: &[String]) {
        self.model_registry().mark_provider_auth_stale(_provider);
    }

    /// Port of `updateConnectionStateFromEvent`.
    pub fn update_connection_state_from_event(&mut self, event: &AgentConnectionSessionEvent) {
        if self.connection_state.is_none() {
            return;
        }
        match event {
            AgentConnectionSessionEvent::AgentStart => {
                self.patch_connection_state(|state| {
                    state.is_streaming = true;
                    state.active_tool_names.clear();
                });
            }
            AgentConnectionSessionEvent::MessageEnd { .. } => {
                self.patch_connection_state(|state| {
                    state.message_count += 1.0;
                });
            }
            AgentConnectionSessionEvent::AgentEnd { .. } => {
                self.patch_connection_state(|state| {
                    state.is_streaming = false;
                    state.active_tool_names.clear();
                });
            }
            AgentConnectionSessionEvent::SessionActionUpdate { actions } => {
                self.patch_connection_state(|state| {
                    state.session_actions = actions.clone();
                });
            }
            AgentConnectionSessionEvent::CompactionStart { .. } => {
                self.patch_connection_state(|state| state.is_compacting = true);
            }
            AgentConnectionSessionEvent::CompactionEnd { .. } => {
                self.patch_connection_state(|state| state.is_compacting = false);
            }
            AgentConnectionSessionEvent::SessionInfoChanged { name } => {
                self.patch_connection_state(|state| state.session_name = name.clone());
            }
            AgentConnectionSessionEvent::ThinkingLevelChanged { level } => {
                self.patch_connection_state(|state| state.thinking_level = level.clone());
            }
            AgentConnectionSessionEvent::ServiceTierChanged { service_tier } => {
                self.patch_connection_state(|state| state.service_tier = *service_tier);
            }
            AgentConnectionSessionEvent::AutoRetryStart { attempt, .. } => {
                self.patch_connection_state(|state| state.retry_attempt = *attempt);
            }
            AgentConnectionSessionEvent::AutoRetryEnd { .. } => {
                self.patch_connection_state(|state| state.retry_attempt = 0.0);
            }
            AgentConnectionSessionEvent::GoalUpdate { goal } => {
                self.patch_connection_state(|state| state.goal = goal.clone());
            }
            AgentConnectionSessionEvent::BashStart { .. } => {
                self.patch_connection_state(|state| state.is_bash_running = true);
            }
            AgentConnectionSessionEvent::BashEnd { .. } => {
                self.patch_connection_state(|state| state.is_bash_running = false);
            }
            AgentConnectionSessionEvent::RecapUpdate { recap } => {
                self.session_recap = recap.clone();
            }
            AgentConnectionSessionEvent::RlmChildUpdate { child } => {
                self.update_subagent_summary(child.clone());
            }
            _ => {}
        }
    }

    /// Port of `handleGoalUpdate`.
    pub fn handle_goal_update(&mut self, goal: &GoalState, terminal_columns: f64) {
        self.sync_goal_tray(goal);
        if self.should_announce_goal_update(goal) {
            let status = self.format_goal_status(goal, terminal_columns);
            self.show_status(&status, "dim");
        } else {
            self.ui.request_render();
        }
    }

    /// Port of `syncGoalTray`.
    pub fn sync_goal_tray(&mut self, _goal: &GoalState) {
        self.ui.request_render();
    }

    /// Port of `updateGoalTrayTimer`.
    pub fn update_goal_tray_timer(&mut self, _goal: &GoalState) {
        // The 1s tray refresh interval is driven by the host timer loop.
    }

    /// Port of `stopGoalTrayTimer`.
    pub fn stop_goal_tray_timer(&mut self) {}

    /// Port of `renderRecap`.
    pub fn render_recap(&mut self) {
        self.recap_container.clear();
        let recap = self
            .session_recap
            .as_ref()
            .map(|recap| recap.trim().to_string())
            .filter(|recap| !recap.is_empty());
        if let Some(recap) = recap {
            self.recap_container
                .add_child(Box::new(super::interactive_mode_services::TruncatedText::new(
                    theme().fg("dim", &format!("Recap: {recap}")),
                    1,
                    0,
                )));
            self.recap_container
                .add_child(Box::new(super::interactive_mode_services::Spacer::new(1)));
        }
        self.ui.request_render();
    }

    /// Port of `renderWidgets`.
    pub fn render_widgets(&mut self) {
        self.render_widget_container(true);
        self.render_widget_container(false);
        self.ui.request_render();
    }

    /// Port of `renderWidgetContainer` (component factories live in other slices).
    pub fn render_widget_container(&mut self, above: bool) {
        let container = if above { &mut self.widget_container_above } else { &mut self.widget_container_below };
        container.clear();
    }

    /// Port of `clearInputBar`.
    pub fn clear_input_bar(&mut self) {
        self.queue_selection.reset();
        self.ui.request_render();
    }

    /// Port of `showError`.
    pub fn show_error(&mut self, message: &str) {
        self.show_status(message, "error");
    }

    /// Port of `showWarning`.
    pub fn show_warning(&mut self, message: &str) {
        self.show_status(message, "warning");
    }

    /// Port of `updateEditorBorderColor`.
    pub fn update_editor_border_color(&mut self) {
        self.ui.request_render();
    }

    /// Port of `getPromptContextContainers`.
    pub fn get_prompt_context_containers(&self) -> Vec<&super::interactive_mode_services::Container> {
        vec![
            &self.widget_container_above,
            &self.recap_container,
            &self.queued_messages_container,
            &self.side_question_container,
            &self.feature_hint_container,
        ]
    }

    /// Port of `getPromptDockComponents`.
    pub fn get_prompt_dock_components(&self) -> Vec<&super::interactive_mode_services::Container> {
        vec![&self.editor_container, &self.footer_slot]
    }

    /// Port of `collectQueueReplaceImages`.
    pub fn collect_queue_replace_images(&self, text: &str) -> Vec<ImageContent> {
        let pending: Vec<(i64, ImageContent)> =
            self.pasted_images.iter().map(|(id, image)| (*id, image.clone())).collect();
        collect_marked_images(&pending, text)
    }

    /// Port of `liveImageMarkerIds`.
    pub fn live_image_marker_ids(&self) -> HashSet<i64> {
        self.pasted_images.keys().copied().collect()
    }

    /// Port of `collectImagesFor`.
    pub fn collect_images_for(&self, text: &str) -> Vec<ImageContent> {
        let pending: Vec<(i64, ImageContent)> =
            self.pasted_images.iter().map(|(id, image)| (*id, image.clone())).collect();
        collect_marked_images(&pending, text)
    }

    /// Port of `hasPastedImagesFor`.
    pub fn has_pasted_images_for(&self, text: &str) -> bool {
        !self.collect_images_for(text).is_empty()
    }

    /// Port of `rememberPastedImage`.
    pub fn remember_pasted_image(&mut self, image: ImageContent, data_bytes: f64) -> i64 {
        let marker_id = self.next_image_marker_id;
        self.next_image_marker_id += 1;
        self.pasted_images.insert(marker_id, image);
        self.evict_pasted_images(data_bytes);
        marker_id
    }

    /// Port of the pasted-image eviction loop (`MAX_PASTED_IMAGE_BYTES`).
    fn evict_pasted_images(&mut self, incoming_bytes: f64) {
        let mut images: Vec<(i64, ImageContent)> = self
            .pasted_images
            .iter()
            .map(|(id, image)| (*id, image.clone()))
            .collect();
        let keep: HashSet<i64> = images.iter().map(|(id, _)| *id).collect();
        evict_images_to_budget(
            &mut images,
            |image: &ImageContent| image.data.len() as f64,
            MAX_PASTED_IMAGE_BYTES - incoming_bytes,
            &keep,
        );
        let retained: HashSet<i64> = images.into_iter().map(|(id, _)| id).collect();
        self.pasted_images.retain(|id, _| retained.contains(id));
    }

    /// Port of `formatImageMarker`.
    pub fn format_image_marker(&self, id: i64) -> String {
        format_image_marker(id as f64)
    }

    /// Port of `remapImageMarkers`.
    pub fn remap_image_markers(&self, text: &str, remaps: &HashMap<i64, i64>) -> String {
        remap_image_markers(text, remaps)
    }
}
