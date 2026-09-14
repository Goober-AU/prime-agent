# Worklist: p-c-daemon-mode

Total errors in this pack: 223  across 1 files

Read docs/PR12-LEAD-DECISIONS.md FIRST (shared root causes + hard rules).
Ownership: these files are YOURS. Do not edit any other file.

### modes/daemon/daemon_mode.rs  (223 errors)
  L728     [E0308] mismatched types
  L1599    [E0599] the method `to_string` exists for reference `&DaemonClientCapability`, but its trait bounds were not satisfied
  L1603    [E0308] mismatched types
  L1660    [E0308] mismatched types
  L1823    [E0609] no field `path` on type `ResolvedSession`
  L1857    [E0308] mismatched types
  L2048    [E0308] mismatched types
  L2095    [E0061] this function takes 3 arguments but 2 arguments were supplied
  L2475    [None] future cannot be sent between threads safely
  L2522    [E0061] this function takes 4 arguments but 7 arguments were supplied
  L2540    [E0599] no method named `connect` found for struct `DaemonClient` in the current scope
  L2579    [E0061] this function takes 2 arguments but 1 argument was supplied
  L2580    [E0308] mismatched types
  L2581    [E0308] mismatched types
  L2584    [E0599] no variant, associated function, or constant named `Failed` found for enum `dir_lock::DirLockAttempt` in the current scope
  L2624    [E0308] mismatched types
  L2632    [E0599] no method named `pid` found for struct `ChildProcessHandle` in the current scope
  L2721    [E0599] no method named `lock` found for reference `&std::sync::Arc<DaemonSessionState>` in the current scope
  L2727    [E0277] a value of type `Vec<std::sync::Arc<std::sync::Mutex<ActiveSessionState>>>` cannot be built from an iterator over elements of type `std::sync::Arc<DaemonSession
  L2816    [E0308] mismatched types
  L3373    [E0599] no method named `clone` found for struct `std::sync::Mutex<T>` in the current scope
  L3925    [E0308] mismatched types
  L3948    [E0599] no method named `contains_key` found for reference `&serde_json::Value` in the current scope
  L3964    [E0308] arguments to this method are incorrect
  L3979    [E0061] this function takes 3 arguments but 2 arguments were supplied
  L3981    [E0599] no method named `unwrap_or_default` found for struct `Vec<session_manager::SessionInfo>` in the current scope
  L3983    [E0308] mismatched types
  L3985    [E0599] no method named `unwrap_or_default` found for struct `Vec<session_manager::SessionInfo>` in the current scope
  L3987    [E0308] arguments to this method are incorrect
  L4010    [E0308] mismatched types
  L4074    [E0308] mismatched types
  L4084    [E0599] no associated function or constant named `list_with_callbacks` found for struct `session_manager::SessionManager` in the current scope
  L4088    [E0308] mismatched types
  L4089    [E0308] mismatched types
  L4095    [E0599] no associated function or constant named `list_all_with_callbacks` found for struct `session_manager::SessionManager` in the current scope
  L4097    [E0308] mismatched types
  L4098    [E0308] mismatched types
  L4108    [E0308] mismatched types
  L4124    [E0631] type mismatch in function arguments
  L4124    [E0599] the method `collect` exists for struct `Map<Iter<'_, SessionInfo>, ...>`, but its trait bounds were not satisfied
  L4163    [E0277] a value of type `&std::collections::HashSet<std::string::String>` cannot be built from an iterator over elements of type `std::string::String`
  L4187    [E0308] mismatched types
  L4217    [E0369] binary operation `!=` cannot be applied to type `std::option::Option<&std::sync::Arc<DaemonSessionState>>`
  L4241    [E0308] mismatched types
  L4419    [E0063] missing field `parent_session_id` in initializer of `AgentSessionNameAvailabilityInput`
  L4421    [E0308] mismatched types
  L4428    [E0593] closure is expected to take 1 argument, but it takes 0 arguments
  L4428    [E0593] closure is expected to take 1 argument, but it takes 0 arguments
  L4429    [E0308] mismatched types
  L4437    [E0061] this function takes 3 arguments but 1 argument was supplied
  L4438    [E0599] no method named `lock` found for enum `Result<T, E>` in the current scope
  L4444    [E0593] closure is expected to take 1 argument, but it takes 0 arguments
  L4479    [E0308] mismatched types
  L4489    [E0308] mismatched types
  L4490    [E0308] mismatched types
  L4498    [E0615] attempted to take value of method `ok` on type `Result<(), std::string::String>`
  L4750    [E0063] missing field `sender` in initializer of `SendAgentMessageInput`
  L4756    [E0308] mismatched types
  L4758    [E0308] mismatched types
  L4759    [E0308] mismatched types
  L4990    [E0599] the method `clone` exists for struct `Pin<Box<dyn Future<Output = Result<Value, String>> + Send>>`, but its trait bounds were not satisfied
  L5026    [E0599] no method named `lock` found for reference `&std::sync::Arc<DaemonSessionState>` in the current scope
  L5229    [E0308] mismatched types
  L5232    [E0308] mismatched types
  L5258    [E0061] this function takes 2 arguments but 1 argument was supplied
  L5409    [E0308] mismatched types
  L5448    [E0308] mismatched types
  L5533    [E0277] the trait bound `ModelIdentity: serde::Serialize` is not satisfied
  L5551    [E0277] the trait bound `ModelIdentity: serde::Serialize` is not satisfied
  L5598    [E0308] mismatched types
  L5717    [E0599] no method named `runtime_switch_session` found for reference `&std::sync::Arc<AgentDaemon>` in the current scope
  L5740    [E0599] no method named `runtime_fork` found for reference `&std::sync::Arc<AgentDaemon>` in the current scope
  L5785    [E0599] no method named `runtime_import_from_jsonl` found for reference `&std::sync::Arc<AgentDaemon>` in the current scope
  L5875    [E0277] the trait bound `SessionTreeFlatNode: serde::Serialize` is not satisfied
  L5927    [E0277] the trait bound `tool_definition::ToolDefinition: serde::de::DeserializeOwned` is not satisfied
  L5935    [E0599] no method named `append_label_change` found for struct `std::sync::Arc<(dyn DaemonSession + 'static)>` in the current scope
  L6146    [E0308] mismatched types
  L6147    [E0308] `if` and `else` have incompatible types
  L6148    [E0061] this function takes 2 arguments but 1 argument was supplied
  L6150    [E0308] mismatched types
  L6152    [E0308] mismatched types
  L6155    [E0061] this function takes 3 arguments but 2 arguments were supplied
  L6155    [E0277] `Result<std::option::Option<SessionLease>, AcquireSessionLeaseError>` is not a future
  L6208    [E0596] cannot borrow `lease` as mutable, as it is not declared as mutable
  L6331    [E0609] no field `kind` on type `std::option::Option<active_session_state::AgentSessionRuntimeMetadata>`
  L6336    [E0061] this method takes 1 argument but 2 arguments were supplied
  L6361    [E0631] type mismatch in closure arguments
  L6363    [E0308] mismatched types
  L6404    [E0308] mismatched types
  L6415    [E0609] no field `kind` on type `std::option::Option<active_session_state::AgentSessionRuntimeMetadata>`
  L6439    [E0308] mismatched types
  L6457    [E0609] no field `kind` on type `std::option::Option<active_session_state::AgentSessionRuntimeMetadata>`
  L6472    [E0061] this method takes 2 arguments but 0 arguments were supplied
  L6474    [E0308] mismatched types
  L6478    [E0308] mismatched types
  L6560    [E0599] no method named `set_exec_env_provider` found for struct `std::sync::Arc<(dyn DaemonSession + 'static)>` in the current scope
  L6567    [E0599] no method named `set_runtime_env_scope` found for struct `std::sync::Arc<(dyn DaemonSession + 'static)>` in the current scope
  L6571    [E0599] no method named `set_subagent_runtime_host` found for struct `std::sync::Arc<(dyn DaemonSession + 'static)>` in the current scope
  L6579    [E0277] `dyn FnOnce() + std::marker::Send` cannot be shared between threads safely
  L6583    [E0599] no method named `set_rebind_session` found for struct `std::sync::Arc<(dyn DaemonSession + 'static)>` in the current scope
  L6590    [E0599] no method named `bind_extensions` found for struct `std::sync::Arc<(dyn DaemonSession + 'static)>` in the current scope
  L6622    [E0061] this function takes 2 arguments but 1 argument was supplied
  L6622    [E0308] mismatched types
  L7089    [E0599] no variant, associated function, or constant named `default` found for enum `pi_agent_core::types::AgentMessage` in the current scope
  L7137    [E0599] no method named `clone_arc` found for reference `&AgentDaemon` in the current scope
  L7169    [E0308] mismatched types
  L7233    [E0599] no method named `begin_replacement_snapshot` found for reference `&AgentDaemon` in the current scope
  L7362    [E0308] mismatched types
  L7457    [E0599] no method named `clone_arc` found for reference `&AgentDaemon` in the current scope
  L7642    [E0599] no method named `schedule_roster_flush` found for reference `&AgentDaemon` in the current scope
  L7673    [E0599] no method named `schedule_roster_flush` found for reference `&AgentDaemon` in the current scope
  L7724    [E0308] mismatched types
  L7725    [E0308] mismatched types
  L7726    [E0308] mismatched types
  L7727    [E0308] mismatched types
  L7777    [E0308] mismatched types
  L7778    [E0308] mismatched types
  L7818    [E0308] mismatched types
  L7849    [E0308] mismatched types
  L8020    [E0308] mismatched types
  L8122    [E0615] attempted to take value of method `role` on type `&pi_agent_core::types::AgentMessage`
  L8122    [E0615] attempted to take value of method `role` on type `&pi_agent_core::types::AgentMessage`
  L8122    [E0609] no field `timestamp` on type `&pi_agent_core::types::AgentMessage`
  L8122    [E0609] no field `timestamp` on type `&pi_agent_core::types::AgentMessage`
  L8129    [E0061] this function takes 1 argument but 2 arguments were supplied
  L8270    [E0596] cannot borrow data in an `Arc` as mutable
  L8301    [E0277] the trait bound `pi_agent_core::types::AgentMessage: std::default::Default` is not satisfied
  L8362    [E0308] mismatched types
  L8454    [E0658] use of unstable library feature `iter_next_chunk`
  L8454    [E0308] mismatched types
  L8467    [E0277] the size for values of type `[u8]` cannot be known at compilation time
  L8468    [E0277] the size for values of type `[u8]` cannot be known at compilation time
  L8468    [E0277] the size for values of type `[u8]` cannot be known at compilation time
  L8500    [E0308] mismatched types
  L8531    [E0308] arguments to this function are incorrect
  L8537    [E0599] no method named `as_object_mut` found for struct `agent_connection::types::AgentConnectionState` in the current scope
  L8547    [E0599] no method named `as_object_mut` found for struct `agent_connection::types::AgentConnectionState` in the current scope
  L8551    [E0308] mismatched types
  L8592    [E0061] this function takes 11 arguments but 9 arguments were supplied
  L8801    [E0308] mismatched types
  L8954    [E0599] no method named `acquire` found for struct `MutationDrainLatch` in the current scope
  L9082    [E0277] the trait bound `active_session_state::AgentSessionRuntimeMetadata: serde::Serialize` is not satisfied
  L9158    [E0308] mismatched types
  L9279    [E0061] this method takes 1 argument but 2 arguments were supplied
  L9363    [E0599] no method named `abort_for_update_restart` found for struct `std::sync::Arc<(dyn DaemonSession + 'static)>` in the current scope
  L9392    [E0308] mismatched types
  L9403    [E0609] no field `state` on type `&std::sync::Arc<DaemonSocketClient>`
  L9408    [E0308] mismatched types
  L9447    [E0308] mismatched types
  L9449    [E0277] `session_file_actions::DeleteSessionFileResult` is not a future
  L9515    [E0609] no field `state` on type `&std::sync::Arc<DaemonSocketClient>`
  L9639    [E0382] the type `Arc` does not implement `Copy`
  L9642    [E0382] use of moved value: `runnable_job`
  L9747    [E0599] no method named `cwd` found for struct `std::sync::Arc<DaemonSessionState>` in the current scope
  L9780    [E0599] no method named `cwd` found for struct `std::sync::Arc<DaemonSessionState>` in the current scope
  L9837    [E0308] mismatched types
  L9845    [E0609] no field `active_session_id` on type `Result<cron_jobs::AgentCronJob, std::string::String>`
  L9850    [E0609] no field `session_id` on type `Result<cron_jobs::AgentCronJob, std::string::String>`
  L9851    [E0609] no field `session_file` on type `Result<cron_jobs::AgentCronJob, std::string::String>`
  L9852    [E0609] no field `cwd` on type `Result<cron_jobs::AgentCronJob, std::string::String>`
  L9852    [E0599] no method named `cwd` found for struct `std::sync::Arc<DaemonSessionState>` in the current scope
  L9853    [E0609] no field `runtime_kind` on type `Result<cron_jobs::AgentCronJob, std::string::String>`
  L9855    [E0308] mismatched types
  L9866    [E0061] this method takes 3 arguments but 1 argument was supplied
  L9873    [E0308] mismatched types
  L9883    [E0308] mismatched types
  L9892    [E0061] this method takes 3 arguments but 1 argument was supplied
  L9921    [E0599] no method named `get` found for struct `daemon_session_list::SessionSummary` in the current scope
  L9926    [E0599] no method named `get` found for struct `daemon_session_list::SessionSummary` in the current scope
  L9979    [E0599] no method named `cwd` found for struct `std::sync::Arc<DaemonSessionState>` in the current scope
  L10071   [E0308] mismatched types
  L10073   [E0277] `session_file_actions::DeleteSessionFileResult` is not a future
  L10193   [E0308] mismatched types
  L10408   [E0599] no method named `rlm_spawn_ledger` found for reference `&AgentDaemon` in the current scope
  L10482   [E0308] mismatched types
  L10636   [E0507] cannot move out of an `Arc`
  L10875   [E0609] no field `entry` on type `RlmSubagentDisplayEntry`
  L11109   [E0308] arguments to this function are incorrect
  L11239   [E0507] cannot move out of an `Arc`
  L11666   [E0609] no field `last_activity_at` on type `SessionPassivationSnapshot`
  L11667   [E0609] no field `last_activity_at` on type `SessionPassivationSnapshot`
  L12133   [E0599] no method named `cwd` found for struct `std::sync::Arc<DaemonSessionState>` in the current scope
  L12144   [E0609] no field `session_dir` on type `&active_session_state::AgentSessionRuntimeMetadata`
  L12314   [E0308] mismatched types
  L12316   [E0599] no method named `unwrap_or_default` found for struct `Vec<session_manager::SessionInfo>` in the current scope
  L12431   [E0599] no method named `in_flight_catchup` found for struct `std::sync::Arc<DaemonClientHandle>` in the current scope
  L12438   [E0599] no method named `set_in_flight_catchup` found for struct `std::sync::Arc<DaemonClientHandle>` in the current scope
  L12440   [E0599] no method named `set_in_flight_catchup` found for struct `std::sync::Arc<DaemonClientHandle>` in the current scope
  L12551   [E0308] mismatched types
  L12589   [E0308] mismatched types
  L12617   [E0308] arguments to this function are incorrect
  L12638   [E0277] the trait bound `pi_agent_core::types::AgentMessage: std::default::Default` is not satisfied
  L12674   [E0308] arguments to this function are incorrect
  L12739   [E0596] cannot borrow data in an `Arc` as mutable
  L12751   [E0596] cannot borrow data in an `Arc` as mutable
  L12839   [E0277] the trait bound `pi_agent_core::types::AgentMessage: std::default::Default` is not satisfied
  L13223   [E0308] mismatched types
  L13281   [E0277] the trait bound `active_session_state::AgentSessionRuntimeMetadata: serde::de::DeserializeOwned` is not satisfied
  L13288   [E0277] the trait bound `active_session_state::AgentSessionRuntimeMetadata: serde::Serialize` is not satisfied
  L13496   [E0599] no method named `as_deref` found for enum `SessionStateStatus` in the current scope
  L13613   [E0599] no method named `as_deref` found for enum `SessionStateStatus` in the current scope
  L13616   [E0599] no method named `cancel_scheduled_jobs_for_session_file` found for reference `&AgentDaemon` in the current scope
  L13826   [E0507] cannot move out of an `Arc`
  L13981   [None] lifetime may not live long enough
  L14127   [E0599] no method named `rlm_spawn_ledger` found for reference `&AgentDaemon` in the current scope
  L14210   [E0599] no method named `edges` found for opaque type `impl futures::Future<Output = std::sync::Arc<RlmSpawnLedger>>` in the current scope
  L14241   [E0308] mismatched types
  L14243   [E0599] no method named `as_ref` found for struct `RlmSubagentDisplayEntry` in the current scope
  L14251   [E0599] no method named `as_ref` found for struct `RlmSubagentDisplayEntry` in the current scope
  L14256   [E0599] no method named `as_ref` found for struct `RlmSubagentDisplayEntry` in the current scope
  L14260   [E0599] no method named `as_ref` found for struct `RlmSubagentDisplayEntry` in the current scope
  L14263   [E0599] no method named `as_ref` found for struct `RlmSubagentDisplayEntry` in the current scope
  L14266   [E0599] no method named `as_ref` found for struct `RlmSubagentDisplayEntry` in the current scope
  L14269   [E0599] no method named `as_ref` found for struct `RlmSubagentDisplayEntry` in the current scope
  L14272   [E0599] no method named `as_ref` found for struct `RlmSubagentDisplayEntry` in the current scope
  L14276   [E0599] no method named `as_ref` found for struct `RlmSubagentDisplayEntry` in the current scope
  L14302   [E0277] `?` couldn't convert the error to `std::string::String`
  L14313   [E0599] no method named `to_passive_entry` found for struct `LegacyRlmSubagentRegistryEntry` in the current scope
  L14341   [E0599] no method named `append_delete` found for opaque type `impl futures::Future<Output = std::sync::Arc<RlmSpawnLedger>>` in the current scope
  L14347   [E0609] no field `removed_agent_ids` on type `std::sync::Mutex<WorkerRosterReporterState>`
  L14350   [E0308] mismatched types
  L14445   [E0599] no method named `is_destroyed` found for reference `&std::sync::Arc<DaemonClientHandle>` in the current scope
  L14448   [E0061] this method takes 3 arguments but 5 arguments were supplied
    --- rendered ---
      error[E0308]: mismatched types
         --> crates\pi-coding-agent\src\modes\daemon\daemon_mode.rs:728:48
          |
      728 |   const RECOVERY_CHECKPOINT_EVENTS: [&str; 17] = [
          |  ___________________________________----------___^
          | |                                   |
          | |                                   expected because of the type of the constant
      729 | |     "agent_start",
      730 | |     "agent_end",
      731 | |     "turn_start",
      ...   |
      744 | |     "rlm_child_update",
      745 | | ];
          | |_^ expected an array with a size of 17, found one with a size of 16
    --- rendered ---
      error[E0599]: the method `to_string` exists for reference `&DaemonClientCapability`, but its trait bounds were not satisfied
          --> crates\pi-coding-agent\src\modes\daemon\daemon_mode.rs:1599:28
           |
      1599 |         .map(|value| value.to_string())
           |                            ^^^^^^^^^ method cannot be called on `&DaemonClientCapability` due to unsatisfied trait bounds
           |
          ::: crates\pi-coding-agent\src\modes\daemon\daemon_protocol.rs:82:1
           |
        82 | pub enum DaemonClientCapability {
           | ------------------------------- doesn't satisfy `DaemonClientCapability: ToString` or `DaemonClientCapability: std::fmt::Display`
           |
           = note: the following trait bounds were not satisfied:
                   `DaemonClientCapability: std::fmt::Display`
                   which is required by `DaemonClientCapability: ToString`
    --- rendered ---
      error[E0308]: mismatched types
          --> crates\pi-coding-agent\src\modes\daemon\daemon_mode.rs:1603:58
           |
      1603 |         if DAEMON_SUPPORTED_CLIENT_CAPABILITIES.contains(&capability.as_str()) {
           |                                                 -------- ^^^^^^^^^^^^^^^^^^^^ expected `&DaemonClientCapability`, found `&&str`
           |                                                 |
           |                                                 arguments to this method are incorrect
           |
           = note: expected reference `&DaemonClientCapability`
                      found reference `&&str`
      note: method defined here
          --> /rustc/48a229ceaefd4985c50990b14116b6d856af0985/library\core\src\slice\mod.rs:2594:11
    --- rendered ---
      error[E0308]: mismatched types
          --> crates\pi-coding-agent\src\modes\daemon\daemon_mode.rs:1660:53
           |
      1660 |         .retain(|candidate| !Arc::ptr_eq(candidate, &client.state));
           |                              -----------            ^^^^^^^^^^^^^ expected `&Arc<DaemonSocketClient>`, found `&Arc<Mutex<DaemonSocketClient>>`
           |                              |
           |                              arguments to this function are incorrect
           |
           = note: expected reference `&std::sync::Arc<DaemonSocketClient>`
                      found reference `&std::sync::Arc<std::sync::Mutex<DaemonSocketClient>>`
      note: associated function defined here
          --> /rustc/48a229ceaefd4985c50990b14116b6d856af0985/library\alloc\src\sync.rs:2166:11
    --- rendered ---
      error[E0609]: no field `path` on type `ResolvedSession`
          --> crates\pi-coding-agent\src\modes\daemon\daemon_mode.rs:1823:10
           |
      1823 |         .path)
           |          ^^^^ unknown field
    --- rendered ---
      error[E0308]: mismatched types
          --> crates\pi-coding-agent\src\modes\daemon\daemon_mode.rs:1857:9
           |
      1856 |       let socket_path = normalize_socket_path(
           |                         --------------------- arguments to this function are incorrect
      1857 | /         options
      1858 | |             .socket_path
      1859 | |             .clone()
      1860 | |             .unwrap_or_else(default_daemon_socket_path),
           | |_______________________________________________________^ expected `&str`, found `String`
           |
      note: function defined here
          --> crates\pi-coding-agent\src\modes\daemon\daemon_socket.rs:493:8
           |