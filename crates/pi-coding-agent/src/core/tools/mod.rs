//! Port of packages/coding-agent/src/core/tools/index.ts

pub mod acp_mcp;
pub mod bash;
pub mod code_preview;
pub mod edit;
pub mod edit_diff;
pub mod file_mutation_queue;
pub mod ipython;
pub mod ipython_cell_code;
pub mod output_accumulator;
pub mod path_utils;
pub mod render_utils;
pub mod tool_definition_wrapper;
pub mod truncate;

use std::collections::BTreeMap;

use crate::core::extensions::types::ToolDefinition;
use crate::core::ipython_tool_types::{IpythonToolDetails, IpythonToolOptions};
use ipython::create_ipython_tool_definition;

/// TypeScript `export type ToolName = "ipython"`.
pub const TOOL_NAMES: [&str; 1] = ["ipython"];

/// TypeScript `interface ToolsOptions`.
#[derive(Clone, Default)]
pub struct ToolsOptions {
    pub ipython: Option<IpythonToolOptions>,
}

/// TypeScript `createAllToolDefinitions`.
///
/// The TypeScript return type is `Record<ToolName, ToolDef>`; `BTreeMap` keeps
/// the single-key record deterministic (the key set is fixed by [`TOOL_NAMES`]).
pub fn create_all_tool_definitions(
    cwd: &str,
    options: Option<&ToolsOptions>,
) -> BTreeMap<&'static str, ToolDefinition<IpythonToolDetails>> {
    let mut definitions = BTreeMap::new();
    definitions.insert(
        "ipython",
        create_ipython_tool_definition(cwd, options.and_then(|options| options.ipython.as_ref())),
    );
    definitions
}
