//! Port of packages/coding-agent/src (see docs/MIGRATION-MAP.md).
pub mod bun;
pub mod cli;
pub mod cli_entry;
pub mod cli_main_entry;
pub mod config;
pub mod core;
pub mod main_entry;
mod native_main_host;
pub mod migrations;
#[path = "mod.rs"]
pub mod index;
pub mod modes;
pub mod package_manager_cli;
pub mod postinstall;
pub mod themes;
pub mod utils;
