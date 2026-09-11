//! Port of packages/coding-agent/src/modes/agent-connection/tool-definition.ts

use crate::modes::agent_connection::types::AgentConnectionToolDefinition;

/// Source shape of `ToolDefinition` from `core/extensions/types.ts`.
///
/// The extensions slice owns the full type; this slice only reads the seven
/// fields it copies, so the port keeps the same projection the TypeScript does.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ToolDefinition {
    pub name: String,
    pub label: String,
    pub description: String,
    pub prompt_snippet: Option<String>,
    pub prompt_guidelines: Option<Vec<String>>,
    pub parameters: serde_json::Value,
    pub render_shell: Option<String>,
    pub replay_built_in_tool_name: Option<String>,
}

pub fn create_agent_connection_tool_definition(
    definition: Option<&ToolDefinition>,
) -> Option<AgentConnectionToolDefinition> {
    let definition = definition?;
    Some(AgentConnectionToolDefinition {
        name: definition.name.clone(),
        label: definition.label.clone(),
        description: definition.description.clone(),
        prompt_snippet: definition.prompt_snippet.clone(),
        prompt_guidelines: definition.prompt_guidelines.as_ref().map(|lines| lines.clone()),
        parameters: definition.parameters.clone(),
        render_shell: definition.render_shell.clone(),
        replay_built_in_tool_name: definition.replay_built_in_tool_name.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn undefined_definition_stays_undefined() {
        assert!(create_agent_connection_tool_definition(None).is_none());
    }

    #[test]
    fn copies_every_projected_field() {
        let definition = ToolDefinition {
            name: "ipython".to_string(),
            label: "Python".to_string(),
            description: "Run a cell".to_string(),
            prompt_snippet: Some("snippet".to_string()),
            prompt_guidelines: Some(vec!["a".to_string()]),
            parameters: json!({"type": "object"}),
            render_shell: Some("self".to_string()),
            replay_built_in_tool_name: Some("bash".to_string()),
        };
        let projected = create_agent_connection_tool_definition(Some(&definition)).unwrap();
        assert_eq!(projected.name, "ipython");
        assert_eq!(projected.prompt_snippet.as_deref(), Some("snippet"));
        assert_eq!(projected.prompt_guidelines.as_ref().unwrap().len(), 1);
        assert_eq!(projected.render_shell.as_deref(), Some("self"));
        assert_eq!(projected.replay_built_in_tool_name.as_deref(), Some("bash"));
        let value = serde_json::to_value(&projected).unwrap();
        assert_eq!(value.get("replayBuiltInToolName").unwrap(), &json!("bash"));
    }

    #[test]
    fn absent_optionals_are_omitted_not_null() {
        let definition = ToolDefinition {
            name: "read".to_string(),
            label: "Read".to_string(),
            description: "Read a file".to_string(),
            parameters: json!({}),
            ..Default::default()
        };
        let value = serde_json::to_value(create_agent_connection_tool_definition(Some(&definition)).unwrap()).unwrap();
        assert!(value.get("promptSnippet").is_none());
        assert!(value.get("promptGuidelines").is_none());
        assert!(value.get("renderShell").is_none());
    }
}
