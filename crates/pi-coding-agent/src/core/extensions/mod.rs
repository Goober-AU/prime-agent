//! Port of packages/coding-agent/src/core/extensions/index.ts
//!
//! Re-export hub for the extension system. Rust has no re-export aliasing for
//! trait objects, so this module re-exports the items with `pub use`.

pub use crate::core::slash_commands::{SlashCommandInfo, SlashCommandSource};
pub use crate::core::source_info::SourceInfo;

pub use super::builtin::herdr_agent_state::{
    create_herdr_agent_state_extension, has_file_based_herdr_integration, herdr_agent_state_extension,
};
pub use super::loader::{
    create_extension_runtime, discover_and_load_extensions, load_extension_from_factory, load_extensions,
};
pub use super::runner::ExtensionRunner;
pub use super::types::*;
pub use super::wrapper::{wrap_registered_tool, wrap_registered_tools};

/// `ExtensionErrorListener`.
pub type ExtensionErrorListener = std::sync::Arc<dyn Fn(&ExtensionError) + Send + Sync>;
