//! Port of packages/coding-agent/src/core/prompts/index.ts
pub mod rlm;

pub use rlm::{build_child_agent_doctrine, build_rlm_prompt, build_subagent_guidance, ChildAgentDoctrineOptions, RlmPromptOptions};
