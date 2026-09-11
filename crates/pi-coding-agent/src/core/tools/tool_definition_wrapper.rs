//! Port of packages/coding-agent/src/core/tools/tool-definition-wrapper.ts

use std::sync::Arc;

use pi_agent_core::types::{AgentTool, AgentToolResult, AgentToolUpdateCallback};
use pi_agent_core::types::ContentBlock;
use tokio_util::sync::CancellationToken;

use super::{ExtensionContext, ToolDefinition};

/// Wrap a ToolDefinition into an AgentTool for the core runtime.
pub fn wrap_tool_definition<TDetails>(
    definition: &ToolDefinition<TDetails>,
    ctx_factory: Option<Arc<dyn Fn() -> ExtensionContext + Send + Sync>>,
) -> AgentTool
where
    TDetails: Clone + Send + Sync + 'static,
{
    let inner = definition.clone();
    let ctx_factory_for_execute = ctx_factory.clone();
    let execute: Arc<
        dyn Fn(String, serde_json::Value, Option<CancellationToken>, Option<AgentToolUpdateCallback>) -> futures::future::BoxFuture<'static, Result<AgentToolResult, anyhow::Error>>
            + Send
            + Sync,
    > = Arc::new(
        move |tool_call_id: String,
              params: serde_json::Value,
              signal: Option<CancellationToken>,
              on_update: Option<AgentToolUpdateCallback>| {
            let inner = inner.clone();
            let ctx_factory = ctx_factory_for_execute.clone();
            Box::pin(async move {
                let ctx = match ctx_factory {
                    Some(factory) => factory(),
                    None => ExtensionContext::default(),
                };
                inner.execute(tool_call_id, params, signal, on_update, ctx).await
            })
        },
    );

    AgentTool {
        name: definition.name.clone(),
        description: definition.description.clone(),
        parameters: definition.parameters.clone(),
        label: definition.label.clone(),
        prepare_arguments: definition.prepare_arguments.clone(),
        execute,
        execution_mode: definition.execution_mode,
    }
}

/// Wrap multiple ToolDefinitions into AgentTools for the core runtime.
pub fn wrap_tool_definitions<TDetails>(
    definitions: &[ToolDefinition<TDetails>],
    ctx_factory: Option<Arc<dyn Fn() -> ExtensionContext + Send + Sync>>,
) -> Vec<AgentTool>
where
    TDetails: Clone + Send + Sync + 'static,
{
    definitions
        .iter()
        .map(|definition| wrap_tool_definition(definition, ctx_factory.clone()))
        .collect()
}

/// Synthesize a minimal ToolDefinition from an AgentTool.
///
/// This keeps AgentSession's internal registry definition-first even when a caller
/// provides plain AgentTool overrides that do not include prompt metadata or renderers.
pub fn create_tool_definition_from_agent_tool(tool: &AgentTool) -> ToolDefinition<serde_json::Value> {
    let execute_tool = tool.clone();
    let execute: Arc<
        dyn Fn(
                String,
                serde_json::Value,
                Option<CancellationToken>,
                Option<AgentToolUpdateCallback>,
                ExtensionContext,
            ) -> futures::future::BoxFuture<'static, Result<AgentToolResult, anyhow::Error>>
            + Send
            + Sync,
    > = Arc::new(
        move |tool_call_id: String,
              params: serde_json::Value,
              signal: Option<CancellationToken>,
              on_update: Option<AgentToolUpdateCallback>,
              _ctx: ExtensionContext| {
            let tool = execute_tool.clone();
            Box::pin(async move { (tool.execute)(tool_call_id, params, signal, on_update).await })
        },
    );

    ToolDefinition {
        name: tool.name.clone(),
        label: tool.label.clone(),
        description: tool.description.clone(),
        parameters: tool.parameters.clone(),
        prepare_arguments: tool.prepare_arguments.clone(),
        execution_mode: tool.execution_mode,
        execute,
        ..ToolDefinition::default()
    }
}

/// Convenience helper for the common case of wrapping a plain text tool result.
pub fn text_tool_result(text: impl Into<String>, details: serde_json::Value) -> AgentToolResult {
    AgentToolResult::new(vec![ContentBlock::text(text)], details)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn constant_definition() -> ToolDefinition<serde_json::Value> {
        let execute: Arc<
            dyn Fn(
                    String,
                    serde_json::Value,
                    Option<CancellationToken>,
                    Option<AgentToolUpdateCallback>,
                    ExtensionContext,
                ) -> futures::future::BoxFuture<'static, Result<AgentToolResult, anyhow::Error>>
                + Send
                + Sync,
        > = Arc::new(|_id, _params, _signal, _update, _ctx| {
            Box::pin(async move { Ok(text_tool_result("ok", serde_json::Value::Null)) })
        });
        ToolDefinition {
            name: "sample".to_string(),
            label: "sample".to_string(),
            description: "sample tool".to_string(),
            parameters: serde_json::json!({"type": "object"}),
            prepare_arguments: None,
            execution_mode: None,
            execute,
            ..ToolDefinition::default()
        }
    }

    #[tokio::test]
    async fn wrap_tool_definition_copies_metadata_and_executes() {
        let definition = constant_definition();
        let wrapped = wrap_tool_definition(&definition, None);
        assert_eq!(wrapped.name, "sample");
        assert_eq!(wrapped.label, "sample");
        assert_eq!(wrapped.description, "sample tool");
        assert_eq!(wrapped.parameters, serde_json::json!({"type": "object"}));
        let result = (wrapped.execute)("call-1".to_string(), serde_json::json!({}), None, None)
            .await
            .expect("executed");
        assert_eq!(result.content.len(), 1);
        assert_eq!(result.content[0].as_text(), Some("ok"));
    }

    #[tokio::test]
    async fn wrap_tool_definitions_maps_every_definition() {
        let definitions = vec![constant_definition(), constant_definition()];
        let wrapped = wrap_tool_definitions(&definitions, None);
        assert_eq!(wrapped.len(), 2);
        assert_eq!(wrapped[1].name, "sample");
    }

    #[tokio::test]
    async fn create_tool_definition_from_agent_tool_round_trips() {
        let wrapped = wrap_tool_definition(&constant_definition(), None);
        let definition = create_tool_definition_from_agent_tool(&wrapped);
        assert_eq!(definition.name, "sample");
        assert_eq!(definition.label, "sample");
        assert_eq!(definition.description, "sample tool");
        let result = definition
            .execute(
                "call-2".to_string(),
                serde_json::json!({}),
                None,
                None,
                ExtensionContext::default(),
            )
            .await
            .expect("executed");
        assert_eq!(result.content[0].as_text(), Some("ok"));
    }
}
