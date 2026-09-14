//! Port of packages/ai/src/mcp.ts
//!
//! `export * from "./mcp/index.js"` - the re-export surface of the mcp module.

pub use crate::mcp::catalog::{
    builtin_mcp_catalog, get_catalog_entry, register_builtin_mcp_oauth_providers, McpCatalogEntry,
    McpCatalogOAuth, BUILTIN_MCP_CATALOG,
};
pub use crate::mcp::oauth::{create_mcp_oauth_provider, McpOAuthConfig};
