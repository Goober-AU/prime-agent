//! Port of packages/coding-agent/src/core/thinking-levels.ts

/// `THINKING_LEVELS: ThinkingLevel[]` - includes `"off"`, unlike the provider
/// level list in pi-ai, so the array is spelled out here.
pub const THINKING_LEVELS: [&str; 7] = ["off", "minimal", "low", "medium", "high", "xhigh", "max"];
