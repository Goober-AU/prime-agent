//! Port of packages/coding-agent/src/core/extensions/wrapper.ts
//!
//! Tool wrappers for extension-registered tools. These wrappers only adapt tool
//! execution so extension tools receive the runner context. Tool call and tool
//! result interception is handled by AgentSession via agent-core hooks.

use std::sync::Arc;

use pi_agent_core::types::AgentTool;
use serde_json::Value;

use super::runner::ExtensionRunner;
use super::types::{ExtensionContext, RegisteredTool, ToolDefinition};

/// `ExtensionRunner | (() => ExtensionRunner)`.
#[derive(Clone)]
pub enum RunnerSource {
    Runner(Arc<ExtensionRunner>),
    Getter(Arc<dyn Fn() -> Arc<ExtensionRunner> + Send + Sync>),
}

fn to_runner_getter(source: &RunnerSource) -> Arc<dyn Fn() -> Arc<ExtensionRunner> + Send + Sync> {
    match source {
        RunnerSource::Getter(getter) => getter.clone(),
        RunnerSource::Runner(runner) => {
            let runner = runner.clone();
            Arc::new(move || runner.clone())
        }
    }
}

/// Adapt an extension `ToolDefinition` into an `AgentTool`.
///
/// `tool-definition-wrapper.ts` is shared with the tools slice, which owns its
/// own `ToolDefinition` type; the extension definition carries renderers and an
/// extension `ExtensionContext`, so the adaptation is done here with the same
/// observable behaviour (context resolved at call time via the runner).
/// blocked_on: needs a single shared ToolDefinition type across the tools and
/// extensions slices.
fn wrap_extension_tool_definition(
    definition: &ToolDefinition,
    get_runner: Arc<dyn Fn() -> Arc<ExtensionRunner> + Send + Sync>,
) -> AgentTool {
    let execute_definition = definition.execute.clone();
    let execute: Arc<
        dyn Fn(
                String,
                Value,
                Option<tokio_util::sync::CancellationToken>,
                Option<pi_agent_core::types::AgentToolUpdateCallback>,
            ) -> futures::future::BoxFuture<'static, Result<pi_agent_core::types::AgentToolResult, anyhow::Error>>
            + Send
            + Sync,
    > = Arc::new(
        move |tool_call_id: String,
              params: Value,
              signal: Option<tokio_util::sync::CancellationToken>,
              on_update: Option<pi_agent_core::types::AgentToolUpdateCallback>| {
            let execute_definition = execute_definition.clone();
            let get_runner = get_runner.clone();
            Box::pin(async move {
                let context = get_runner().create_context();
                execute_definition(tool_call_id, params, signal, on_update, context)
                    .await
                    .map_err(|error| anyhow::anyhow!(error))
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

/// Wrap a `RegisteredTool` into an `AgentTool`.
///
/// Uses the runner's `createContext()` for consistent context across tools and
/// event handlers.
pub fn wrap_registered_tool(registered_tool: &RegisteredTool, runner: RunnerSource) -> AgentTool {
    let get_runner = to_runner_getter(&runner);
    wrap_extension_tool_definition(&registered_tool.definition, get_runner)
}

/// Wrap all registered tools into `AgentTool`s.
///
/// Uses the runner's `createContext()` for consistent context across tools and
/// event handlers.
pub fn wrap_registered_tools(registered_tools: &[RegisteredTool], runner: RunnerSource) -> Vec<AgentTool> {
    let get_runner = to_runner_getter(&runner);
    registered_tools
        .iter()
        .map(|registered_tool| wrap_extension_tool_definition(&registered_tool.definition, get_runner.clone()))
        .collect()
}

/// Unused guard so `ExtensionContext` stays imported for the adapter signature.
#[allow(dead_code)]
fn _context_marker(_: Arc<dyn ExtensionContext>) {}
