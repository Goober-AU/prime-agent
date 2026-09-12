# Worklist: p-b-runtime-members

Total errors in this pack: 108  across 1 files

Read docs/PR12-LEAD-DECISIONS.md FIRST (shared root causes + hard rules).
Ownership: these files are YOURS. Do not edit any other file.

### core/agent_session/runtime_members.rs  (108 errors)
  L8       [E0308] mismatched types
  L9       [E0308] mismatched types
  L10      [E0308] mismatched types
  L53      [E0308] mismatched types
  L54      [E0308] mismatched types
  L68      [E0425] cannot find function `runtime_extension_actions` in this scope
  L68      [E0425] cannot find function `runtime_extension_context_actions` in this scope
  L81      [E0425] cannot find function `runtime_source_info` in this scope
  L85      [E0425] cannot find function `runtime_source_info` in this scope
  L123     [E0277] the trait bound `std::sync::Mutex<session_manager::SessionManager>: extensions::types::SessionManager` is not satisfied
  L124     [E0425] cannot find function, tuple struct or tuple variant `RuntimeModelRegistry` in this scope
  L317     [E0308] mismatched types
  L451     [E0560] struct `rlm_runtime::RlmSubagentRegistryEntry` has no field named `child_id`
  L454     [E0560] struct `rlm_runtime::RlmSubagentRegistryEntry` has no field named `label`
  L454     [E0609] no field `label` on type `&agent_session::RlmChildRun`
  L455     [E0560] struct `rlm_runtime::RlmSubagentRegistryEntry` has no field named `model`
  L459     [E0609] no field `subagents` on type `agent_messages::AgentSessionMessageListResult`
  L464     [E0560] struct `rlm_runtime::RlmSubagentRegistryEntry` has no field named `child_id`
  L467     [E0560] struct `rlm_runtime::RlmSubagentRegistryEntry` has no field named `label`
  L468     [E0560] struct `rlm_runtime::RlmSubagentRegistryEntry` has no field named `model`
  L474     [E0560] struct `rlm_runtime::RlmListSubagentsResult` has no field named `agents`
  L475     [E0560] struct `rlm_runtime::RlmListSubagentsResult` has no field named `max_depth`
  L476     [E0560] struct `rlm_runtime::RlmListSubagentsResult` has no field named `depth`
  L493     [E0609] no field `agents` on type `rlm_runtime::RlmListSubagentsResult`
  L509     [E0560] struct `rlm_runtime::RlmDeleteSubagentResult` has no field named `deleted`
  L510     [E0560] struct `rlm_runtime::RlmDeleteSubagentResult` has no field named `child_id`
  L511     [E0560] struct `rlm_runtime::RlmDeleteSubagentResult` has no field named `message`
  L518     [E0560] struct `rlm_runtime::RlmDeleteSubagentResult` has no field named `deleted`
  L519     [E0560] struct `rlm_runtime::RlmDeleteSubagentResult` has no field named `child_id`
  L520     [E0560] struct `rlm_runtime::RlmDeleteSubagentResult` has no field named `message`
  L540     [E0560] struct `rlm_runtime::RlmDeleteSubagentResult` has no field named `deleted`
  L541     [E0560] struct `rlm_runtime::RlmDeleteSubagentResult` has no field named `child_id`
  L542     [E0560] struct `rlm_runtime::RlmDeleteSubagentResult` has no field named `message`
  L588     [E0560] struct `rlm_runtime::RlmSubagentRegistryEntry` has no field named `child_id`
  L591     [E0560] struct `rlm_runtime::RlmSubagentRegistryEntry` has no field named `label`
  L591     [E0609] no field `label` on type `&agent_session::RlmChildRun`
  L592     [E0560] struct `rlm_runtime::RlmSubagentRegistryEntry` has no field named `model`
  L639     [E0308] mismatched types
  L679     [E0560] struct `rlm_runtime::RlmDeleteSubagentResult` has no field named `deleted`
  L680     [E0560] struct `rlm_runtime::RlmDeleteSubagentResult` has no field named `child_id`
  L681     [E0560] struct `rlm_runtime::RlmDeleteSubagentResult` has no field named `message`
  L705     [E0560] struct `agent_session::RlmChildAgentSnapshot` has no field named `child_id`
  L706     [E0308] mismatched types
  L708     [E0609] no field `label` on type `&agent_session::RlmChildRun`
  L709     [E0308] mismatched types
  L710     [E0560] struct `agent_session::RlmChildAgentSnapshot` has no field named `depth`
  L710     [E0609] no field `depth` on type `&agent_session::RlmChildRun`
  L718     [E0560] struct `agent_session::RlmChildAgentSnapshot` has no field named `child_id`
  L721     [E0308] mismatched types
  L722     [E0609] no field `id` on type `std::option::Option<pi_ai::index::Model>`
  L723     [E0560] struct `agent_session::RlmChildAgentSnapshot` has no field named `depth`
  L782     [E0061] this function takes 2 arguments but 1 argument was supplied
  L809     [E0308] `match` arms have incompatible types
  L814     [E0560] struct `agent_session::RlmSubagentModelSelection` has no field named `thinking_level`
  L826     [E0063] missing fields `id`, `spawn_code` and `spawned_by_request_id` in initializer of `agent_session::RlmSubagentRuntimeOptionsInput`
  L828     [E0308] mismatched types
  L832     [E0308] mismatched types
  L837     [E0308] mismatched types
  L842     [E0308] mismatched types
  L844     [E0560] struct `rlm_runtime::RlmCreateSessionResult` has no field named `child_id`
  L844     [E0609] no field `runtime_id` on type `rlm_runtime::RlmSubagentRuntime`
  L845     [E0560] struct `rlm_runtime::RlmCreateSessionResult` has no field named `session_dir`
  L845     [E0609] no field `session_dir` on type `rlm_runtime::RlmSubagentRuntime`
  L846     [E0560] struct `rlm_runtime::RlmCreateSessionResult` has no field named `session_name`
  L858     [E0560] struct `rlm_runtime::RlmSpawnHandle` has no field named `child_id`
  L858     [E0609] no field `child_id` on type `rlm_runtime::RlmCreateSessionResult`
  L859     [E0609] no field `session_dir` on type `rlm_runtime::RlmCreateSessionResult`
  L860     [E0560] struct `rlm_runtime::RlmSpawnHandle` has no field named `session_name`
  L860     [E0609] no field `session_name` on type `rlm_runtime::RlmCreateSessionResult`
  L872     [E0599] no method named `as_deref` found for struct `std::string::String` in the current scope
  L895     [E0609] no field `details` on type `&pi_ai::index::AssistantMessage`
  L905     [E0609] no field `details` on type `&pi_ai::index::AssistantMessage`
  L924     [E0609] no field `details` on type `&pi_ai::index::AssistantMessage`
  L975     [E0609] no field `max_attempts` on type `ResolvedRetrySettings`
  L985     [E0609] no field `max_attempts` on type `ResolvedRetrySettings`
  L991     [E0609] no field `delay_ms` on type `ResolvedRetrySettings`
  L1003    [E0609] no field `max_attempts` on type `ResolvedRetrySettings`
  L1023    [E0599] the method `clone` exists for struct `MutexGuard<'_, Option<Pin<Box<dyn Future<Output = ...> + Send>>>>`, but its trait bounds were not satisfied
  L1038    [E0308] mismatched types
  L1039    [E0308] mismatched types
  L1183    [E0308] mismatched types
  L1209    [E0308] mismatched types
  L1216    [E0308] mismatched types
  L1216    [E0560] struct `agent_session::SetRlmMaxDepthResult` has no field named `global`
  L1251    [E0061] this function takes 2 arguments but 1 argument was supplied
  L1252    [E0599] no method named `is_empty` found for struct `BranchPreparation` in the current scope
  L1255    [E0061] this function takes 2 arguments but 1 argument was supplied
  L1256    [E0560] struct `branch_summarization::GenerateBranchSummaryOptions` has no field named `messages`
  L1261    [E0308] mismatched types
  L1293    [E0599] the method `clone` exists for struct `MutexGuard<'_, Pin<Box<dyn Future<Output = Result<(), ...>> + Send>>>`, but its trait bounds were not satisfied
  L1352    [E0061] this function takes 2 arguments but 1 argument was supplied
  L1354    [E0308] mismatched types
  L1356    [E0560] struct `session_stats::SessionStats` has no field named `message_count`
  L1357    [E0560] struct `session_stats::SessionStats` has no field named `user_message_count`
  L1361    [E0560] struct `session_stats::SessionStats` has no field named `assistant_message_count`
  L1365    [E0560] struct `session_stats::SessionStats` has no field named `tool_call_count`
  L1367    [E0599] no variant, associated function, or constant named `Tool` found for enum `pi_ai::index::Message` in the current scope
  L1369    [E0560] struct `session_stats::SessionStats` has no field named `usage`
  L1369    [E0609] no field `total` on type `std::option::Option<usage::SessionUsageSummary>`
  L1370    [E0560] struct `session_stats::SessionStats` has no field named `tokens_before`
  L1382    [E0308] mismatched types
  L1385    [E0308] mismatched types
  L1391    [E0308] mismatched types
  L1393    [E0369] cannot divide `ContextUsageEstimate` by `f64`
  L1412    [E0308] mismatched types
  L1431    [E0308] mismatched types
  L1431    [E0308] mismatched types
  L1464    [E0599] no method named `get` found for struct `std::sync::Arc<ExtensionRunnerRef>` in the current scope
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
    --- rendered ---
      error[E0425]: cannot find function `runtime_extension_actions` in this scope
        --> crates\pi-coding-agent\src\core\agent_session\runtime_members.rs:68:26
         |
      68 |         runner.bind_core(runtime_extension_actions(weak.clone()), runtime_extension_context_actions(weak), None);
         |                          ^^^^^^^^^^^^^^^^^^^^^^^^^ not found in this scope