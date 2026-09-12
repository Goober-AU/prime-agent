# Worklist: core/agent_session/runtime_members.rs (pack B respawn)

Total errors: 109

Shared-contract note: core/agent_session.rs now RE-EXPORTS the canonical owners:
    pub use crate::core::agent_messages::AgentSessionMessageController;
    pub use crate::core::autonomous::AgentAutonomousConfig;
    pub use crate::core::cron_jobs::{AgentCronJob, AgentRlmHeartbeatController};
Use the CANONICAL types from core/cron_jobs.rs, core/agent_messages.rs, core/autonomous.rs.
Most of your 42 'struct has no field' + 20 'no field on type' errors are callers still using
the OLD lossy shapes. Fix the CALL SITES to the canonical fields - do not add shims.

  L8       mismatched types
    --- rendered ---
      error[E0308]: mismatched types
       --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:8:77
        |
      8 |         if let Some(ui) = &bindings.ui_context { runner.set_ui_context(Some(ui.clone())); }
        |                                                                        ---- ^^^^^^^^^^ expected `Arc<dyn ExtensionUiContext>`, found `Value`
        |                                                                        |
        |                                                                        arguments to this enum variant are incorrect
        |
        = note: expected struct `std::sync::Arc<dyn extensions::types::ExtensionUiContext>`
                     found enum `serde_json::Value`
      help: the type constructed contains `serde_json::Value` due to the type of the argument passed
       --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:8:72
        |
      8 |         if let Some(ui) = &bindings.ui_context { runner.set_ui_context(Some(ui.clone())); }
        |                                                                        ^^^^^----------^
        |                                                                             |
  L9       mismatched types
    --- rendered ---
      error[E0308]: mismatched types
       --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:9:101
        |
      9 |         if let Some(actions) = &bindings.command_context_actions { runner.bind_command_context(Some(actions.clone())); }
        |                                                                                                ---- ^^^^^^^^^^^^^^^ expected `ExtensionCommandContextActions`, found `Value`
        |                                                                                                |
        |                                                                                                arguments to this enum variant are incorrect
        |
      help: the type constructed contains `serde_json::Value` due to the type of the argument passed
       --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:9:96
        |
      9 |         if let Some(actions) = &bindings.command_context_actions { runner.bind_command_context(Some(actions.clone())); }
        |                                                                                                ^^^^^---------------^
        |                                                                                                     |
        |                                                                                                     this argument influences the type of `Some`
      note: tuple variant defined here
  L10      mismatched types
    --- rendered ---
      error[E0308]: mismatched types
         --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:10:70
          |
       10 |         if let Some(listener) = &bindings.on_error { runner.on_error(listener.clone()); }
          |                                                             -------- ^^^^^^^^^^^^^^^^ expected `Arc<dyn Fn(...) + Send + Sync>`, found `Arc<dyn Fn(Value) + Send + Sync>`
          |                                                             |
          |                                                             arguments to this method are incorrect
          |
          = note: expected struct `std::sync::Arc<(dyn Fn(extensions::types::ExtensionError) + std::marker::Send + Sync + 'static)>`
                     found struct `std::sync::Arc<dyn Fn(serde_json::Value) + std::marker::Send + Sync>`
      note: method defined here
         --> crates\pi-coding-agent\src\core\extensions\runner.rs:684:12
          |
      684 |     pub fn on_error(&self, listener: ExtensionErrorListener) -> Arc<dyn Fn() + Send + Sync> {
          |            ^^^^^^^^        --------------------------------
      
  L53      mismatched types
    --- rendered ---
      error[E0308]: mismatched types
         --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:53:37
          |
       53 |         runner.bind_command_context(self.extension_command_context_actions.clone());
          |                -------------------- ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ expected `Option<ExtensionCommandContextActions>`, found `Option<Value>`
          |                |
          |                arguments to this method are incorrect
          |
          = note: expected enum `std::option::Option<ExtensionCommandContextActions>`
                     found enum `std::option::Option<serde_json::Value>`
      note: method defined here
         --> crates\pi-coding-agent\src\core\extensions\runner.rs:495:12
          |
      495 |     pub fn bind_command_context(&self, actions: Option<ExtensionCommandContextActions>) {
          |            ^^^^^^^^^^^^^^^^^^^^        -----------------------------------------------
      
  L54      mismatched types
    --- rendered ---
      error[E0308]: mismatched types
         --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:54:82
          |
       54 |         if let Some(listener) = &self.extension_error_listener { runner.on_error(listener.clone()); }
          |                                                                         -------- ^^^^^^^^^^^^^^^^ expected `Arc<dyn Fn(...) + Send + Sync>`, found `Arc<dyn Fn(Value) + Send + Sync>`
          |                                                                         |
          |                                                                         arguments to this method are incorrect
          |
          = note: expected struct `std::sync::Arc<(dyn Fn(extensions::types::ExtensionError) + std::marker::Send + Sync + 'static)>`
                     found struct `std::sync::Arc<dyn Fn(serde_json::Value) + std::marker::Send + Sync>`
      note: method defined here
         --> crates\pi-coding-agent\src\core\extensions\runner.rs:684:12
          |
      684 |     pub fn on_error(&self, listener: ExtensionErrorListener) -> Arc<dyn Fn() + Send + Sync> {
          |            ^^^^^^^^        --------------------------------
      
  L68      cannot find function `runtime_extension_actions` in this scope
    --- rendered ---
      error[E0425]: cannot find function `runtime_extension_context_actions` in this scope
        --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:68:67
         |
      68 |         runner.bind_core(runtime_extension_actions(weak.clone()), runtime_extension_context_actions(weak), None);
         |                                                                   ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ not found in this scope
      
      
  L68      cannot find function `runtime_extension_context_actions` in this scope
    --- rendered ---
      error[E0425]: cannot find function `runtime_extension_context_actions` in this scope
        --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:68:67
         |
      68 |         runner.bind_core(runtime_extension_actions(weak.clone()), runtime_extension_context_actions(weak), None);
         |                                                                   ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ not found in this scope
      
      
  L81      cannot find function `runtime_source_info` in this scope
    --- rendered ---
      error[E0425]: cannot find function `runtime_source_info` in this scope
        --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:81:62
         |
      81 |                 definition: definition.clone(), source_info: runtime_source_info(name, "builtin"),
         |                                                              ^^^^^^^^^^^^^^^^^^^ not found in this scope
      
      
  L85      cannot find function `runtime_source_info` in this scope
    --- rendered ---
      error[E0425]: cannot find function `runtime_source_info` in this scope
        --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:85:58
         |
      85 |             definition: definition.clone(), source_info: runtime_source_info(&definition.name, "sdk"),
         |                                                          ^^^^^^^^^^^^^^^^^^^ not found in this scope
      
      
  L123     the trait bound `std::sync::Mutex<session_manager::SessionManager>: extensions::types::SessionManager` is not satisfied
    --- rendered ---
      error[E0277]: the trait bound `std::sync::Mutex<session_manager::SessionManager>: extensions::types::SessionManager` is not satisfied
         --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:123:66
          |
      123 |             loaded.extensions, loaded.runtime, self.cwd.clone(), self.session_manager.clone(),
          |                                                                  ^^^^^^^^^^^^^^^^^^^^^^^^^^^^ the trait `extensions::types::SessionManager` is not implemented for `std::sync::Mutex<session_manager::SessionManager>`
          |
      help: the trait `extensions::types::SessionManager` is implemented for `NullSessionManager`
         --> crates\pi-coding-agent\src\core\extensions\runner.rs:313:1
          |
      313 | impl SessionManager for NullSessionManager {}
          | ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
          = note: required for the cast from `std::sync::Arc<std::sync::Mutex<session_manager::SessionManager>>` to `std::sync::Arc<(dyn extensions::types::SessionManager + 'static)>`
      
      
  L124     cannot find function, tuple struct or tuple variant `RuntimeModelRegistry` in this scope
    --- rendered ---
      error[E0425]: cannot find function, tuple struct or tuple variant `RuntimeModelRegistry` in this scope
         --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:124:22
          |
      124 |             Arc::new(RuntimeModelRegistry(self.model_registry.clone())),
          |                      ^^^^^^^^^^^^^^^^^^^^ not found in this scope
      
      
  L317     mismatched types
    --- rendered ---
      error[E0308]: mismatched types
          --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:317:46
           |
       317 |         manager.append_thinking_level_change(&options.thinking_level)?;
           |                 ---------------------------- ^^^^^^^^^^^^^^^^^^^^^^^ expected `&str`, found `&ThinkingLevel`
           |                 |
           |                 arguments to this method are incorrect
           |
           = note: expected reference `&str`
                      found reference `&pi_agent_core::types::ThinkingLevel`
      note: method defined here
          --> crates\pi-coding-agent\src\core\session_manager.rs:3698:12
           |
      3698 |     pub fn append_thinking_level_change(&mut self, thinking_level: &str) -> Result<String, String> {
           |            ^^^^^^^^^^^^^^^^^^^^^^^^^^^^            --------------------
      
  L433     mismatched types
    --- rendered ---
      error[E0308]: mismatched types
         --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:433:41
          |
      433 |         Ok(self.build_rlm_subagent_list(listed))
          |                 ----------------------- ^^^^^^ expected `Option<AgentSessionMessageListResult>`, found `Option<AgentFamilyRosterResult>`
          |                 |
          |                 arguments to this method are incorrect
          |
          = note: expected enum `std::option::Option<agent_messages::AgentSessionMessageListResult>`
                     found enum `std::option::Option<AgentFamilyRosterResult>`
      note: method defined here
         --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:437:19
          |
      437 |     pub(super) fn build_rlm_subagent_list(
          |                   ^^^^^^^^^^^^^^^^^^^^^^^
      438 |         &self,
  L451     struct `rlm_runtime::RlmSubagentRegistryEntry` has no field named `child_id`
    --- rendered ---
      error[E0560]: struct `rlm_runtime::RlmSubagentRegistryEntry` has no field named `child_id`
         --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:451:17
          |
      451 |                 child_id: run.id.clone(),
          |                 ^^^^^^^^ `rlm_runtime::RlmSubagentRegistryEntry` does not have this field
          |
          = note: available fields are: `rlm_child_id`, `active_session_id`, `session_id`, `session_dir`
      
      
  L454     struct `rlm_runtime::RlmSubagentRegistryEntry` has no field named `label`
    --- rendered ---
      error[E0609]: no field `label` on type `&agent_session::RlmChildRun`
         --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:454:28
          |
      454 |                 label: run.label.clone(),
          |                            ^^^^^ unknown field
          |
          = note: available fields are: `id`, `prompt`, `session_name`, `session_dir`, `model` ... and 27 others
      
      
  L454     no field `label` on type `&agent_session::RlmChildRun`
    --- rendered ---
      error[E0609]: no field `label` on type `&agent_session::RlmChildRun`
         --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:454:28
          |
      454 |                 label: run.label.clone(),
          |                            ^^^^^ unknown field
          |
          = note: available fields are: `id`, `prompt`, `session_name`, `session_dir`, `model` ... and 27 others
      
      
  L455     struct `rlm_runtime::RlmSubagentRegistryEntry` has no field named `model`
    --- rendered ---
      error[E0560]: struct `rlm_runtime::RlmSubagentRegistryEntry` has no field named `model`
         --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:455:17
          |
      455 |                 model: run.model.clone(),
          |                 ^^^^^ `rlm_runtime::RlmSubagentRegistryEntry` does not have this field
          |
          = note: available fields are: `rlm_child_id`, `active_session_id`, `session_id`, `session_dir`
      
      
  L459     no field `subagents` on type `agent_messages::AgentSessionMessageListResult`
    --- rendered ---
      error[E0609]: no field `subagents` on type `agent_messages::AgentSessionMessageListResult`
         --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:459:33
          |
      459 |             for agent in listed.subagents.iter() {
          |                                 ^^^^^^^^^ unknown field
          |
      help: a field with a similar name exists
          |
      459 -             for agent in listed.subagents.iter() {
      459 +             for agent in listed.agents.iter() {
          |
      
      
  L464     struct `rlm_runtime::RlmSubagentRegistryEntry` has no field named `child_id`
    --- rendered ---
      error[E0560]: struct `rlm_runtime::RlmSubagentRegistryEntry` has no field named `child_id`
         --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:464:21
          |
      464 |                     child_id: agent.session_id.clone(),
          |                     ^^^^^^^^ `rlm_runtime::RlmSubagentRegistryEntry` does not have this field
          |
          = note: available fields are: `rlm_child_id`, `active_session_id`, `session_id`, `session_dir`
      
      
  L467     struct `rlm_runtime::RlmSubagentRegistryEntry` has no field named `label`
    --- rendered ---
      error[E0560]: struct `rlm_runtime::RlmSubagentRegistryEntry` has no field named `label`
         --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:467:21
          |
      467 |                     label: None,
          |                     ^^^^^ `rlm_runtime::RlmSubagentRegistryEntry` does not have this field
          |
          = note: available fields are: `rlm_child_id`, `active_session_id`, `session_id`, `session_dir`
      
      
  L468     struct `rlm_runtime::RlmSubagentRegistryEntry` has no field named `model`
    --- rendered ---
      error[E0560]: struct `rlm_runtime::RlmSubagentRegistryEntry` has no field named `model`
         --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:468:21
          |
      468 |                     model: None,
          |                     ^^^^^ `rlm_runtime::RlmSubagentRegistryEntry` does not have this field
          |
          = note: available fields are: `rlm_child_id`, `active_session_id`, `session_id`, `session_dir`
      
      
  L474     struct `rlm_runtime::RlmListSubagentsResult` has no field named `agents`
    --- rendered ---
      error[E0560]: struct `rlm_runtime::RlmListSubagentsResult` has no field named `agents`
         --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:474:13
          |
      474 |             agents: entries,
          |             ^^^^^^ `rlm_runtime::RlmListSubagentsResult` does not have this field
          |
          = note: available fields are: `subagents`
      
      
  L475     struct `rlm_runtime::RlmListSubagentsResult` has no field named `max_depth`
    --- rendered ---
      error[E0560]: struct `rlm_runtime::RlmListSubagentsResult` has no field named `max_depth`
         --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:475:13
          |
      475 |             max_depth,
          |             ^^^^^^^^^ `rlm_runtime::RlmListSubagentsResult` does not have this field
          |
          = note: available fields are: `subagents`
      
      
  L476     struct `rlm_runtime::RlmListSubagentsResult` has no field named `depth`
    --- rendered ---
      error[E0560]: struct `rlm_runtime::RlmListSubagentsResult` has no field named `depth`
         --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:476:13
          |
      476 |             depth: self.rlm_depth,
          |             ^^^^^ `rlm_runtime::RlmListSubagentsResult` does not have this field
          |
          = note: available fields are: `subagents`
      
      
  L493     no field `agents` on type `rlm_runtime::RlmListSubagentsResult`
    --- rendered ---
      error[E0609]: no field `agents` on type `rlm_runtime::RlmListSubagentsResult`
         --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:493:14
          |
      493 |             .agents
          |              ^^^^^^ unknown field
          |
          = note: available field is: `subagents`
      
      
  L509     struct `rlm_runtime::RlmDeleteSubagentResult` has no field named `deleted`
    --- rendered ---
      error[E0560]: struct `rlm_runtime::RlmDeleteSubagentResult` has no field named `deleted`
         --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:509:21
          |
      509 |                     deleted: false,
          |                     ^^^^^^^ `rlm_runtime::RlmDeleteSubagentResult` does not have this field
          |
          = note: available fields are: `subagent`, `outcome`
      
      
  L510     struct `rlm_runtime::RlmDeleteSubagentResult` has no field named `child_id`
    --- rendered ---
      error[E0560]: struct `rlm_runtime::RlmDeleteSubagentResult` has no field named `child_id`
         --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:510:21
          |
      510 |                     child_id: None,
          |                     ^^^^^^^^ `rlm_runtime::RlmDeleteSubagentResult` does not have this field
          |
          = note: available fields are: `subagent`, `outcome`
      
      
  L511     struct `rlm_runtime::RlmDeleteSubagentResult` has no field named `message`
    --- rendered ---
      error[E0560]: struct `rlm_runtime::RlmDeleteSubagentResult` has no field named `message`
         --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:511:21
          |
      511 |                     message: Some(format!("No subagent matched {target}")),
          |                     ^^^^^^^ `rlm_runtime::RlmDeleteSubagentResult` does not have this field
          |
          = note: available fields are: `subagent`, `outcome`
      
      
  L518     struct `rlm_runtime::RlmDeleteSubagentResult` has no field named `deleted`
    --- rendered ---
      error[E0560]: struct `rlm_runtime::RlmDeleteSubagentResult` has no field named `deleted`
         --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:518:21
          |
      518 |                     deleted: false,
          |                     ^^^^^^^ `rlm_runtime::RlmDeleteSubagentResult` does not have this field
          |
          = note: available fields are: `subagent`, `outcome`
      
      
  L519     struct `rlm_runtime::RlmDeleteSubagentResult` has no field named `child_id`
    --- rendered ---
      error[E0560]: struct `rlm_runtime::RlmDeleteSubagentResult` has no field named `child_id`
         --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:519:21
          |
      519 |                     child_id: Some(resolved.rlm_child_id.clone()),
          |                     ^^^^^^^^ `rlm_runtime::RlmDeleteSubagentResult` does not have this field
          |
          = note: available fields are: `subagent`, `outcome`
      
      
  L520     struct `rlm_runtime::RlmDeleteSubagentResult` has no field named `message`
    --- rendered ---
      error[E0560]: struct `rlm_runtime::RlmDeleteSubagentResult` has no field named `message`
         --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:520:21
          |
      520 |                     message: Some(format!(
          |                     ^^^^^^^ `rlm_runtime::RlmDeleteSubagentResult` does not have this field
          |
          = note: available fields are: `subagent`, `outcome`
      
      
  L540     struct `rlm_runtime::RlmDeleteSubagentResult` has no field named `deleted`
    --- rendered ---
      error[E0560]: struct `rlm_runtime::RlmDeleteSubagentResult` has no field named `deleted`
         --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:540:21
          |
      540 |                     deleted: false,
          |                     ^^^^^^^ `rlm_runtime::RlmDeleteSubagentResult` does not have this field
          |
          = note: available fields are: `subagent`, `outcome`
      
      
  L541     struct `rlm_runtime::RlmDeleteSubagentResult` has no field named `child_id`
    --- rendered ---
      error[E0560]: struct `rlm_runtime::RlmDeleteSubagentResult` has no field named `child_id`
         --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:541:21
          |
      541 |                     child_id: None,
          |                     ^^^^^^^^ `rlm_runtime::RlmDeleteSubagentResult` does not have this field
          |
          = note: available fields are: `subagent`, `outcome`
      
      
  L542     struct `rlm_runtime::RlmDeleteSubagentResult` has no field named `message`
    --- rendered ---
      error[E0560]: struct `rlm_runtime::RlmDeleteSubagentResult` has no field named `message`
         --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:542:21
          |
      542 |                     message: Some(format!("No subagent matched {target}")),
          |                     ^^^^^^^ `rlm_runtime::RlmDeleteSubagentResult` does not have this field
          |
          = note: available fields are: `subagent`, `outcome`
      
      
  L588     struct `rlm_runtime::RlmSubagentRegistryEntry` has no field named `child_id`
    --- rendered ---
      error[E0560]: struct `rlm_runtime::RlmSubagentRegistryEntry` has no field named `child_id`
         --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:588:13
          |
      588 |             child_id: run.id.clone(),
          |             ^^^^^^^^ `rlm_runtime::RlmSubagentRegistryEntry` does not have this field
          |
          = note: available fields are: `rlm_child_id`, `active_session_id`, `session_id`, `session_dir`
      
      
  L591     struct `rlm_runtime::RlmSubagentRegistryEntry` has no field named `label`
    --- rendered ---
      error[E0609]: no field `label` on type `&agent_session::RlmChildRun`
         --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:591:24
          |
      591 |             label: run.label.clone(),
          |                        ^^^^^ unknown field
          |
          = note: available fields are: `id`, `prompt`, `session_name`, `session_dir`, `model` ... and 27 others
      
      
  L591     no field `label` on type `&agent_session::RlmChildRun`
    --- rendered ---
      error[E0609]: no field `label` on type `&agent_session::RlmChildRun`
         --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:591:24
          |
      591 |             label: run.label.clone(),
          |                        ^^^^^ unknown field
          |
          = note: available fields are: `id`, `prompt`, `session_name`, `session_dir`, `model` ... and 27 others
      
      
  L592     struct `rlm_runtime::RlmSubagentRegistryEntry` has no field named `model`
    --- rendered ---
      error[E0560]: struct `rlm_runtime::RlmSubagentRegistryEntry` has no field named `model`
         --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:592:13
          |
      592 |             model: run.model.clone(),
          |             ^^^^^ `rlm_runtime::RlmSubagentRegistryEntry` does not have this field
          |
          = note: available fields are: `rlm_child_id`, `active_session_id`, `session_id`, `session_dir`
      
      
  L639     mismatched types
    --- rendered ---
      error[E0308]: mismatched types
         --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:639:27
          |
      639 |             session_name: subagent.session_name.clone(),
          |                           ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ expected `Option<String>`, found `String`
          |
          = note: expected enum `std::option::Option<std::string::String>`
                   found struct `std::string::String`
      help: try wrapping the expression in `Some`
          |
      639 |             session_name: Some(subagent.session_name.clone()),
          |                           +++++                             +
      
      
  L679     struct `rlm_runtime::RlmDeleteSubagentResult` has no field named `deleted`
    --- rendered ---
      error[E0560]: struct `rlm_runtime::RlmDeleteSubagentResult` has no field named `deleted`
         --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:679:13
          |
      679 |             deleted: true,
          |             ^^^^^^^ `rlm_runtime::RlmDeleteSubagentResult` does not have this field
          |
          = note: available fields are: `subagent`, `outcome`
      
      
  L680     struct `rlm_runtime::RlmDeleteSubagentResult` has no field named `child_id`
    --- rendered ---
      error[E0560]: struct `rlm_runtime::RlmDeleteSubagentResult` has no field named `child_id`
         --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:680:13
          |
      680 |             child_id: Some(child_id),
          |             ^^^^^^^^ `rlm_runtime::RlmDeleteSubagentResult` does not have this field
          |
          = note: available fields are: `subagent`, `outcome`
      
      
  L681     struct `rlm_runtime::RlmDeleteSubagentResult` has no field named `message`
    --- rendered ---
      error[E0560]: struct `rlm_runtime::RlmDeleteSubagentResult` has no field named `message`
         --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:681:13
          |
      681 |             message: None,
          |             ^^^^^^^ `rlm_runtime::RlmDeleteSubagentResult` does not have this field
          |
          = note: available fields are: `subagent`, `outcome`
      
      
  L705     struct `agent_session::RlmChildAgentSnapshot` has no field named `child_id`
    --- rendered ---
      error[E0560]: struct `agent_session::RlmChildAgentSnapshot` has no field named `child_id`
         --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:705:13
          |
      705 |             child_id: run.id.clone(),
          |             ^^^^^^^^ `agent_session::RlmChildAgentSnapshot` does not have this field
          |
          = note: available fields are: `id`, `parent_id`, `active_session_id`, `duration_ms`, `answer_preview` ... and 6 others
      
      
  L706     mismatched types
    --- rendered ---
      error[E0308]: mismatched types
         --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:706:27
          |
      706 |             session_name: run.session_name.clone(),
          |                           ^^^^^^^^^^^^^^^^^^^^^^^^ expected `Option<String>`, found `String`
          |
          = note: expected enum `std::option::Option<std::string::String>`
                   found struct `std::string::String`
      help: try wrapping the expression in `Some`
          |
      706 |             session_name: Some(run.session_name.clone()),
          |                           +++++                        +
      
      
  L708     no field `label` on type `&agent_session::RlmChildRun`
    --- rendered ---
      error[E0609]: no field `label` on type `&agent_session::RlmChildRun`
         --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:708:24
          |
      708 |             label: run.label.clone(),
          |                        ^^^^^ unknown field
          |
          = note: available fields are: `id`, `prompt`, `session_name`, `session_dir`, `model` ... and 27 others
      
      
  L709     mismatched types
    --- rendered ---
      error[E0308]: mismatched types
         --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:709:20
          |
      709 |             model: run.model.clone(),
          |                    ^^^^^^^^^^^^^^^^^ expected `Option<String>`, found `Option<Model>`
          |
          = note: expected enum `std::option::Option<std::string::String>`
                     found enum `std::option::Option<pi_ai::index::Model>`
      
      
  L710     struct `agent_session::RlmChildAgentSnapshot` has no field named `depth`
    --- rendered ---
      error[E0609]: no field `depth` on type `&agent_session::RlmChildRun`
         --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:710:24
          |
      710 |             depth: run.depth,
          |                        ^^^^^ unknown field
          |
          = note: available fields are: `id`, `prompt`, `session_name`, `session_dir`, `model` ... and 27 others
      
      
  L710     no field `depth` on type `&agent_session::RlmChildRun`
    --- rendered ---
      error[E0609]: no field `depth` on type `&agent_session::RlmChildRun`
         --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:710:24
          |
      710 |             depth: run.depth,
          |                        ^^^^^ unknown field
          |
          = note: available fields are: `id`, `prompt`, `session_name`, `session_dir`, `model` ... and 27 others
      
      
  L718     struct `agent_session::RlmChildAgentSnapshot` has no field named `child_id`
    --- rendered ---
      error[E0560]: struct `agent_session::RlmChildAgentSnapshot` has no field named `child_id`
         --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:718:13
          |
      718 |             child_id: child_id.to_string(),
          |             ^^^^^^^^ `agent_session::RlmChildAgentSnapshot` does not have this field
          |
          = note: available fields are: `id`, `parent_id`, `active_session_id`, `duration_ms`, `answer_preview` ... and 6 others
      
      
  L721     mismatched types
    --- rendered ---
      error[E0308]: mismatched types
         --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:721:20
          |
      721 |             label: None,
          |                    ^^^^ expected `String`, found `Option<_>`
          |
          = note: expected struct `std::string::String`
                       found enum `std::option::Option<_>`
      
      
  L722     no field `id` on type `std::option::Option<pi_ai::index::Model>`
    --- rendered ---
      error[E0609]: no field `id` on type `std::option::Option<pi_ai::index::Model>`
         --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:722:39
          |
      722 |             model: Some(child.model().id.clone()),
          |                                       ^^ unknown field
          |
      help: one of the expressions' fields has a field of the same name
          |
      722 |             model: Some(child.model().unwrap().id.clone()),
          |                                       +++++++++
      
      
  L723     struct `agent_session::RlmChildAgentSnapshot` has no field named `depth`
    --- rendered ---
      error[E0560]: struct `agent_session::RlmChildAgentSnapshot` has no field named `depth`
         --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:723:13
          |
      723 |             depth: child.rlm_depth(),
          |             ^^^^^ `agent_session::RlmChildAgentSnapshot` does not have this field
          |
          = note: available fields are: `id`, `parent_id`, `active_session_id`, `duration_ms`, `answer_preview` ... and 6 others
      
      
  L782     this function takes 2 arguments but 1 argument was supplied
    --- rendered ---
      error[E0061]: this function takes 2 arguments but 1 argument was supplied
         --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:782:24
          |
      782 |             return Err(format_agent_session_name_unavailable(name));
          |                        ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^------ argument #2 of type `f64` is missing
          |
      note: function defined here
         --> crates\pi-coding-agent\src\core\agent_messages.rs:349:8
          |
      349 | pub fn format_agent_session_name_unavailable(name: &str, depth: f64) -> String {
          |        ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^             ----------
      help: provide the argument
          |
      782 |             return Err(format_agent_session_name_unavailable(name, /* f64 */));
          |                                                                  +++++++++++
      
  L809     `match` arms have incompatible types
    --- rendered ---
      error[E0308]: `match` arms have incompatible types
         --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:809:21
          |
      804 |           let model = match requested {
          |                       --------------- `match` arms have incompatible types
      805 |               Some(requested) => models
          |  ________________________________-
      806 | |                 .iter()
      807 | |                 .find(|model| model.id == requested || model.name == requested)
      808 | |                 .cloned(),
          | |_________________________- this is found to be of type `std::option::Option<pi_ai::index::Model>`
      809 |               None => Some(self.model()),
          |                       ^^^^^^^^^^^^^^^^^^ expected `Option<Model>`, found `Option<Option<Model>>`
          |
          = note: expected enum `std::option::Option<pi_ai::index::Model>`
                     found enum `std::option::Option<std::option::Option<pi_ai::index::Model>>`
  L814     struct `agent_session::RlmSubagentModelSelection` has no field named `thinking_level`
    --- rendered ---
      error[E0560]: struct `agent_session::RlmSubagentModelSelection` has no field named `thinking_level`
         --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:814:17
          |
      814 |                 thinking_level,
          |                 ^^^^^^^^^^^^^^ `agent_session::RlmSubagentModelSelection` does not have this field
          |
          = note: all struct fields are already assigned
      
      
  L826     missing fields `id`, `spawn_code` and `spawned_by_request_id` in initializer of `agent_session::RlmSubagentRuntimeOptionsInput`
    --- rendered ---
      error[E0063]: missing fields `id`, `spawn_code` and `spawned_by_request_id` in initializer of `agent_session::RlmSubagentRuntimeOptionsInput`
         --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:826:23
          |
      826 |         let options = RlmSubagentRuntimeOptionsInput {
          |                       ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ missing `id`, `spawn_code` and `spawned_by_request_id`
      
      
  L828     mismatched types
    --- rendered ---
      error[E0308]: mismatched types
         --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:828:27
          |
      828 |               session_name: kwargs
          |  ___________________________^
      829 | |                 .get("session_name")
      830 | |                 .and_then(Value::as_str)
      831 | |                 .map(|value| value.to_string()),
          | |_______________________________________________^ expected `String`, found `Option<String>`
          |
          = note: expected struct `std::string::String`
                       found enum `std::option::Option<std::string::String>`
      help: consider using `Option::expect` to unwrap the `std::option::Option<std::string::String>` value, panicking if the value is an `Option::None`
          |
      831 |                 .map(|value| value.to_string()).expect("REASON"),
          |                                                +++++++++++++++++
  L832     mismatched types
    --- rendered ---
      error[E0308]: mismatched types
         --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:832:20
          |
      832 |               model: kwargs
          |  ____________________^
      833 | |                 .get("model")
      834 | |                 .and_then(Value::as_str)
      835 | |                 .map(|value| value.to_string()),
          | |_______________________________________________^ expected `Model`, found `Option<String>`
          |
          = note: expected struct `pi_ai::index::Model`
                       found enum `std::option::Option<std::string::String>`
      
      
  L837     mismatched types
    --- rendered ---
      error[E0308]: mismatched types
         --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:837:26
          |
      837 |               session_dir: kwargs
          |  __________________________^
      838 | |                 .get("session_dir")
      839 | |                 .and_then(Value::as_str)
      840 | |                 .map(|value| value.to_string()),
          | |_______________________________________________^ expected `String`, found `Option<String>`
          |
          = note: expected struct `std::string::String`
                       found enum `std::option::Option<std::string::String>`
      help: consider using `Option::expect` to unwrap the `std::option::Option<std::string::String>` value, panicking if the value is an `Option::None`
          |
      840 |                 .map(|value| value.to_string()).expect("REASON"),
          |                                                +++++++++++++++++
  L842     mismatched types
    --- rendered ---
      error[E0308]: mismatched types
         --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:842:56
          |
      842 |         let runtime = self.create_rlm_subagent_runtime(options).await?;
          |                            --------------------------- ^^^^^^^ expected `CreateRlmSubagentRuntimeOptions`, found `RlmSubagentRuntimeOptionsInput`
          |                            |
          |                            arguments to this method are incorrect
          |
      note: method defined here
         --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:301:25
          |
      301 |     pub(super) async fn create_rlm_subagent_runtime(
          |                         ^^^^^^^^^^^^^^^^^^^^^^^^^^^
      302 |         self: &Arc<Self>, options: CreateRlmSubagentRuntimeOptions,
          |                           ----------------------------------------
      
  L844     struct `rlm_runtime::RlmCreateSessionResult` has no field named `child_id`
    --- rendered ---
      error[E0609]: no field `runtime_id` on type `rlm_runtime::RlmSubagentRuntime`
         --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:844:31
          |
      844 |             child_id: runtime.runtime_id.clone(),
          |                               ^^^^^^^^^^ unknown field
          |
          = note: available field is: `session`
      
      
  L844     no field `runtime_id` on type `rlm_runtime::RlmSubagentRuntime`
    --- rendered ---
      error[E0609]: no field `runtime_id` on type `rlm_runtime::RlmSubagentRuntime`
         --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:844:31
          |
      844 |             child_id: runtime.runtime_id.clone(),
          |                               ^^^^^^^^^^ unknown field
          |
          = note: available field is: `session`
      
      
  L845     struct `rlm_runtime::RlmCreateSessionResult` has no field named `session_dir`
    --- rendered ---
      error[E0609]: no field `session_dir` on type `rlm_runtime::RlmSubagentRuntime`
         --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:845:34
          |
      845 |             session_dir: runtime.session_dir.clone(),
          |                                  ^^^^^^^^^^^ unknown field
          |
          = note: available field is: `session`
      
      
  L845     no field `session_dir` on type `rlm_runtime::RlmSubagentRuntime`
    --- rendered ---
      error[E0609]: no field `session_dir` on type `rlm_runtime::RlmSubagentRuntime`
         --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:845:34
          |
      845 |             session_dir: runtime.session_dir.clone(),
          |                                  ^^^^^^^^^^^ unknown field
          |
          = note: available field is: `session`
      
      
  L846     struct `rlm_runtime::RlmCreateSessionResult` has no field named `session_name`
    --- rendered ---
      error[E0560]: struct `rlm_runtime::RlmCreateSessionResult` has no field named `session_name`
         --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:846:13
          |
      846 |             session_name: None,
          |             ^^^^^^^^^^^^ unknown field
          |
      help: a field with a similar name exists
          |
      846 -             session_name: None,
      846 +             session_file: None,
          |
      
      
  L858     struct `rlm_runtime::RlmSpawnHandle` has no field named `child_id`
    --- rendered ---
      error[E0609]: no field `child_id` on type `rlm_runtime::RlmCreateSessionResult`
         --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:858:31
          |
      858 |             child_id: created.child_id,
          |                               ^^^^^^^^ unknown field
          |
          = note: available fields are: `active_session_id`, `session_id`, `name`, `session_file`, `model`
      
      
  L858     no field `child_id` on type `rlm_runtime::RlmCreateSessionResult`
    --- rendered ---
      error[E0609]: no field `child_id` on type `rlm_runtime::RlmCreateSessionResult`
         --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:858:31
          |
      858 |             child_id: created.child_id,
          |                               ^^^^^^^^ unknown field
          |
          = note: available fields are: `active_session_id`, `session_id`, `name`, `session_file`, `model`
      
      
  L859     no field `session_dir` on type `rlm_runtime::RlmCreateSessionResult`
    --- rendered ---
      error[E0609]: no field `session_dir` on type `rlm_runtime::RlmCreateSessionResult`
         --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:859:34
          |
      859 |             session_dir: created.session_dir,
          |                                  ^^^^^^^^^^^ unknown field
          |
      help: a field with a similar name exists
          |
      859 -             session_dir: created.session_dir,
      859 +             session_dir: created.session_id,
          |
      
      
  L860     struct `rlm_runtime::RlmSpawnHandle` has no field named `session_name`
    --- rendered ---
      error[E0609]: no field `session_name` on type `rlm_runtime::RlmCreateSessionResult`
         --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:860:35
          |
      860 |             session_name: created.session_name,
          |                                   ^^^^^^^^^^^^ unknown field
          |
      help: a field with a similar name exists
          |
      860 -             session_name: created.session_name,
      860 +             session_name: created.session_file,
          |
      
      
  L860     no field `session_name` on type `rlm_runtime::RlmCreateSessionResult`
    --- rendered ---
      error[E0609]: no field `session_name` on type `rlm_runtime::RlmCreateSessionResult`
         --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:860:35
          |
      860 |             session_name: created.session_name,
          |                                   ^^^^^^^^^^^^ unknown field
          |
      help: a field with a similar name exists
          |
      860 -             session_name: created.session_name,
      860 +             session_name: created.session_file,
          |
      
      
  L872     no method named `as_deref` found for struct `std::string::String` in the current scope
    --- rendered ---
      error[E0599]: no method named `as_deref` found for struct `std::string::String` in the current scope
         --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:872:32
          |
      872 |         if message.stop_reason.as_deref() == Some(STOP_REASON_ERROR) {
          |                                ^^^^^^^^
          |
      help: there is a method `as_ref` with a similar name
          |
      872 -         if message.stop_reason.as_deref() == Some(STOP_REASON_ERROR) {
      872 +         if message.stop_reason.as_ref() == Some(STOP_REASON_ERROR) {
          |
      
      
  L895     no field `details` on type `&pi_ai::index::AssistantMessage`
    --- rendered ---
      error[E0609]: no field `details` on type `&pi_ai::index::AssistantMessage`
         --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:895:14
          |
      895 |             .details
          |              ^^^^^^^ unknown field
          |
          = note: available fields are: `role`, `content`, `api`, `provider`, `model` ... and 8 others
      
      
  L905     no field `details` on type `&pi_ai::index::AssistantMessage`
    --- rendered ---
      error[E0609]: no field `details` on type `&pi_ai::index::AssistantMessage`
         --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:905:14
          |
      905 |             .details
          |              ^^^^^^^ unknown field
          |
          = note: available fields are: `role`, `content`, `api`, `provider`, `model` ... and 8 others
      
      
  L924     no field `details` on type `&pi_ai::index::AssistantMessage`
    --- rendered ---
      error[E0609]: no field `details` on type `&pi_ai::index::AssistantMessage`
         --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:924:14
          |
      924 |             .details
          |              ^^^^^^^ unknown field
          |
          = note: available fields are: `role`, `content`, `api`, `provider`, `model` ... and 8 others
      
      
  L975     no field `max_attempts` on type `ResolvedRetrySettings`
    --- rendered ---
      error[E0609]: no field `max_attempts` on type `ResolvedRetrySettings`
         --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:975:31
          |
      975 |         if attempt > settings.max_attempts {
          |                               ^^^^^^^^^^^^ unknown field
          |
          = note: available fields are: `enabled`, `max_retries`, `base_delay_ms`
      
      
  L985     no field `max_attempts` on type `ResolvedRetrySettings`
    --- rendered ---
      error[E0609]: no field `max_attempts` on type `ResolvedRetrySettings`
         --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:985:36
          |
      985 |             max_attempts: settings.max_attempts as i64,
          |                                    ^^^^^^^^^^^^ unknown field
          |
          = note: available fields are: `enabled`, `max_retries`, `base_delay_ms`
      
      
  L991     no field `delay_ms` on type `ResolvedRetrySettings`
    --- rendered ---
      error[E0609]: no field `delay_ms` on type `ResolvedRetrySettings`
         --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:991:27
          |
      991 |                 (settings.delay_ms.unwrap_or(1000.0) * attempt as f64) as u64,
          |                           ^^^^^^^^ unknown field
          |
          = note: available fields are: `enabled`, `max_retries`, `base_delay_ms`
      
      
  L1003    no field `max_attempts` on type `ResolvedRetrySettings`
    --- rendered ---
      error[E0609]: no field `max_attempts` on type `ResolvedRetrySettings`
          --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:1003:40
           |
      1003 |                 max_attempts: settings.max_attempts as i64,
           |                                        ^^^^^^^^^^^^ unknown field
           |
           = note: available fields are: `enabled`, `max_retries`, `base_delay_ms`
      
      
  L1023    the method `clone` exists for struct `MutexGuard<'_, Option<Pin<Box<dyn Future<Output = ...> + Send>>>>`, but its trait bounds were not satisfied
    --- rendered ---
      error[E0599]: the method `clone` exists for struct `MutexGuard<'_, Option<Pin<Box<dyn Future<Output = ...> + Send>>>>`, but its trait bounds were not satisfied
          --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:1023:58
           |
      1023 |         let promise = self.retry_promise.lock().unwrap().clone();
           |                                                          ^^^^^ method cannot be called due to unsatisfied trait bounds
           |
           = note: the following trait bounds were not satisfied:
                   `Pin<Box<dyn futures::Future<Output = Result<(), std::string::String>> + std::marker::Send>>: Clone`
                   which is required by `std::option::Option<Pin<Box<dyn futures::Future<Output = Result<(), std::string::String>> + std::marker::Send>>>: Clone`
           = note: the full name for the type has been written to 'C:\Users\openclawuser\optimus-rust-port\.port-env/target\debug\deps\pi_coding_agent-ab0b3e324740a12a.long-type-2172341572183619736.txt'
           = note: consider using `--verbose` to print the full type name to the console
      
      
  L1038    mismatched types
    --- rendered ---
      error[E0308]: mismatched types
          --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:1038:49
           |
      1038 |                 && (action.lifecycle.state() == "committing"
           |                     ------------------------    ^^^^^^^^^^^^ expected `ActionLifecycleState`, found `&str`
           |                     |
           |                     expected because this is `ActionLifecycleState`
      
      
  L1039    mismatched types
    --- rendered ---
      error[E0308]: mismatched types
          --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:1039:52
           |
      1039 |                     || action.lifecycle.state() == "running")
           |                        ------------------------    ^^^^^^^^^ expected `ActionLifecycleState`, found `&str`
           |                        |
           |                        expected because this is `ActionLifecycleState`
      
      
  L1183    mismatched types
    --- rendered ---
      error[E0308]: mismatched types
          --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:1183:24
           |
      1183 |             max_depth: self.rlm_max_depth(),
           |                        ^^^^^^^^^^^^^^^^^^^^ expected `f64`, found `i64`
      
      
  L1209    mismatched types
    --- rendered ---
      error[E0308]: mismatched types
          --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:1209:17
           |
      1207 |             .append_custom_message_entry(
           |              --------------------------- arguments to this method are incorrect
      1208 |                 RLM_MAX_DEPTH_STATE_CUSTOM_TYPE,
      1209 |                 CustomMessageContent::Text(max_depth.to_string()),
           |                 ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ expected `&CustomMessageEntryContent`, found `CustomMessageContent`
           |
      note: method defined here
          --> crates\pi-coding-agent\src\core\session_manager.rs:4117:12
           |
      4117 |     pub fn append_custom_message_entry(
           |            ^^^^^^^^^^^^^^^^^^^^^^^^^^^
      ...
      4120 |         content: &CustomMessageEntryContent,
  L1216    mismatched types
    --- rendered ---
      error[E0560]: struct `agent_session::SetRlmMaxDepthResult` has no field named `global`
          --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:1216:46
           |
      1216 |         Ok(SetRlmMaxDepthResult { max_depth, global })
           |                                              ^^^^^^ `agent_session::SetRlmMaxDepthResult` does not have this field
           |
           = note: available fields are: `source`, `global_saved`, `global_error`
      
      
  L1216    struct `agent_session::SetRlmMaxDepthResult` has no field named `global`
    --- rendered ---
      error[E0560]: struct `agent_session::SetRlmMaxDepthResult` has no field named `global`
          --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:1216:46
           |
      1216 |         Ok(SetRlmMaxDepthResult { max_depth, global })
           |                                              ^^^^^^ `agent_session::SetRlmMaxDepthResult` does not have this field
           |
           = note: available fields are: `source`, `global_saved`, `global_error`
      
      
  L1251    this function takes 2 arguments but 1 argument was supplied
    --- rendered ---
      error[E0061]: this function takes 2 arguments but 1 argument was supplied
          --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:1251:28
           |
      1251 |             let prepared = prepare_branch_entries(&entries);
           |                            ^^^^^^^^^^^^^^^^^^^^^^---------- argument #2 of type `f64` is missing
           |
      note: expected `&[CompactionSessionEntry]`, found `&Vec<Map<String, Value>>`
          --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:1251:51
           |
      1251 |             let prepared = prepare_branch_entries(&entries);
           |                                                   ^^^^^^^^
           = note: expected reference `&[CompactionSessionEntry]`
                      found reference `&Vec<serde_json::Map<std::string::String, serde_json::Value>>`
      note: function defined here
          --> crates\pi-coding-agent\src\core\compaction\branch_summarization.rs:216:8
           |
  L1252    no method named `is_empty` found for struct `BranchPreparation` in the current scope
    --- rendered ---
      error[E0599]: no method named `is_empty` found for struct `BranchPreparation` in the current scope
          --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:1252:26
           |
      1252 |             if !prepared.is_empty() {
           |                          ^^^^^^^^ method not found in `BranchPreparation`
           |
          ::: crates\pi-coding-agent\src\core\compaction\branch_summarization.rs:64:1
           |
        64 | pub struct BranchPreparation {
           | ---------------------------- method `is_empty` not found for this struct
           |
           = help: items from traits can only be used if the trait is implemented and in scope
           = note: the following traits define an item `is_empty`, perhaps you need to implement one of them:
                   candidate #1: `ExactSizeIterator`
                   candidate #2: `RangeBounds`
                   candidate #3: `SampleRange`
  L1255    this function takes 2 arguments but 1 argument was supplied
    --- rendered ---
      error[E0061]: this function takes 2 arguments but 1 argument was supplied
          --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:1255:30
           |
      1255 |                   let result = generate_branch_summary(GenerateBranchSummaryOptions {
           |  ______________________________^^^^^^^^^^^^^^^^^^^^^^^_-
      1256 | |                     messages: prepared,
      1257 | |                     signal: Some(controller.clone()),
      1258 | |                 })
           | |_________________- argument #1 of type `&[CompactionSessionEntry]` is missing
           |
      note: function defined here
          --> crates\pi-coding-agent\src\core\compaction\branch_summarization.rs:322:14
           |
       322 | pub async fn generate_branch_summary(
           |              ^^^^^^^^^^^^^^^^^^^^^^^
       323 |     entries: &[SessionEntry],
  L1256    struct `branch_summarization::GenerateBranchSummaryOptions` has no field named `messages`
    --- rendered ---
      error[E0560]: struct `branch_summarization::GenerateBranchSummaryOptions` has no field named `messages`
          --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:1256:21
           |
      1256 |                     messages: prepared,
           |                     ^^^^^^^^ `branch_summarization::GenerateBranchSummaryOptions` does not have this field
           |
           = note: available fields are: `model`, `api_key`, `headers`, `custom_instructions`, `replace_instructions` ... and 2 others
      
      
  L1261    mismatched types
    --- rendered ---
      error[E0308]: mismatched types
          --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:1261:24
           |
      1261 |                 if let Ok(result) = result {
           |                        ^^^^^^^^^^   ------ this expression has type `BranchSummaryResult`
           |                        |
           |                        expected `BranchSummaryResult`, found `Result<_, _>`
           |
           = note: expected struct `BranchSummaryResult`
                        found enum `Result<_, _>`
      
      
  L1293    the method `clone` exists for struct `MutexGuard<'_, Pin<Box<dyn Future<Output = Result<(), ...>> + Send>>>`, but its trait bounds were not satisfied
    --- rendered ---
      error[E0599]: the method `clone` exists for struct `MutexGuard<'_, Pin<Box<dyn Future<Output = Result<(), ...>> + Send>>>`, but its trait bounds were not satisfied
          --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:1293:69
           |
      1293 |         let previous = self.branch_navigation_queue.lock().unwrap().clone();
           |                                                                     ^^^^^ method cannot be called due to unsatisfied trait bounds
           |
           = note: the following trait bounds were not satisfied:
                   `Box<dyn futures::Future<Output = Result<(), std::string::String>> + std::marker::Send>: Clone`
                   which is required by `Pin<Box<dyn futures::Future<Output = Result<(), std::string::String>> + std::marker::Send>>: Clone`
           = note: the full name for the type has been written to 'C:\Users\openclawuser\optimus-rust-port\.port-env/target\debug\deps\pi_coding_agent-ab0b3e324740a12a.long-type-15642071286362253641.txt'
           = note: consider using `--verbose` to print the full type name to the console
      
      
  L1352    this function takes 2 arguments but 1 argument was supplied
    --- rendered ---
      error[E0061]: this function takes 2 arguments but 1 argument was supplied
          --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:1352:25
           |
      1352 |         let own_usage = compute_own_and_total_usage(&entries).0;
           |                         ^^^^^^^^^^^^^^^^^^^^^^^^^^^---------- argument #2 of type `&[ContextTreeEntry]` is missing
           |
      note: expected `&[ContextTreeEntry]`, found `&Vec<Map<String, Value>>`
          --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:1352:53
           |
      1352 |         let own_usage = compute_own_and_total_usage(&entries).0;
           |                                                     ^^^^^^^^
           = note: expected reference `&[ContextTreeEntry]`
                      found reference `&Vec<serde_json::Map<std::string::String, serde_json::Value>>`
      note: function defined here
          --> crates\pi-coding-agent\src\core\context_tree.rs:175:8
           |
  L1354    mismatched types
    --- rendered ---
      error[E0308]: mismatched types
          --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:1354:50
           |
      1354 |         let summary = session_usage_summary_from(Some(&own_usage));
           |                       -------------------------- ^^^^^^^^^^^^^^^^ expected `&Usage`, found `Option<&Usage>`
           |                       |
           |                       arguments to this function are incorrect
           |
           = note: expected reference `&pi_ai::index::Usage`
                           found enum `std::option::Option<&pi_ai::index::Usage>`
      note: function defined here
          --> crates\pi-coding-agent\src\core\usage.rs:15:8
           |
        15 | pub fn session_usage_summary_from(usage: &Usage) -> Option<SessionUsageSummary> {
           |        ^^^^^^^^^^^^^^^^^^^^^^^^^^ -------------
      
  L1356    struct `session_stats::SessionStats` has no field named `message_count`
    --- rendered ---
      error[E0560]: struct `session_stats::SessionStats` has no field named `message_count`
          --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:1356:13
           |
      1356 |             message_count: messages.len() as i64,
           |             ^^^^^^^^^^^^^ `session_stats::SessionStats` does not have this field
           |
           = note: available fields are: `session_file`, `session_id`, `user_messages`, `assistant_messages`, `tool_calls` ... and 5 others
      
      
  L1357    struct `session_stats::SessionStats` has no field named `user_message_count`
    --- rendered ---
      error[E0560]: struct `session_stats::SessionStats` has no field named `user_message_count`
          --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:1357:13
           |
      1357 |             user_message_count: messages
           |             ^^^^^^^^^^^^^^^^^^ unknown field
           |
      help: a field with a similar name exists
           |
      1357 -             user_message_count: messages
      1357 +             user_messages: messages
           |
      
      
  L1361    struct `session_stats::SessionStats` has no field named `assistant_message_count`
    --- rendered ---
      error[E0560]: struct `session_stats::SessionStats` has no field named `assistant_message_count`
          --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:1361:13
           |
      1361 |             assistant_message_count: messages
           |             ^^^^^^^^^^^^^^^^^^^^^^^ unknown field
           |
      help: a field with a similar name exists
           |
      1361 -             assistant_message_count: messages
      1361 +             assistant_messages: messages
           |
      
      
  L1365    struct `session_stats::SessionStats` has no field named `tool_call_count`
    --- rendered ---
      error[E0560]: struct `session_stats::SessionStats` has no field named `tool_call_count`
          --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:1365:13
           |
      1365 |             tool_call_count: messages
           |             ^^^^^^^^^^^^^^^ `session_stats::SessionStats` does not have this field
           |
           = note: available fields are: `session_file`, `session_id`, `user_messages`, `assistant_messages`, `tool_calls` ... and 5 others
      
      
  L1367    no variant, associated function, or constant named `Tool` found for enum `pi_ai::index::Message` in the current scope
    --- rendered ---
      error[E0599]: no variant, associated function, or constant named `Tool` found for enum `pi_ai::index::Message` in the current scope
          --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:1367:98
           |
      1367 |                 .filter(|message| matches!(message, AgentMessage::Message(pi_ai::types::Message::Tool(_))))
           |                                                                                                  ^^^^ variant, associated function, or constant not found in `pi_ai::index::Message`
      
      
  L1369    struct `session_stats::SessionStats` has no field named `usage`
    --- rendered ---
      error[E0609]: no field `total` on type `std::option::Option<usage::SessionUsageSummary>`
          --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:1369:28
           |
      1369 |             usage: summary.total.clone(),
           |                            ^^^^^ unknown field
           |
           = note: available fields are: `input_tokens`, `output_tokens`, `cost`
      
      
  L1369    no field `total` on type `std::option::Option<usage::SessionUsageSummary>`
    --- rendered ---
      error[E0609]: no field `total` on type `std::option::Option<usage::SessionUsageSummary>`
          --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:1369:28
           |
      1369 |             usage: summary.total.clone(),
           |                            ^^^^^ unknown field
           |
           = note: available fields are: `input_tokens`, `output_tokens`, `cost`
      
      
  L1370    struct `session_stats::SessionStats` has no field named `tokens_before`
    --- rendered ---
      error[E0560]: struct `session_stats::SessionStats` has no field named `tokens_before`
          --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:1370:13
           |
      1370 |             tokens_before: None,
           |             ^^^^^^^^^^^^^ `session_stats::SessionStats` does not have this field
           |
           = note: available fields are: `session_file`, `session_id`, `user_messages`, `assistant_messages`, `tool_calls` ... and 5 others
      
      
  L1382    mismatched types
    --- rendered ---
      error[E0308]: mismatched types
           --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:1382:43
            |
       1382 |         let limit = get_model_input_limit(&model);
            |                     --------------------- ^^^^^^ expected `&Model`, found `&Option<Model>`
            |                     |
            |                     arguments to this function are incorrect
            |
            = note: expected reference `&pi_ai::index::Model`
                       found reference `&std::option::Option<pi_ai::index::Model>`
      note: function defined here
           --> crates\pi-coding-agent\src\core\agent_session.rs:11531:4
            |
      11531 | fn get_model_input_limit(model: &Model) -> f64 {
            |    ^^^^^^^^^^^^^^^^^^^^^ -------------
      
  L1385    mismatched types
    --- rendered ---
      error[E0308]: mismatched types
          --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:1385:30
           |
      1385 |                 tokens: Some(tokens),
           |                         ---- ^^^^^^ expected `f64`, found `ContextUsageEstimate`
           |                         |
           |                         arguments to this enum variant are incorrect
           |
      help: the type constructed contains `ContextUsageEstimate` due to the type of the argument passed
          --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:1385:25
           |
      1385 |                 tokens: Some(tokens),
           |                         ^^^^^------^
           |                              |
           |                              this argument influences the type of `Some`
      note: tuple variant defined here
  L1391    mismatched types
    --- rendered ---
      error[E0308]: mismatched types
          --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:1391:26
           |
      1391 |             tokens: Some(tokens),
           |                     ---- ^^^^^^ expected `f64`, found `ContextUsageEstimate`
           |                     |
           |                     arguments to this enum variant are incorrect
           |
      help: the type constructed contains `ContextUsageEstimate` due to the type of the argument passed
          --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:1391:21
           |
      1391 |             tokens: Some(tokens),
           |                     ^^^^^------^
           |                          |
           |                          this argument influences the type of `Some`
      note: tuple variant defined here
  L1393    cannot divide `ContextUsageEstimate` by `f64`
    --- rendered ---
      error[E0369]: cannot divide `ContextUsageEstimate` by `f64`
          --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:1393:35
           |
      1393 |             percent: Some((tokens / limit) * 100.0),
           |                            ------ ^ ----- f64
           |                            |
           |                            ContextUsageEstimate
           |
      note: an implementation of `Div<f64>` might be missing for `ContextUsageEstimate`
          --> crates\pi-coding-agent\src\core\compaction\compaction.rs:682:1
           |
       682 | pub struct ContextUsageEstimate {
           | ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ must implement `Div<f64>`
      note: the trait `Div` must be implemented
          --> /rustc/48a229ceaefd4985c50990b14116b6d856af0985/library\core\src\ops\arith.rs:460:0
      
  L1412    mismatched types
    --- rendered ---
      error[E0308]: mismatched types
          --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:1412:35
           |
      1412 |                 .and_then(|model| model.context_window)
           |                                   ^^^^^^^^^^^^^^^^^^^^ expected `Option<_>`, found `f64`
           |
           = note: expected enum `std::option::Option<_>`
                      found type `f64`
      help: try wrapping the expression in `Some`
           |
      1412 |                 .and_then(|model| Some(model.context_window))
           |                                   +++++                    +
      
      
  L1431    mismatched types
    --- rendered ---
      error[E0308]: mismatched types
          --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:1431:21
           |
      1426 |         let mut usage = own_usage;
           |                         --------- expected due to this value
      ...
      1431 |             usage = subtract_assistant_usage(&usage, child_usage);
           |                     ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ expected `Usage`, found `()`
      
      
  L1431    mismatched types
    --- rendered ---
      error[E0308]: mismatched types
          --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:1431:21
           |
      1426 |         let mut usage = own_usage;
           |                         --------- expected due to this value
      ...
      1431 |             usage = subtract_assistant_usage(&usage, child_usage);
           |                     ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ expected `Usage`, found `()`
      
      
  L1464    no method named `get` found for struct `std::sync::Arc<ExtensionRunnerRef>` in the current scope
    --- rendered ---
      error[E0599]: no method named `get` found for struct `std::sync::Arc<ExtensionRunnerRef>` in the current scope
          --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:1464:35
           |
      1464 |         self.extension_runner_ref.get()
           |                                   ^^^
           |
           = help: items from traits can only be used if the trait is implemented and in scope
           = note: the following traits define an item `get`, perhaps you need to implement one of them:
                   candidate #1: `AuthStorageLike`
                   candidate #2: `SliceIndex`
                   candidate #3: `icu_collections::codepointtrie::cptrie::TypedCodePointTrie`
                   candidate #4: `icu_properties::names::PropertyEnumToValueNameLookup`
                   candidate #5: `icu_provider::baked::DataStore`
      help: there is a method `set` with a similar name, but with different arguments
          --> crates\pi-coding-agent\src\core\agent_session.rs:337:5
           |