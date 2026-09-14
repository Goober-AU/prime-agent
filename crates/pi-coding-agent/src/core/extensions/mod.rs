//! Port of packages/coding-agent/src/core/extensions/index.ts
//!
//! Re-export hub for the extension system. Rust has no re-export aliasing for
//! trait objects, so this module re-exports the items with `pub use`.
//!
//! NOTE (lead fix): the sibling modules were not declared, so the whole
//! extension tree was unreachable (`super::loader`, `super::types`,
//! `super::runner`, `super::wrapper`, `super::bundled_modules`, `super::builtin`).
//! The TypeScript `index.ts` re-exports from `./loader.js`, `./runner.js`,
//! `./types.js`, `./wrapper.js`, `./bundled-modules.js` and `./builtin/*.js`,
//! so those modules are declared here exactly as `core/kernel/mod.rs` does.

pub mod builtin;
pub mod bundled_modules;
pub mod loader;
pub mod runner;
pub mod types;
pub mod wrapper;

pub use crate::core::slash_commands::{SlashCommandInfo, SlashCommandSource};
pub use crate::core::source_info::SourceInfo;

pub use self::builtin::herdr_agent_state::{
    create_herdr_agent_state_extension, has_file_based_herdr_integration, herdr_agent_state_extension,
};
pub use self::loader::{
    create_extension_runtime, discover_and_load_extensions, load_extension_from_factory, load_extensions,
};
pub use self::runner::ExtensionRunner;
pub use self::types::*;
pub use self::wrapper::{wrap_registered_tool, wrap_registered_tools};

/// `ExtensionErrorListener`.
pub type ExtensionErrorListener = std::sync::Arc<dyn Fn(&ExtensionError) + Send + Sync>;
