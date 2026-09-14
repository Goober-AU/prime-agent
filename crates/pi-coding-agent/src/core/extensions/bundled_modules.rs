//! Port of packages/coding-agent/src/core/extensions/bundled-modules.ts
//!
//! Modules made available to extensions via the bundled virtual module table.
//!
//! In the TypeScript build these imports are static so Bun bundles them into the
//! compiled binary; the module itself is loaded lazily so merely importing the
//! extension loader does not pull in the whole package graph at startup.
//!
//! The Rust port has no JS module graph: the table below keeps the same keys so
//! an extension that asks for one of these specifiers gets a deterministic
//! answer instead of an unknown-module error. Values are the Rust-side handles
//! for the corresponding crate (the crate root path plus a marker for the
//! sub-module specifiers).

use std::sync::Arc;

use serde_json::Value;

/// Rust-side stand-in for a bundled module namespace.
#[derive(Debug, Clone, PartialEq)]
pub struct BundledModule {
    /// Rust crate/module path that supplies this specifier.
    pub module_path: &'static str,
    /// True when the namespace is the whole crate root.
    pub is_crate_root: bool,
}

impl BundledModule {
    /// Serialise the namespace for extension consumption.
    pub fn to_value(&self) -> Value {
        serde_json::json!({
            "modulePath": self.module_path,
            "crateRoot": self.is_crate_root,
        })
    }
}

/// `VIRTUAL_MODULES`.
pub fn virtual_modules() -> Arc<indexmap::IndexMap<String, BundledModule>> {
    let mut modules: indexmap::IndexMap<String, BundledModule> = indexmap::IndexMap::new();
    // typebox has no Rust counterpart; the JSON-schema value shape is what the
    // port uses, so the specifier resolves to the serde_json schema helper.
    let typebox = BundledModule {
        module_path: "serde_json",
        is_crate_root: true,
    };
    modules.insert("typebox".to_string(), typebox.clone());
    modules.insert("typebox/compile".to_string(), typebox.clone());
    modules.insert("typebox/value".to_string(), typebox.clone());
    modules.insert("@sinclair/typebox".to_string(), typebox.clone());
    modules.insert("@sinclair/typebox/compile".to_string(), typebox.clone());
    modules.insert("@sinclair/typebox/value".to_string(), typebox);

    let pi_agent_core = BundledModule {
        module_path: "pi_agent_core",
        is_crate_root: true,
    };
    let pi_tui = BundledModule {
        module_path: "pi_tui",
        is_crate_root: true,
    };
    let pi_ai = BundledModule {
        module_path: "pi_ai",
        is_crate_root: true,
    };
    let pi_ai_oauth = BundledModule {
        module_path: "pi_ai::utils::oauth",
        is_crate_root: false,
    };
    let pi_coding_agent = BundledModule {
        module_path: "pi_coding_agent",
        is_crate_root: true,
    };

    modules.insert("@earendil-works/pi-agent-core".to_string(), pi_agent_core.clone());
    modules.insert("@earendil-works/pi-tui".to_string(), pi_tui.clone());
    modules.insert("@earendil-works/pi-ai".to_string(), pi_ai.clone());
    modules.insert("@earendil-works/pi-ai/oauth".to_string(), pi_ai_oauth.clone());
    modules.insert("@earendil-works/pi-coding-agent".to_string(), pi_coding_agent.clone());
    modules.insert("@mariozechner/pi-agent-core".to_string(), pi_agent_core);
    modules.insert("@mariozechner/pi-tui".to_string(), pi_tui);
    modules.insert("@mariozechner/pi-ai".to_string(), pi_ai);
    modules.insert("@mariozechner/pi-ai/oauth".to_string(), pi_ai_oauth);
    modules.insert("@mariozechner/pi-coding-agent".to_string(), pi_coding_agent);

    Arc::new(modules)
}

/// `VIRTUAL_MODULES` as an insert-ordered map, computed once per process.
pub fn virtual_modules_static() -> Arc<indexmap::IndexMap<String, BundledModule>> {
    static MODULES: std::sync::OnceLock<Arc<indexmap::IndexMap<String, BundledModule>>> =
        std::sync::OnceLock::new();
    MODULES.get_or_init(virtual_modules).clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn virtual_module_keys_match_the_typescript_table_in_order() {
        let modules = virtual_modules_static();
        let keys: Vec<&str> = modules.keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            [
                "typebox",
                "typebox/compile",
                "typebox/value",
                "@sinclair/typebox",
                "@sinclair/typebox/compile",
                "@sinclair/typebox/value",
                "@earendil-works/pi-agent-core",
                "@earendil-works/pi-tui",
                "@earendil-works/pi-ai",
                "@earendil-works/pi-ai/oauth",
                "@earendil-works/pi-coding-agent",
                "@mariozechner/pi-agent-core",
                "@mariozechner/pi-tui",
                "@mariozechner/pi-ai",
                "@mariozechner/pi-ai/oauth",
                "@mariozechner/pi-coding-agent",
            ]
        );
    }

    #[test]
    fn legacy_and_current_specifiers_share_one_namespace() {
        let modules = virtual_modules_static();
        assert_eq!(
            modules.get("@earendil-works/pi-ai").unwrap(),
            modules.get("@mariozechner/pi-ai").unwrap()
        );
        assert_eq!(
            modules.get("@earendil-works/pi-coding-agent").unwrap().module_path,
            "pi_coding_agent"
        );
        assert_eq!(
            modules.get("@earendil-works/pi-ai/oauth").unwrap().module_path,
            "pi_ai::utils::oauth"
        );
    }
}
