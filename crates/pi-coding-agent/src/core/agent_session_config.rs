//! Port of packages/coding-agent/src/core/agent-session-config.ts

use pi_agent_core::types::ThinkingLevel;

use crate::core::autonomous::AgentAutonomousConfig;

/// `type AgentExecutionMode`.
pub type AgentExecutionMode = String;

pub const AGENT_EXECUTION_MODE_INTERACTIVE: &str = "interactive";
pub const AGENT_EXECUTION_MODE_PRINT: &str = "print";
pub const AGENT_EXECUTION_MODE_JSON: &str = "json";
pub const AGENT_EXECUTION_MODE_RPC: &str = "rpc";
pub const AGENT_EXECUTION_MODE_ACP: &str = "acp";

/// `interface AgentSessionRuntimeConfig`.
///
/// Every member is optional in the TypeScript. `undefined` means "do not
/// override the base value", so each maps to `Option<T>`; the merge below keeps
/// that distinction observable.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AgentSessionRuntimeConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_dir: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_dir: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub append_system_prompt: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<ThinkingLevel>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub models: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub no_tools: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub no_builtin_tools: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extensions: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub no_extensions: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skills: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub no_skills: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_templates: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub no_prompt_templates: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub themes: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub no_themes: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub no_context_files: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub autonomous: Option<AgentAutonomousConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extension_flag_values: Option<indexmap::IndexMap<String, serde_json::Value>>,
    /// When true, auto-refine runs synchronously between turns at the
    /// shouldStopAfterTurn boundary instead of in the background after
    /// agent_end. Passed from the JSON/print client to the daemon worker
    /// so it survives the appMode="daemon" context switch.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub serialized_refine: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution_mode: Option<AgentExecutionMode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub telemetry_disabled: Option<bool>,
    /// Initial goal to seed when creating a new top-level session (rlmDepth 0).
    /// Ignored for subagent sessions and when the branch already has a persisted
    /// thread_goal_state entry (idempotent restart/rehydration).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub initial_goal: Option<InitialGoalConfig>,
}

/// `initialGoal?: { objective: string; tokenBudget?: number }`.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct InitialGoalConfig {
    pub objective: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_budget: Option<f64>,
}

/// `type DurableAgentSessionRuntimeConfig`.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DurableAgentSessionRuntimeConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_dir: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_dir: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub telemetry_disabled: Option<bool>,
}

/// `durableAgentSessionRuntimeConfig(config)`.
///
/// Only non-secret host settings needed to locate and govern durable daemon
/// state.
pub fn durable_agent_session_runtime_config(config: &AgentSessionRuntimeConfig) -> DurableAgentSessionRuntimeConfig {
    DurableAgentSessionRuntimeConfig {
        cwd: config.cwd.clone(),
        agent_dir: config.agent_dir.clone(),
        session_dir: config.session_dir.clone(),
        telemetry_disabled: if config.telemetry_disabled == Some(true) {
            Some(true)
        } else {
            None
        },
    }
}

/// `mergeAgentSessionRuntimeConfig(base, override?)`.
pub fn merge_agent_session_runtime_config(
    base: &AgentSessionRuntimeConfig,
    override_config: Option<&AgentSessionRuntimeConfig>,
) -> AgentSessionRuntimeConfig {
    let Some(override_config) = override_config else {
        return clone_agent_session_runtime_config(base);
    };
    AgentSessionRuntimeConfig {
        cwd: override_config.cwd.clone().or_else(|| base.cwd.clone()),
        agent_dir: override_config.agent_dir.clone().or_else(|| base.agent_dir.clone()),
        session_dir: override_config.session_dir.clone().or_else(|| base.session_dir.clone()),
        provider: override_config.provider.clone().or_else(|| base.provider.clone()),
        model: override_config.model.clone().or_else(|| base.model.clone()),
        api_key: override_config.api_key.clone().or_else(|| base.api_key.clone()),
        system_prompt: override_config
            .system_prompt
            .clone()
            .or_else(|| base.system_prompt.clone()),
        append_system_prompt: clone_array(
            override_config
                .append_system_prompt
                .clone()
                .or_else(|| base.append_system_prompt.clone()),
        ),
        thinking: override_config.thinking.clone().or_else(|| base.thinking.clone()),
        models: clone_array(override_config.models.clone().or_else(|| base.models.clone())),
        tools: clone_array(override_config.tools.clone().or_else(|| base.tools.clone())),
        no_tools: override_config.no_tools.or(base.no_tools),
        no_builtin_tools: override_config.no_builtin_tools.or(base.no_builtin_tools),
        extensions: clone_array(override_config.extensions.clone().or_else(|| base.extensions.clone())),
        no_extensions: override_config.no_extensions.or(base.no_extensions),
        skills: clone_array(override_config.skills.clone().or_else(|| base.skills.clone())),
        no_skills: override_config.no_skills.or(base.no_skills),
        prompt_templates: clone_array(
            override_config
                .prompt_templates
                .clone()
                .or_else(|| base.prompt_templates.clone()),
        ),
        no_prompt_templates: override_config.no_prompt_templates.or(base.no_prompt_templates),
        themes: clone_array(override_config.themes.clone().or_else(|| base.themes.clone())),
        no_themes: override_config.no_themes.or(base.no_themes),
        no_context_files: override_config.no_context_files.or(base.no_context_files),
        autonomous: merge_autonomous_config(base.autonomous.as_ref(), override_config.autonomous.as_ref()),
        extension_flag_values: match (&base.extension_flag_values, &override_config.extension_flag_values) {
            (None, None) => None,
            (base_flags, override_flags) => {
                let mut merged = base_flags.clone().unwrap_or_default();
                if let Some(override_flags) = override_flags {
                    for (key, value) in override_flags {
                        merged.insert(key.clone(), value.clone());
                    }
                }
                Some(merged)
            }
        },
        serialized_refine: override_config.serialized_refine.or(base.serialized_refine),
        execution_mode: override_config
            .execution_mode
            .clone()
            .or_else(|| base.execution_mode.clone()),
        telemetry_disabled: if base.telemetry_disabled == Some(true)
            || override_config.telemetry_disabled == Some(true)
        {
            Some(true)
        } else {
            None
        },
        initial_goal: override_config
            .initial_goal
            .clone()
            .or_else(|| base.initial_goal.clone()),
    }
}

/// `cloneAgentSessionRuntimeConfig(config)`.
fn clone_agent_session_runtime_config(config: &AgentSessionRuntimeConfig) -> AgentSessionRuntimeConfig {
    AgentSessionRuntimeConfig {
        cwd: config.cwd.clone(),
        agent_dir: config.agent_dir.clone(),
        session_dir: config.session_dir.clone(),
        provider: config.provider.clone(),
        model: config.model.clone(),
        api_key: config.api_key.clone(),
        system_prompt: config.system_prompt.clone(),
        append_system_prompt: clone_array(config.append_system_prompt.clone()),
        thinking: config.thinking.clone(),
        models: clone_array(config.models.clone()),
        tools: clone_array(config.tools.clone()),
        no_tools: config.no_tools,
        no_builtin_tools: config.no_builtin_tools,
        extensions: clone_array(config.extensions.clone()),
        no_extensions: config.no_extensions,
        skills: clone_array(config.skills.clone()),
        no_skills: config.no_skills,
        prompt_templates: clone_array(config.prompt_templates.clone()),
        no_prompt_templates: config.no_prompt_templates,
        themes: clone_array(config.themes.clone()),
        no_themes: config.no_themes,
        no_context_files: config.no_context_files,
        autonomous: merge_autonomous_config(None, config.autonomous.as_ref()),
        extension_flag_values: config.extension_flag_values.clone(),
        serialized_refine: config.serialized_refine,
        execution_mode: config.execution_mode.clone(),
        telemetry_disabled: config.telemetry_disabled,
        initial_goal: config.initial_goal.clone(),
    }
}

/// `mergeAutonomousConfig(base, override)`.
pub fn merge_autonomous_config(
    base: Option<&AgentAutonomousConfig>,
    override_config: Option<&AgentAutonomousConfig>,
) -> Option<AgentAutonomousConfig> {
    if base.is_none() && override_config.is_none() {
        return None;
    }
    let gates = merge_autonomous_gate_config(
        base.and_then(|base| base.gates.as_ref()),
        override_config.and_then(|override_config| override_config.gates.as_ref()),
    );
    let mut merged = base.cloned().unwrap_or_default();
    if let Some(override_config) = override_config {
        if override_config.enabled.is_some() {
            merged.enabled = override_config.enabled;
        }
        if override_config.max_continuations.is_some() {
            merged.max_continuations = override_config.max_continuations;
        }
        if override_config.max_turns.is_some() {
            merged.max_turns = override_config.max_turns;
        }
        if override_config.max_tokens.is_some() {
            merged.max_tokens = override_config.max_tokens;
        }
        if override_config.timeout_ms.is_some() {
            merged.timeout_ms = override_config.timeout_ms;
        }
        if override_config.continuation_prompt.is_some() {
            merged.continuation_prompt = override_config.continuation_prompt.clone();
        }
    }
    merged.gates = gates;
    Some(merged)
}

/// `mergeAutonomousGateConfig(base, override)`.
fn merge_autonomous_gate_config(
    base: Option<&crate::core::autonomous::AgentAutonomousGateConfig>,
    override_config: Option<&crate::core::autonomous::AgentAutonomousGateConfig>,
) -> Option<crate::core::autonomous::AgentAutonomousGateConfig> {
    if base.is_none() && override_config.is_none() {
        return None;
    }
    let mut merged = base.cloned().unwrap_or_default();
    if let Some(override_config) = override_config {
        if override_config.max_retries.is_some() {
            merged.max_retries = override_config.max_retries;
        }
        if override_config.timeout_ms.is_some() {
            merged.timeout_ms = override_config.timeout_ms;
        }
    }
    if override_config.and_then(|value| value.commands.clone()).is_some()
        || base.and_then(|value| value.commands.clone()).is_some()
    {
        merged.commands = clone_array(
            override_config
                .and_then(|value| value.commands.clone())
                .or_else(|| base.and_then(|value| value.commands.clone())),
        );
    }
    Some(merged)
}

/// `cloneArray(value)`.
fn clone_array<T: Clone>(value: Option<Vec<T>>) -> Option<Vec<T>> {
    value.map(|value| value.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::autonomous::AgentAutonomousGateConfig;
    use serde_json::json;

    #[test]
    fn durable_config_keeps_only_non_secret_host_settings() {
        let config = AgentSessionRuntimeConfig {
            cwd: Some("/work".to_string()),
            agent_dir: Some("/agent".to_string()),
            session_dir: Some("/sessions".to_string()),
            api_key: Some("secret".to_string()),
            telemetry_disabled: Some(true),
            ..Default::default()
        };
        let durable = durable_agent_session_runtime_config(&config);
        assert_eq!(durable.cwd.as_deref(), Some("/work"));
        assert_eq!(durable.agent_dir.as_deref(), Some("/agent"));
        assert_eq!(durable.session_dir.as_deref(), Some("/sessions"));
        assert_eq!(durable.telemetry_disabled, Some(true));
        assert_eq!(
            durable_agent_session_runtime_config(&AgentSessionRuntimeConfig::default()),
            DurableAgentSessionRuntimeConfig::default()
        );
    }

    #[test]
    fn merge_without_override_clones_the_base_arrays() {
        let base = AgentSessionRuntimeConfig {
            tools: Some(vec!["ipython".to_string()]),
            append_system_prompt: Some(vec!["one".to_string()]),
            ..Default::default()
        };
        let merged = merge_agent_session_runtime_config(&base, None);
        assert_eq!(merged.tools.as_ref().unwrap(), &vec!["ipython".to_string()]);
        assert_eq!(merged.append_system_prompt.as_ref().unwrap(), &vec!["one".to_string()]);
    }

    #[test]
    fn override_wins_per_field_and_keeps_base_values_otherwise() {
        let base = AgentSessionRuntimeConfig {
            provider: Some("anthropic".to_string()),
            model: Some("claude".to_string()),
            no_tools: Some(true),
            ..Default::default()
        };
        let override_config = AgentSessionRuntimeConfig {
            model: Some("gpt".to_string()),
            no_tools: Some(false),
            ..Default::default()
        };
        let merged = merge_agent_session_runtime_config(&base, Some(&override_config));
        assert_eq!(merged.provider.as_deref(), Some("anthropic"));
        assert_eq!(merged.model.as_deref(), Some("gpt"));
        assert_eq!(merged.no_tools, Some(false));
    }

    #[test]
    fn extension_flags_merge_and_stay_absent_when_both_are_absent() {
        assert!(merge_agent_session_runtime_config(&AgentSessionRuntimeConfig::default(), None)
            .extension_flag_values
            .is_none());

        let mut base_flags = indexmap::IndexMap::new();
        base_flags.insert("alpha".to_string(), json!(true));
        let mut override_flags = indexmap::IndexMap::new();
        override_flags.insert("beta".to_string(), json!("value"));
        override_flags.insert("alpha".to_string(), json!(false));
        let base = AgentSessionRuntimeConfig {
            extension_flag_values: Some(base_flags),
            ..Default::default()
        };
        let override_config = AgentSessionRuntimeConfig {
            extension_flag_values: Some(override_flags),
            ..Default::default()
        };
        let merged = merge_agent_session_runtime_config(&base, Some(&override_config));
        let flags = merged.extension_flag_values.unwrap();
        assert_eq!(flags.get("alpha"), Some(&json!(false)));
        assert_eq!(flags.get("beta"), Some(&json!("value")));
    }

    #[test]
    fn telemetry_disabled_is_only_set_when_requested() {
        let merged = merge_agent_session_runtime_config(
            &AgentSessionRuntimeConfig {
                telemetry_disabled: Some(false),
                ..Default::default()
            },
            Some(&AgentSessionRuntimeConfig::default()),
        );
        assert_eq!(merged.telemetry_disabled, None);

        let merged = merge_agent_session_runtime_config(
            &AgentSessionRuntimeConfig::default(),
            Some(&AgentSessionRuntimeConfig {
                telemetry_disabled: Some(true),
                ..Default::default()
            }),
        );
        assert_eq!(merged.telemetry_disabled, Some(true));
    }

    #[test]
    fn autonomous_config_merge_keeps_gates_and_override_fields() {
        let base = AgentAutonomousConfig {
            enabled: Some(false),
            max_turns: Some(5.0),
            gates: Some(AgentAutonomousGateConfig {
                commands: Some(vec!["a".to_string()]),
                max_retries: Some(1.0),
                timeout_ms: None,
            }),
            ..Default::default()
        };
        let override_config = AgentAutonomousConfig {
            enabled: Some(true),
            gates: Some(AgentAutonomousGateConfig {
                commands: Some(vec!["b".to_string()]),
                max_retries: None,
                timeout_ms: Some(9.0),
            }),
            ..Default::default()
        };
        let merged = merge_autonomous_config(Some(&base), Some(&override_config)).unwrap();
        assert_eq!(merged.enabled, Some(true));
        assert_eq!(merged.max_turns, Some(5.0));
        let gates = merged.gates.unwrap();
        assert_eq!(gates.commands.unwrap(), vec!["b".to_string()]);
        assert_eq!(gates.max_retries, Some(1.0));
        assert_eq!(gates.timeout_ms, Some(9.0));
    }

    #[test]
    fn autonomous_merge_returns_none_only_when_both_sides_are_missing() {
        assert!(merge_autonomous_config(None, None).is_none());
        let base = AgentAutonomousConfig {
            enabled: Some(true),
            ..Default::default()
        };
        let merged = merge_autonomous_config(Some(&base), None).unwrap();
        assert_eq!(merged.enabled, Some(true));
        assert!(merged.gates.is_none());
    }

    #[test]
    fn serialized_json_uses_typescript_field_names() {
        let config = AgentSessionRuntimeConfig {
            api_key: Some("k".to_string()),
            no_context_files: Some(true),
            serialized_refine: Some(true),
            execution_mode: Some(AGENT_EXECUTION_MODE_JSON.to_string()),
            initial_goal: Some(InitialGoalConfig {
                objective: "do it".to_string(),
                token_budget: Some(10.0),
            }),
            ..Default::default()
        };
        let serialized = serde_json::to_value(&config).unwrap();
        assert_eq!(serialized["apiKey"], json!("k"));
        assert_eq!(serialized["noContextFiles"], json!(true));
        assert_eq!(serialized["serializedRefine"], json!(true));
        assert_eq!(serialized["executionMode"], json!("json"));
        assert_eq!(serialized["initialGoal"]["objective"], json!("do it"));
        assert!(serialized.get("systemPrompt").is_none());
    }
}
