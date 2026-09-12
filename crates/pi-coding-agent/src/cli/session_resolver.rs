//! Port of packages/coding-agent/src/cli/session-resolver.ts
//!
//! The TypeScript module is a single re-export line:
//! `export * from "../core/session-resolver.js";`
//! The Rust equivalent is a re-export of the ported `core::session_resolver`
//! module, which is owned by the ca-session slice and is already landed.

pub use crate::core::session_resolver::*;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn re_exports_the_core_session_resolver_surface() {
        assert!(looks_like_session_path("./sessions/a.jsonl"));
        assert!(looks_like_session_path("a/b"));
        assert!(looks_like_session_path("a\\b"));
        assert!(!looks_like_session_path("abc123"));
    }
}
