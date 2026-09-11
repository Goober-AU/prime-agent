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

use crate::config::{app_name, APP_TITLE, VERSION};
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
use super::onboarding::{is_onboarding_model_ready, should_run_onboarding, OnboardingStartupState};
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
pub const APP_NAME: &str = app_name();

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

