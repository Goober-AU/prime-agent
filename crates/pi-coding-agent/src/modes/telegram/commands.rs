//! Port of packages/coding-agent/src/modes/telegram/commands.ts

use std::collections::HashSet;
use std::sync::Arc;

use pi_ai::models::supports_fast_mode;
use pi_agent_core::types::ThinkingLevel;

use crate::core::cron_jobs::{format_agent_cron_job, AgentCronJob};
use crate::core::slash_commands::{
    builtin_slash_commands, is_builtin_slash_command_name, parse_refine_command_options, parse_slash_command,
    resolve_builtin_slash_command_name,
};
use crate::core::tools::path_utils::resolve_to_cwd;
use crate::modes::agent_connection::types::*;

/// `SUPPORTED_COMMANDS`.
pub fn supported_commands() -> HashSet<&'static str> {
    [
        "new",
        "resume",
        "model",
        "effort",
        "fast",
        "name",
        "session",
        "context",
        "compact",
        "refine",
        "goal",
        "autonomous",
        "heartbeat",
        "heartbeats",
        "rlm-max-depth",
        "reload",
        "system-prompt",
        "copy",
        "settings",
        "fork",
        "tree",
    ]
    .into_iter()
    .collect()
}

/// `telegramCommandMenu()`.
pub fn telegram_command_menu() -> Vec<TelegramMenuCommand> {
    let descriptions: [(&str, &str); 7] = [
        ("settings", "Show current session settings"),
        (
            "model",
            "List available models or select one with /model provider/model-id",
        ),
        ("effort", "Show or set the reasoning level"),
        ("copy", "Show the last assistant response"),
        ("resume", "List saved sessions or resume one by ID"),
        ("heartbeats", "List configured heartbeats"),
        ("tree", "List branch entries or switch to an entry ID"),
    ];
    let supported = supported_commands();
    let mut menu = vec![
        TelegramMenuCommand {
            command: "help".to_string(),
            description: "Show Prime commands available in Telegram".to_string(),
        },
        TelegramMenuCommand {
            command: "stop".to_string(),
            description: "Interrupt the current turn and clear queued messages".to_string(),
        },
    ];
    for command in builtin_slash_commands() {
        if !supported.contains(command.name.as_str()) {
            continue;
        }
        let description = descriptions
            .iter()
            .find(|(name, _)| *name == command.name)
            .map(|(_, description)| *description)
            .unwrap_or(command.description.as_str());
        menu.push(TelegramMenuCommand {
            command: command.name.replace('-', "_"),
            description: description.chars().take(100).collect(),
        });
    }
    menu
}

/// `{ command: string; description: string }`.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TelegramMenuCommand {
    pub command: String,
    pub description: String,
}

/// `telegramHelp()`.
pub fn telegram_help() -> String {
    let mut lines = vec!["Prime in Telegram".to_string(), String::new()];
    for command in telegram_command_menu() {
        lines.push(format!("/{} \u{2014} {}", command.command, command.description));
    }
    lines.push(String::new());
    lines.push(
        "Aliases such as /clear, /thinking, /rename, and /usage also work. Use /answer for a pending question."
            .to_string(),
    );
    lines.push(
        "Terminal display, login, package, and update commands stay in the Prime terminal. Text messages are queued as follow-ups while Prime is working."
            .to_string(),
    );
    lines.join("\n")
}

/// `parseTelegramCommand(text, botUsername)`.
pub fn parse_telegram_command(text: &str, bot_username: &str) -> Option<ParsedTelegramCommand> {
    let parsed = parse_slash_command(text)?;
    let mut parts = parsed.name.split('@');
    let command = parts.next().unwrap_or_default();
    let addressed_bot = parts.next();
    let extra: Vec<&str> = parts.collect();
    if !extra.is_empty()
        || addressed_bot
            .map(|bot| bot.to_lowercase() != bot_username.to_lowercase())
            .unwrap_or(false)
    {
        return Some(ParsedTelegramCommand {
            name: "ignore".to_string(),
            args: String::new(),
        });
    }
    let hyphenated = command.replace('_', "-");
    let normalized = if is_builtin_slash_command_name(&hyphenated) {
        hyphenated
    } else {
        command.to_string()
    };
    Some(ParsedTelegramCommand {
        name: resolve_builtin_slash_command_name(&normalized),
        args: parsed.args,
    })
}

/// `{ name: string; args: string }`.
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedTelegramCommand {
    pub name: String,
    pub args: String,
}

/// `class TelegramCommands`.
pub struct TelegramCommands {
    connection: Arc<dyn AgentConnection>,
    reply: Arc<dyn Fn(String) + Send + Sync>,
}

impl TelegramCommands {
    pub fn new(connection: Arc<dyn AgentConnection>, reply: Arc<dyn Fn(String) + Send + Sync>) -> Self {
        Self { connection, reply }
    }

    fn reply(&self, text: impl Into<String>) {
        (self.reply)(text.into());
    }

    /// `execute(name, args)`.
    pub async fn execute(&self, name: &str, args: &str) -> Result<(), String> {
        let connection = self.connection.clone();
        match name {
            "start" | "help" => {
                self.reply(telegram_help());
                Ok(())
            }
            "ignore" => Ok(()),
            "stop" | "cancel" => {
                connection.abort_and_clear_queue().await?;
                let _ = connection.abort_compaction().await;
                let _ = connection.abort_retry().await;
                let _ = connection.abort_bash().await;
                self.reply("Interrupted the current turn and cleared queued messages.");
                Ok(())
            }
            "settings" | "session" | "status" => {
                let state = connection.get_state().await?;
                let model = state
                    .model
                    .as_ref()
                    .map(|model| format!("{}/{}", model.provider, model.id))
                    .unwrap_or_else(|| "not selected".to_string());
                let state_text = if state.is_streaming {
                    "working"
                } else if state.is_compacting {
                    "compacting"
                } else {
                    "idle"
                };
                self.reply(format!(
                    "{}\nID: {}\nDirectory: {}\nModel: {}\nEffort: {}\nFast: {}\nState: {}",
                    state.session_name.clone().unwrap_or_else(|| "Prime session".to_string()),
                    state.session_id,
                    state.cwd,
                    model,
                    state.thinking_level.as_str(),
                    if state.service_tier.as_ref().and_then(|tier| tier.as_deref()) == Some("priority") {
                        "on"
                    } else {
                        "off"
                    },
                    state_text
                ));
                Ok(())
            }
            "context" => {
                let stats = connection.get_session_stats().await?;
                let total_messages = stats.get("totalMessages").and_then(serde_json::Value::as_f64).unwrap_or(0.0);
                let tool_calls = stats.get("toolCalls").and_then(serde_json::Value::as_f64).unwrap_or(0.0);
                let tokens_total = stats
                    .get("tokens")
                    .and_then(|tokens| tokens.get("total"))
                    .and_then(serde_json::Value::as_f64)
                    .unwrap_or(0.0);
                let cost = stats.get("cost").and_then(serde_json::Value::as_f64).unwrap_or(0.0);
                let context_usage = stats.get("contextUsage").cloned();
                let mut text = format!(
                    "Messages: {}\nTool calls: {}\nTokens: {}\nCost: ${}",
                    format_number(total_messages),
                    format_number(tool_calls),
                    format_number_with_separators(tokens_total),
                    format_fixed(cost, 4)
                );
                if let Some(context_usage) = context_usage {
                    text.push_str(&format!("\nContext: {context_usage}"));
                }
                self.reply(text);
                Ok(())
            }
            "model" => {
                let models = connection.get_available_models().await?;
                let matches: Vec<&AgentConnectionModel> = models
                    .iter()
                    .filter(|model| format!("{}/{}", model.provider, model.id) == args || model.id == args)
                    .collect();
                if !args.is_empty() && matches.len() == 1 {
                    let model = matches[0];
                    connection.set_model(&model.provider, &model.id).await?;
                    self.reply(format!("Model: {}/{}", model.provider, model.id));
                } else {
                    let filtered: Vec<&AgentConnectionModel> = if args.is_empty() {
                        models.iter().collect()
                    } else {
                        let needle = args.to_lowercase();
                        models
                            .iter()
                            .filter(|model| {
                                format!("{}/{}", model.provider, model.id)
                                    .to_lowercase()
                                    .contains(&needle)
                            })
                            .collect()
                    };
                    let mut lines = vec!["Use /model <provider/model-id>:".to_string()];
                    for model in filtered.iter().take(60) {
                        lines.push(format!("{}/{}", model.provider, model.id));
                    }
                    if filtered.len() > 60 {
                        lines.push("Use /model <search> to narrow the list.".to_string());
                    }
                    self.reply(lines.join("\n"));
                }
                Ok(())
            }
            "effort" => {
                let state = connection.get_state().await?;
                if args.is_empty() {
                    let levels: Vec<&str> = state
                        .available_thinking_levels
                        .iter()
                        .map(|level| level.as_str())
                        .collect();
                    self.reply(format!(
                        "Effort: {}\nUse /effort {}",
                        state.thinking_level.as_str(),
                        levels.join("|")
                    ));
                    return Ok(());
                }
                let level = thinking_level_from_str(args);
                if !level
                    .as_ref()
                    .map(|level| state.available_thinking_levels.contains(level))
                    .unwrap_or(false)
                {
                    let levels: Vec<&str> = state
                        .available_thinking_levels
                        .iter()
                        .map(|level| level.as_str())
                        .collect();
                    return Err(format!("Available effort levels: {}", levels.join(", ")));
                }
                connection.set_thinking_level(level.expect("checked above")).await?;
                self.reply(format!("Effort: {args}"));
                Ok(())
            }
            "fast" => {
                let state = connection.get_state().await?;
                let Some(model) = state.model.as_ref() else {
                    return Err("The current model does not support fast mode.".to_string());
                };
                if !supports_fast_mode(model) {
                    return Err("The current model does not support fast mode.".to_string());
                }
                // TS: `state.serviceTier !== "priority"` - the resolved tier is not priority.
                let service_tier_is_priority =
                    state.service_tier.as_ref().and_then(|tier| tier.as_deref()) == Some("priority");
                let enable = args == "on" || (args.is_empty() && !service_tier_is_priority);
                if !args.is_empty() && args != "on" && args != "off" {
                    return Err("Usage: /fast [on|off]".to_string());
                }
                connection
                    .set_service_tier(Some(Some(
                        if enable { "priority" } else { "default" }.to_string(),
                    )))
                    .await?;
                self.reply(format!("Fast mode: {}", if enable { "on" } else { "off" }));
                Ok(())
            }
            "name" => {
                if !args.is_empty() {
                    connection.set_session_name(args).await?;
                }
                let state = connection.get_state().await?;
                self.reply(format!(
                    "Session name: {}",
                    state.session_name.unwrap_or_else(|| "unnamed".to_string())
                ));
                Ok(())
            }
            "new" => {
                let raw_args = if args.is_empty() {
                    String::new()
                } else {
                    format!(" {args}")
                };
                let options = crate::core::new_session_command::parse_new_session_command(&raw_args)?;
                if connection.new_session(None).await? {
                    self.reply("New session cancelled.");
                    return Ok(());
                }
                if let Some(name) = &options.name {
                    connection.set_session_name(name).await?;
                }
                let state = connection.get_state().await?;
                self.reply(format!("Started session {}.", state.session_id));
                if let Some(prompt) = &options.prompt {
                    connection
                        .prompt(
                            prompt,
                            Some(AgentConnectionPromptOptions {
                                images: None,
                                streaming_behavior: Some("followUp".to_string()),
                                queue_if_busy: Some(true),
                                source: Some("interactive".to_string()),
                            }),
                        )
                        .await?;
                }
                Ok(())
            }
            "resume" => {
                if args.ends_with(".jsonl") {
                    let cwd = connection.get_state().await?.cwd;
                    let path = resolve_to_cwd(args, &cwd);
                    let cancelled = connection.switch_session(&path, None).await?;
                    self.reply(if cancelled {
                        "Resume cancelled.".to_string()
                    } else {
                        format!("Resumed {path}.")
                    });
                    return Ok(());
                }
                let sessions = connection.list_saved_sessions("all").await?;
                let exact: Vec<&AgentConnectionSavedSessionInfo> = sessions
                    .iter()
                    .filter(|session| {
                        session.id == args || session.path == args || session.name.as_deref() == Some(args)
                    })
                    .collect();
                let matches: Vec<&AgentConnectionSavedSessionInfo> = if exact.is_empty() {
                    sessions
                        .iter()
                        .filter(|session| !args.is_empty() && session.id.starts_with(args))
                        .collect()
                } else {
                    exact
                };
                if !args.is_empty() && matches.len() == 1 {
                    let session = matches[0];
                    let cancelled = connection.switch_session(&session.path, None).await?;
                    self.reply(if cancelled {
                        "Resume cancelled.".to_string()
                    } else {
                        format!(
                            "Resumed {}.",
                            session
                                .name
                                .clone()
                                .unwrap_or_else(|| session.id.clone())
                        )
                    });
                } else {
                    let mut lines = vec!["Use /resume <session-id>:".to_string()];
                    for session in sessions.iter().take(30) {
                        let label = session
                            .name
                            .clone()
                            .or_else(|| {
                                let first: String = session.first_message.chars().take(80).collect();
                                if first.is_empty() {
                                    None
                                } else {
                                    Some(first)
                                }
                            })
                            .unwrap_or_else(|| "untitled".to_string());
                        lines.push(format!("{} \u{2014} {}", session.id, label));
                    }
                    self.reply(lines.join("\n"));
                }
                Ok(())
            }
            "compact" => {
                self.reply("Compacting context\u{2026}");
                let result = connection
                    .compact(if args.is_empty() { None } else { Some(args) })
                    .await?;
                let tokens_before = result
                    .get("tokensBefore")
                    .and_then(serde_json::Value::as_f64)
                    .unwrap_or(0.0);
                self.reply(format!(
                    "Context compacted ({} tokens before compaction).",
                    format_number_with_separators(tokens_before)
                ));
                Ok(())
            }
            "refine" => {
                self.reply("Refining session harness\u{2026}");
                let options = parse_refine_command_options(args)?;
                connection
                    .refine(serde_json::to_value(&options).unwrap_or(serde_json::Value::Null))
                    .await?;
                self.reply("Refinement completed.");
                Ok(())
            }
            "goal" | "autonomous" => {
                if args.chars().any(|ch| matches!(ch, '\r' | '\n' | '\u{2028}' | '\u{2029}')) {
                    return Err(format!("/{name} requires a single-line command."));
                }
                let text = if args.is_empty() {
                    format!("/{name}")
                } else {
                    format!("/{name} {args}")
                };
                connection
                    .prompt(
                        &text,
                        Some(AgentConnectionPromptOptions {
                            images: None,
                            streaming_behavior: Some("followUp".to_string()),
                            queue_if_busy: Some(true),
                            source: Some("interactive".to_string()),
                        }),
                    )
                    .await?;
                Ok(())
            }
            "heartbeat" => {
                let command = crate::core::cron_jobs::parse_heartbeat_command(args)?;
                match command {
                    crate::core::cron_jobs::ParsedHeartbeatCommand::Status => {
                        let job = connection.get_heartbeat().await?;
                        self.reply(match job {
                            Some(job) => format_heartbeat_value(&job),
                            None => "No heartbeat is configured.".to_string(),
                        });
                    }
                    crate::core::cron_jobs::ParsedHeartbeatCommand::Set {
                        schedule,
                        instruction,
                        delivery_mode,
                    } => {
                        let job = connection
                            .set_heartbeat(&schedule, &instruction, delivery_mode.as_deref())
                            .await?;
                        self.reply(format_heartbeat_value(&job));
                    }
                    crate::core::cron_jobs::ParsedHeartbeatCommand::Pause
                    | crate::core::cron_jobs::ParsedHeartbeatCommand::Resume
                    | crate::core::cron_jobs::ParsedHeartbeatCommand::Clear => {
                        let action = match command {
                            crate::core::cron_jobs::ParsedHeartbeatCommand::Pause => "pause",
                            crate::core::cron_jobs::ParsedHeartbeatCommand::Resume => "resume",
                            _ => "clear",
                        };
                        let job = connection
                            .update_heartbeat(serde_json::Value::String(action.to_string()))
                            .await?;
                        self.reply(match job {
                            Some(job) => format_heartbeat_value(&job),
                            None => "Heartbeat cleared.".to_string(),
                        });
                    }
                }
                Ok(())
            }
            "heartbeats" => {
                let heartbeats = connection.list_heartbeats().await?;
                let text = heartbeats
                    .iter()
                    .map(|heartbeat| format_heartbeat_value(&heartbeat.job))
                    .collect::<Vec<String>>()
                    .join("\n");
                self.reply(if text.is_empty() { "No heartbeats.".to_string() } else { text });
                Ok(())
            }
            "rlm-max-depth" => {
                if !args.is_empty() {
                    if !args.chars().all(|ch| ch.is_ascii_digit()) {
                        return Err("Usage: /rlm_max_depth [non-negative integer]".to_string());
                    }
                    connection
                        .set_rlm_max_depth(args.parse::<f64>().unwrap_or_default(), None)
                        .await?;
                }
                let status = connection.get_rlm_max_depth_status().await?;
                self.reply(format!(
                    "RLM max depth: {}",
                    status
                        .get("maxDepth")
                        .and_then(serde_json::Value::as_f64)
                        .map(format_number)
                        .unwrap_or_default()
                ));
                Ok(())
            }
            "reload" => {
                connection.reload().await?;
                self.reply("Reloaded Prime resources.");
                Ok(())
            }
            "copy" => {
                let text = connection.get_last_assistant_text().await?;
                self.reply(text.unwrap_or_else(|| "No assistant response yet.".to_string()));
                Ok(())
            }
            "system-prompt" => {
                let prompt = connection.get_system_prompt().await?;
                self.reply(prompt);
                Ok(())
            }
            "fork" => {
                let messages = connection.get_user_messages_for_forking().await?;
                if args.is_empty() {
                    let mut lines = vec!["Use /fork <entry-id>:".to_string()];
                    for message in messages.iter().rev().take(20).rev() {
                        lines.push(format!(
                            "{} \u{2014} {}",
                            message.entry_id,
                            message.text.chars().take(100).collect::<String>()
                        ));
                    }
                    self.reply(lines.join("\n"));
                    return Ok(());
                }
                if !messages.iter().any(|message| message.entry_id == args) {
                    return Err("Choose an entry ID from /fork.".to_string());
                }
                let result = connection.fork(args, None).await?;
                self.reply(if result
                    .get("cancelled")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false)
                {
                    "Fork cancelled."
                } else {
                    "Created session fork."
                });
                Ok(())
            }
            "tree" => {
                if !args.is_empty() {
                    let result = connection.navigate_tree(args, None).await?;
                    self.reply(if result.cancelled {
                        "Navigation cancelled."
                    } else {
                        "Switched session branch."
                    });
                    return Ok(());
                }
                let tree = connection.get_session_tree().await?;
                let messages = connection.get_user_messages_for_forking().await?;
                let recent: Vec<String> = messages
                    .iter()
                    .rev()
                    .take(20)
                    .rev()
                    .map(|message| {
                        format!(
                            "{} \u{2014} {}",
                            message.entry_id,
                            message.text.chars().take(100).collect::<String>()
                        )
                    })
                    .collect();
                self.reply(format!(
                    "Current entry: {}\nUse /tree <entry-id>. Recent user entries:\n{}",
                    tree.leaf_id.unwrap_or_else(|| "root".to_string()),
                    recent.join("\n")
                ));
                Ok(())
            }
            "telegram" => {
                self.reply("Configure the Telegram connection with /telegram in the Prime terminal.");
                Ok(())
            }
            _ => {
                if is_builtin_slash_command_name(name) {
                    self.reply(format!("/{name} needs the Prime terminal. Use /help for Telegram commands."));
                    return Ok(());
                }
                let commands = connection.get_commands().await?;
                if commands
                    .iter()
                    .any(|command| command.name == name || command.registered_name.as_deref() == Some(name))
                {
                    let text = if args.is_empty() {
                        format!("/{name}")
                    } else {
                        format!("/{name} {args}")
                    };
                    connection
                        .prompt(
                            &text,
                            Some(AgentConnectionPromptOptions {
                                images: None,
                                streaming_behavior: Some("followUp".to_string()),
                                queue_if_busy: Some(true),
                                source: Some("interactive".to_string()),
                            }),
                        )
                        .await?;
                } else {
                    self.reply(format!("Unknown command /{name}. Use /help."));
                }
                Ok(())
            }
        }
    }
}

/// The connection trait carries heartbeat jobs as wire `Value`s while the TS seam
/// is typed (`AgentCronJob`), so decode through the canonical `Deserialize` impl
/// before formatting. TS reads missing fields as `undefined`; the port's
/// all-defaulted struct renders them the same way.
fn format_heartbeat_value(value: &serde_json::Value) -> String {
    format_agent_cron_job(&serde_json::from_value::<AgentCronJob>(value.clone()).unwrap_or_default())
}

fn thinking_level_from_str(value: &str) -> Option<ThinkingLevel> {
    match value {
        "off" => Some(ThinkingLevel::Off),
        "minimal" => Some(ThinkingLevel::Minimal),
        "low" => Some(ThinkingLevel::Low),
        "medium" => Some(ThinkingLevel::Medium),
        "high" => Some(ThinkingLevel::High),
        "xhigh" => Some(ThinkingLevel::Xhigh),
        "max" => Some(ThinkingLevel::Max),
        _ => None,
    }
}

/// `Number#toLocaleString()` with the default (en-US) grouping.
fn format_number_with_separators(value: f64) -> String {
    let text = format_number(value);
    let (integer, fraction) = match text.split_once('.') {
        Some((integer, fraction)) => (integer.to_string(), Some(fraction.to_string())),
        None => (text.clone(), None),
    };
    let negative = integer.starts_with('-');
    let digits = integer.trim_start_matches('-');
    let mut grouped = String::new();
    for (index, ch) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(ch);
    }
    let mut result = format!("{}{}", if negative { "-" } else { "" }, grouped);
    if let Some(fraction) = fraction {
        result.push('.');
        result.push_str(&fraction);
    }
    result
}

/// JavaScript number stringification.
fn format_number(value: f64) -> String {
    if value.fract() == 0.0 && value.abs() < 9_007_199_254_740_992.0 {
        format!("{}", value as i64)
    } else {
        format!("{value}")
    }
}

/// `Number#toFixed(digits)`.
fn format_fixed(value: f64, digits: usize) -> String {
    format!("{value:.digits$}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_menu_starts_with_help_and_stop() {
        let menu = telegram_command_menu();
        assert_eq!(menu[0].command, "help");
        assert_eq!(menu[1].command, "stop");
        assert!(menu.iter().any(|command| command.command == "rlm_max_depth"));
        assert!(menu.iter().all(|command| !command.command.contains('-')));
        assert!(menu.iter().all(|command| command.description.chars().count() <= 100));
    }

    #[test]
    fn help_lists_every_menu_entry() {
        let help = telegram_help();
        assert!(help.starts_with("Prime in Telegram\n\n/help \u{2014} "));
        assert!(help.contains("/stop \u{2014} "));
        assert!(help.ends_with("Text messages are queued as follow-ups while Prime is working."));
    }

    #[test]
    fn commands_addressed_to_another_bot_are_ignored() {
        let parsed = parse_telegram_command("/help@other_bot", "prime_bot").unwrap();
        assert_eq!(parsed.name, "ignore");
        let parsed = parse_telegram_command("/help@prime_bot", "prime_bot").unwrap();
        assert_eq!(parsed.name, "help");
        assert!(parse_telegram_command("plain text", "prime_bot").is_none());
    }

    #[test]
    fn underscores_normalize_to_hyphenated_builtins() {
        let parsed = parse_telegram_command("/rlm_max_depth 3", "prime_bot").unwrap();
        assert_eq!(parsed.name, "rlm-max-depth");
        assert_eq!(parsed.args, "3");
        let parsed = parse_telegram_command("/clear", "prime_bot").unwrap();
        assert_eq!(parsed.name, "new");
    }

    #[test]
    fn number_formatting_matches_javascript() {
        assert_eq!(format_number_with_separators(1234.0), "1,234");
        assert_eq!(format_number_with_separators(1234567.0), "1,234,567");
        assert_eq!(format_number_with_separators(-1000.0), "-1,000");
        assert_eq!(format_number(1.5), "1.5");
        assert_eq!(format_fixed(1.23456, 4), "1.2346");
        assert_eq!(format_fixed(1.0, 4), "1.0000");
    }
}
