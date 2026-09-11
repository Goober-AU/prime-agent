//! Port of packages/coding-agent/src/cli/args.ts
//!
//! CLI argument parsing and help display.

use std::collections::BTreeMap;

use pi_agent_core::types::ThinkingLevel;

use crate::config::APP_NAME;
use crate::core::thinking_levels::THINKING_LEVELS;

pub type Mode = &'static str;

pub const MODES: [&str; 5] = ["text", "json", "rpc", "acp", "daemon"];

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Args {
    pub provider: Option<String>,
    pub model: Option<String>,
    pub api_key: Option<String>,
    pub cwd: Option<String>,
    pub system_prompt: Option<String>,
    pub append_system_prompt: Option<Vec<String>>,
    pub thinking: Option<ThinkingLevel>,
    pub continue_: Option<bool>,
    /// `true` = resume latest, string = resume selector.
    pub resume: Option<ResumeValue>,
    pub help: Option<bool>,
    pub version: Option<bool>,
    pub mode: Option<String>,
    pub daemon_socket: Option<String>,
    pub no_session: Option<bool>,
    pub fork: Option<String>,
    pub session_dir: Option<String>,
    pub models: Option<Vec<String>>,
    pub tools: Option<Vec<String>>,
    pub no_tools: Option<bool>,
    pub no_builtin_tools: Option<bool>,
    pub extensions: Option<Vec<String>>,
    pub no_extensions: Option<bool>,
    pub print: Option<bool>,
    pub export: Option<String>,
    pub no_skills: Option<bool>,
    pub skills: Option<Vec<String>>,
    pub prompt_templates: Option<Vec<String>>,
    pub no_prompt_templates: Option<bool>,
    pub themes: Option<Vec<String>>,
    pub no_themes: Option<bool>,
    pub no_context_files: Option<bool>,
    pub autonomous: Option<bool>,
    pub autonomous_gates: Option<Vec<String>>,
    pub autonomous_gate_retries: Option<i64>,
    pub autonomous_gate_timeout_ms: Option<i64>,
    pub autonomous_max_continuations: Option<i64>,
    pub autonomous_max_turns: Option<i64>,
    pub autonomous_max_tokens: Option<i64>,
    pub autonomous_timeout_ms: Option<i64>,
    pub goal: Option<String>,
    pub goal_token_budget: Option<i64>,
    /// `true` = list all, string = search pattern.
    pub list_models: Option<ListModelsValue>,
    pub offline: Option<bool>,
    pub verbose: Option<bool>,
    pub messages: Vec<String>,
    pub file_args: Vec<String>,
    /// Unknown flags (potentially extension flags) - map of flag name to value.
    ///
    /// `BTreeMap` keeps iteration deterministic; the TypeScript `Map` preserves
    /// insertion order and only lookups are observable here.
    pub unknown_flags: BTreeMap<String, UnknownFlagValue>,
    pub diagnostics: Vec<Diagnostic>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ResumeValue {
    Latest,
    Selector(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum ListModelsValue {
    All,
    Search(String),
}

/// `boolean | string` flag value: `true` is the no-value form.
#[derive(Debug, Clone, PartialEq)]
pub enum UnknownFlagValue {
    Flag,
    Value(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Diagnostic {
    pub type_: DiagnosticType,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticType {
    Warning,
    Error,
}

impl DiagnosticType {
    pub fn as_str(self) -> &'static str {
        match self {
            DiagnosticType::Warning => "warning",
            DiagnosticType::Error => "error",
        }
    }
}

pub const REMOVED_BUILTIN_TOOL_NAMES: [&str; 5] = ["read", "write", "grep", "find", "ls"];
pub const BUILTIN_TOOL_NAMES: [&str; 1] = ["ipython"];

pub const INTERNAL_RUNTIME_COMMAND_MARKER: &str = "\u{0}prime-agent-runtime-command";

pub fn is_valid_thinking_level(level: &str) -> bool {
    THINKING_LEVELS.contains(&level)
}

#[allow(dead_code)]
const _: fn(&str) -> bool = is_valid_thinking_level;

pub fn parse_args(args: &[String]) -> Args {
    let mut result = Args::default();

    let mut end_of_options = false;
    let internal_runtime_command = args.first().map(String::as_str) == Some(INTERNAL_RUNTIME_COMMAND_MARKER);
    let first_arg_index = if internal_runtime_command { 1 } else { 0 };

    let mut i = first_arg_index;
    while i < args.len() {
        let arg = args[i].clone();

        // POSIX end-of-options: everything after a standalone "--" is a positional
        // message, even if it starts with a dash (e.g. a Markdown-bullet prompt).
        if end_of_options {
            result.messages.push(arg);
            i += 1;
            continue;
        }
        if arg == "--" {
            end_of_options = true;
            i += 1;
            continue;
        }

        if arg == "--help" || arg == "-h" {
            result.help = Some(true);
        } else if arg == "--version" || arg == "-v" {
            result.version = Some(true);
        } else if arg == "--mode" && i + 1 < args.len() {
            i += 1;
            let mode = args[i].clone();
            if MODES.contains(&mode.as_str()) {
                result.mode = Some(mode);
            }
        } else if arg == "--daemon-socket" && i + 1 < args.len() {
            i += 1;
            result.daemon_socket = Some(args[i].clone());
        } else if arg == "--continue" || arg == "-c" {
            result.continue_ = Some(true);
        } else if arg == "--resume" || arg == "-r" {
            let next = args.get(i + 1).cloned();
            match next {
                Some(next) if !next.starts_with('-') && !next.starts_with('@') => {
                    if next.is_empty() {
                        result.resume = Some(ResumeValue::Latest);
                    } else {
                        result.resume = Some(ResumeValue::Selector(next));
                    }
                    i += 1;
                }
                _ => {
                    result.resume = Some(ResumeValue::Latest);
                }
            }
        } else if let Some(value) = arg.strip_prefix("--resume=") {
            if value.is_empty() {
                result.resume = Some(ResumeValue::Latest);
            } else {
                result.resume = Some(ResumeValue::Selector(value.to_string()));
            }
        } else if arg == "--provider" && i + 1 < args.len() {
            i += 1;
            result.provider = Some(args[i].clone());
        } else if arg == "--model" && i + 1 < args.len() {
            i += 1;
            result.model = Some(args[i].clone());
        } else if arg == "--api-key" && i + 1 < args.len() {
            i += 1;
            result.api_key = Some(args[i].clone());
        } else if arg == "--cwd" && i + 1 < args.len() {
            i += 1;
            result.cwd = Some(args[i].clone());
        } else if arg == "--system-prompt" && i + 1 < args.len() {
            i += 1;
            result.system_prompt = Some(args[i].clone());
        } else if arg == "--append-system-prompt" && i + 1 < args.len() {
            i += 1;
            let value = args[i].clone();
            result
                .append_system_prompt
                .get_or_insert_with(Vec::new)
                .push(value);
        } else if arg == "--no-session" {
            result.no_session = Some(true);
        } else if arg == "--fork" && i + 1 < args.len() {
            i += 1;
            result.fork = Some(args[i].clone());
        } else if arg == "--session-dir" && i + 1 < args.len() {
            i += 1;
            result.session_dir = Some(args[i].clone());
        } else if arg == "--models" && i + 1 < args.len() {
            i += 1;
            result.models = Some(args[i].split(',').map(|s| s.trim().to_string()).collect());
        } else if arg == "--no-tools" || arg == "-nt" {
            result.no_tools = Some(true);
        } else if arg == "--no-builtin-tools" || arg == "-nbt" {
            result.no_builtin_tools = Some(true);
        } else if (arg == "--tools" || arg == "-t") && i + 1 < args.len() {
            i += 1;
            let tools: Vec<String> = args[i]
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|name| !name.is_empty())
                .collect();
            let removed_tools: Vec<String> = tools
                .iter()
                .filter(|name| REMOVED_BUILTIN_TOOL_NAMES.contains(&name.as_str()))
                .cloned()
                .collect();
            result.tools = Some(tools);
            if !removed_tools.is_empty() {
                result.diagnostics.push(Diagnostic {
                    type_: DiagnosticType::Error,
                    message: format!(
                        "Unknown built-in tool(s): {}. Available built-in tools: {}",
                        removed_tools.join(", "),
                        BUILTIN_TOOL_NAMES.join(", ")
                    ),
                });
            }
        } else if arg == "--thinking" && i + 1 < args.len() {
            i += 1;
            let level = args[i].clone();
            match thinking_level_from_str(&level) {
                Some(level) => {
                    result.thinking = Some(level);
                }
                None => {
                    result.diagnostics.push(Diagnostic {
                        type_: DiagnosticType::Warning,
                        message: format!(
                            "Invalid thinking level \"{}\". Valid values: {}",
                            level,
                            THINKING_LEVELS.join(", ")
                        ),
                    });
                }
            }
        } else if arg == "--print" || arg == "-p" {
            result.print = Some(true);
            if let Some(next) = args.get(i + 1) {
                if !next.starts_with('@') && (!next.starts_with('-') || next.starts_with("---")) {
                    result.messages.push(next.clone());
                    i += 1;
                }
            }
        } else if arg == "--export" {
            if !internal_runtime_command {
                result.diagnostics.push(Diagnostic {
                    type_: DiagnosticType::Error,
                    message: format!(
                        "--export was removed. Use \"{} session export <file> [output]\".",
                        APP_NAME
                    ),
                });
                if i + 1 < args.len() && !args[i + 1].starts_with('-') {
                    i += 1;
                }
            } else if i + 1 < args.len() {
                i += 1;
                result.export = Some(args[i].clone());
            } else {
                result.diagnostics.push(Diagnostic {
                    type_: DiagnosticType::Error,
                    message: "--export requires a value".to_string(),
                });
            }
        } else if arg.starts_with("--export=") {
            result.diagnostics.push(Diagnostic {
                type_: DiagnosticType::Error,
                message: format!(
                    "--export was removed. Use \"{} session export <file> [output]\".",
                    APP_NAME
                ),
            });
        } else if (arg == "--extension" || arg == "-e") && i + 1 < args.len() {
            i += 1;
            let value = args[i].clone();
            result.extensions.get_or_insert_with(Vec::new).push(value);
        } else if arg == "--no-extensions" || arg == "-ne" {
            result.no_extensions = Some(true);
        } else if arg == "--skill" && i + 1 < args.len() {
            i += 1;
            let value = args[i].clone();
            result.skills.get_or_insert_with(Vec::new).push(value);
        } else if arg == "--prompt-template" && i + 1 < args.len() {
            i += 1;
            let value = args[i].clone();
            result.prompt_templates.get_or_insert_with(Vec::new).push(value);
        } else if arg == "--theme" && i + 1 < args.len() {
            i += 1;
            let value = args[i].clone();
            result.themes.get_or_insert_with(Vec::new).push(value);
        } else if arg == "--no-skills" || arg == "-ns" {
            result.no_skills = Some(true);
        } else if arg == "--no-prompt-templates" || arg == "-np" {
            result.no_prompt_templates = Some(true);
        } else if arg == "--no-themes" {
            result.no_themes = Some(true);
        } else if arg == "--no-context-files" || arg == "-nc" {
            result.no_context_files = Some(true);
        } else if arg == "--autonomous" {
            result.autonomous = Some(true);
        } else if arg == "--autonomous-gate" {
            result.autonomous = Some(true);
            if has_required_option_value(args, i, &arg, &mut result) {
                i += 1;
                let value = args[i].clone();
                result.autonomous_gates.get_or_insert_with(Vec::new).push(value);
            }
        } else if arg == "--autonomous-gate-retries" {
            result.autonomous = Some(true);
            if has_required_option_value(args, i, &arg, &mut result) {
                i += 1;
                result.autonomous_gate_retries = parse_positive_int(&args[i], &arg, &mut result);
            }
        } else if arg == "--autonomous-gate-timeout-ms" {
            result.autonomous = Some(true);
            if has_required_option_value(args, i, &arg, &mut result) {
                i += 1;
                result.autonomous_gate_timeout_ms = parse_positive_int(&args[i], &arg, &mut result);
            }
        } else if arg == "--autonomous-max-continuations" {
            result.autonomous = Some(true);
            if has_required_option_value(args, i, &arg, &mut result) {
                i += 1;
                result.autonomous_max_continuations = parse_positive_int(&args[i], &arg, &mut result);
            }
        } else if arg == "--autonomous-max-turns" {
            result.autonomous = Some(true);
            if has_required_option_value(args, i, &arg, &mut result) {
                i += 1;
                result.autonomous_max_turns = parse_positive_int(&args[i], &arg, &mut result);
            }
        } else if arg == "--autonomous-max-tokens" {
            result.autonomous = Some(true);
            if has_required_option_value(args, i, &arg, &mut result) {
                i += 1;
                result.autonomous_max_tokens = parse_positive_int(&args[i], &arg, &mut result);
            }
        } else if arg == "--autonomous-timeout-ms" {
            result.autonomous = Some(true);
            if has_required_option_value(args, i, &arg, &mut result) {
                i += 1;
                result.autonomous_timeout_ms = parse_positive_int(&args[i], &arg, &mut result);
            }
        } else if arg == "--goal" {
            if has_required_option_value(args, i, &arg, &mut result) {
                i += 1;
                let value = args[i].clone();
                if value.trim().is_empty() {
                    result.diagnostics.push(Diagnostic {
                        type_: DiagnosticType::Error,
                        message: "--goal requires a non-empty objective".to_string(),
                    });
                } else {
                    result.goal = Some(value);
                }
            }
        } else if arg == "--goal-token-budget" {
            if has_required_option_value(args, i, &arg, &mut result) {
                i += 1;
                result.goal_token_budget = parse_positive_int(&args[i], &arg, &mut result);
            }
        } else if arg == "--list-models" {
            let has_search = args
                .get(i + 1)
                .map(|next| !next.starts_with('-') && !next.starts_with('@'))
                .unwrap_or(false);
            if !internal_runtime_command {
                result.diagnostics.push(Diagnostic {
                    type_: DiagnosticType::Error,
                    message: format!(
                        "--list-models was removed. Use \"{} model list [search]\".",
                        APP_NAME
                    ),
                });
                if has_search {
                    i += 1;
                }
            } else if has_search {
                i += 1;
                result.list_models = Some(ListModelsValue::Search(args[i].clone()));
            } else {
                result.list_models = Some(ListModelsValue::All);
            }
        } else if arg.starts_with("--list-models=") {
            result.diagnostics.push(Diagnostic {
                type_: DiagnosticType::Error,
                message: format!(
                    "--list-models was removed. Use \"{} model list [search]\".",
                    APP_NAME
                ),
            });
        } else if arg == "--verbose" {
            result.verbose = Some(true);
        } else if arg == "--offline" {
            result.offline = Some(true);
        } else if let Some(rest) = arg.strip_prefix('@') {
            result.file_args.push(rest.to_string()); // Remove @ prefix
        } else if arg.starts_with("--") {
            match arg.find('=') {
                Some(eq_index) => {
                    result.unknown_flags.insert(
                        arg[2..eq_index].to_string(),
                        UnknownFlagValue::Value(arg[eq_index + 1..].to_string()),
                    );
                }
                None => {
                    let flag_name = arg[2..].to_string();
                    let next = args.get(i + 1).cloned();
                    match next {
                        Some(next) if !next.starts_with('-') && !next.starts_with('@') => {
                            result
                                .unknown_flags
                                .insert(flag_name, UnknownFlagValue::Value(next));
                            i += 1;
                        }
                        _ => {
                            result.unknown_flags.insert(flag_name, UnknownFlagValue::Flag);
                        }
                    }
                }
            }
        } else if arg.starts_with('-') && !arg.starts_with("--") {
            result.diagnostics.push(Diagnostic {
                type_: DiagnosticType::Error,
                message: format!("Unknown option: {}", arg),
            });
        } else if !arg.starts_with('-') {
            result.messages.push(arg);
        }

        i += 1;
    }

    if result.goal_token_budget.is_some() && result.goal.is_none() {
        result.diagnostics.push(Diagnostic {
            type_: DiagnosticType::Error,
            message: "--goal-token-budget requires --goal".to_string(),
        });
    }

    result
}

/// Maps a thinking-level string to the shared enum; unknown strings stay `None`
/// so the caller can report the invalid-value diagnostic. `THINKING_LEVELS`
/// lists exactly these strings, so `isValidThinkingLevel` and a successful
/// mapping agree.
fn thinking_level_from_str(level: &str) -> Option<ThinkingLevel> {
    match level {
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

fn has_required_option_value(args: &[String], index: usize, flag: &str, result: &mut Args) -> bool {
    match args.get(index + 1) {
        None => {
            result.diagnostics.push(Diagnostic {
                type_: DiagnosticType::Error,
                message: format!("{} requires a value", flag),
            });
            false
        }
        Some(next) if next.starts_with("--") => {
            result.diagnostics.push(Diagnostic {
                type_: DiagnosticType::Error,
                message: format!("{} requires a value", flag),
            });
            false
        }
        Some(_) => true,
    }
}

fn parse_positive_int(value: &str, flag: &str, result: &mut Args) -> Option<i64> {
    let parsed = parse_js_number(value);
    match parsed {
        Some(parsed) if parsed.fract() == 0.0 && parsed > 0.0 => Some(parsed as i64),
        _ => {
            result.diagnostics.push(Diagnostic {
                type_: DiagnosticType::Error,
                message: format!("{} must be a positive integer", flag),
            });
            None
        }
    }
}

/// JavaScript `Number(value)`: trims whitespace, accepts decimal and hex
/// literals, and rejects empty strings and non-numeric text.
fn parse_js_number(value: &str) -> Option<f64> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Some(0.0);
    }
    if let Some(hex) = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
    {
        return i64::from_str_radix(hex, 16).ok().map(|value| value as f64);
    }
    if trimmed == "Infinity" || trimmed == "+Infinity" {
        return Some(f64::INFINITY);
    }
    if trimmed == "-Infinity" {
        return Some(f64::NEG_INFINITY);
    }
    trimmed.parse::<f64>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Args {
        parse_args(&args.iter().map(|arg| arg.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn parses_modes_and_positionals() {
        let args = parse(&["--mode", "json", "hello", "world"]);
        assert_eq!(args.mode.as_deref(), Some("json"));
        assert_eq!(args.messages, vec!["hello".to_string(), "world".to_string()]);
    }

    #[test]
    fn ignores_an_unknown_mode() {
        let args = parse(&["--mode", "bogus"]);
        assert_eq!(args.mode, None);
    }

    #[test]
    fn end_of_options_makes_everything_positional() {
        let args = parse(&["--", "--mode", "json"]);
        assert_eq!(args.messages, vec!["--mode".to_string(), "json".to_string()]);
        assert_eq!(args.mode, None);
    }

    #[test]
    fn resume_without_a_value_is_latest() {
        let args = parse(&["--resume"]);
        assert_eq!(args.resume, Some(ResumeValue::Latest));
        let args = parse(&["--resume", "abc"]);
        assert_eq!(args.resume, Some(ResumeValue::Selector("abc".to_string())));
        let args = parse(&["--resume="]);
        assert_eq!(args.resume, Some(ResumeValue::Latest));
        let args = parse(&["--resume=", "x"]);
        assert_eq!(args.resume, Some(ResumeValue::Latest));
    }

    #[test]
    fn resume_does_not_consume_flags_or_file_args() {
        let args = parse(&["--resume", "--json"]);
        assert_eq!(args.resume, Some(ResumeValue::Latest));
        assert_eq!(args.unknown_flags.get("json"), Some(&UnknownFlagValue::Flag));
    }

    #[test]
    fn removed_builtin_tools_report_an_error() {
        let args = parse(&["--tools", "read,ipython"]);
        assert_eq!(args.tools.as_ref().unwrap().len(), 2);
        assert_eq!(args.diagnostics.len(), 1);
        assert_eq!(
            args.diagnostics[0].message,
            "Unknown built-in tool(s): read. Available built-in tools: ipython"
        );
    }

    #[test]
    fn invalid_thinking_level_warns() {
        let args = parse(&["--thinking", "bogus"]);
        assert_eq!(args.thinking, None);
        assert_eq!(args.diagnostics[0].type_, DiagnosticType::Warning);
        assert_eq!(
            args.diagnostics[0].message,
            "Invalid thinking level \"bogus\". Valid values: off, minimal, low, medium, high, xhigh, max"
        );
    }

    #[test]
    fn print_consumes_a_plain_message() {
        let args = parse(&["-p", "hello"]);
        assert_eq!(args.print, Some(true));
        assert_eq!(args.messages, vec!["hello".to_string()]);
    }

    #[test]
    fn print_keeps_dash_prefixed_values() {
        let args = parse(&["-p", "-not-a-message"]);
        assert_eq!(args.messages, Vec::<String>::new());
        assert_eq!(
            args.diagnostics[0].message,
            "Unknown option: -not-a-message"
        );
    }

    #[test]
    fn export_is_removed_outside_the_internal_runtime() {
        let args = parse(&["--export", "file.jsonl"]);
        assert_eq!(
            args.diagnostics[0].message,
            "--export was removed. Use \"prime-agent session export <file> [output]\"."
        );
        assert_eq!(args.export, None);
    }

    #[test]
    fn export_still_parses_for_the_internal_runtime_command() {
        let args = parse_args(
            &[
                INTERNAL_RUNTIME_COMMAND_MARKER.to_string(),
                "--export".to_string(),
                "file.jsonl".to_string(),
            ]
            .to_vec(),
        );
        assert_eq!(args.export.as_deref(), Some("file.jsonl"));
        assert!(args.diagnostics.is_empty());
    }

    #[test]
    fn unknown_long_flags_keep_their_value_shape() {
        let args = parse(&["--my-flag", "value", "--other", "--third=1"]);
        assert_eq!(
            args.unknown_flags.get("my-flag"),
            Some(&UnknownFlagValue::Value("value".to_string()))
        );
        assert_eq!(args.unknown_flags.get("other"), Some(&UnknownFlagValue::Flag));
        assert_eq!(
            args.unknown_flags.get("third"),
            Some(&UnknownFlagValue::Value("1".to_string()))
        );
    }

    #[test]
    fn file_args_strip_the_at_prefix() {
        let args = parse(&["@notes.txt"]);
        assert_eq!(args.file_args, vec!["notes.txt".to_string()]);
    }

    #[test]
    fn autonomous_flags_validate_positive_integers() {
        let args = parse(&["--autonomous-gate-retries", "0"]);
        assert_eq!(args.autonomous, Some(true));
        assert_eq!(args.autonomous_gate_retries, None);
        assert_eq!(
            args.diagnostics[0].message,
            "--autonomous-gate-retries must be a positive integer"
        );

        let args = parse(&["--autonomous-gate-retries", "3"]);
        assert_eq!(args.autonomous_gate_retries, Some(3));
        assert!(args.diagnostics.is_empty());
    }

    #[test]
    fn autonomous_flags_require_a_value() {
        let args = parse(&["--autonomous-gate"]);
        assert_eq!(args.autonomous, Some(true));
        assert_eq!(args.diagnostics[0].message, "--autonomous-gate requires a value");
    }

    #[test]
    fn goal_requires_a_non_empty_objective() {
        let args = parse(&["--goal", "   "]);
        assert_eq!(args.goal, None);
        assert_eq!(args.diagnostics[0].message, "--goal requires a non-empty objective");
    }

    #[test]
    fn goal_token_budget_requires_goal() {
        let args = parse(&["--goal-token-budget", "100"]);
        assert_eq!(args.goal_token_budget, Some(100));
        assert_eq!(args.diagnostics[0].message, "--goal-token-budget requires --goal");
    }

    #[test]
    fn list_models_is_removed_outside_the_internal_runtime() {
        let args = parse(&["--list-models", "claude"]);
        assert_eq!(args.list_models, None);
        assert_eq!(
            args.diagnostics[0].message,
            "--list-models was removed. Use \"prime-agent model list [search]\"."
        );
    }

    #[test]
    fn list_models_parses_for_the_internal_runtime_command() {
        let args = parse_args(&[
            INTERNAL_RUNTIME_COMMAND_MARKER.to_string(),
            "--list-models".to_string(),
        ]);
        assert_eq!(args.list_models, Some(ListModelsValue::All));
        let args = parse_args(&[
            INTERNAL_RUNTIME_COMMAND_MARKER.to_string(),
            "--list-models".to_string(),
            "gpt".to_string(),
        ]);
        assert_eq!(args.list_models, Some(ListModelsValue::Search("gpt".to_string())));
    }

    #[test]
    fn short_unknown_options_are_errors() {
        let args = parse(&["-z"]);
        assert_eq!(args.diagnostics[0].message, "Unknown option: -z");
    }

    #[test]
    fn models_split_on_commas() {
        let args = parse(&["--models", " a , b "]);
        assert_eq!(
            args.models,
            Some(vec!["a".to_string(), "b".to_string()])
        );
    }
}
