# Worklist: p-a-agent-session

Total errors in this pack: 355  across 1 files

Read docs/PR12-LEAD-DECISIONS.md FIRST (shared root causes + hard rules).
Ownership: these files are YOURS. Do not edit any other file.

### core/agent_session.rs  (355 errors)
  L1349    [E0308] mismatched types
  L1857    [E0599] no method named `map_err` found for type parameter `T` in the current scope
  L1864    [E0599] no method named `map_err` found for type parameter `T` in the current scope
  L2182    [E0609] no field `rlm_depth` on type `serde_json::Map<std::string::String, serde_json::Value>`
  L2210    [E0599] no method named `as_str` found for enum `std::option::Option<T>` in the current scope
  L2444    [E0308] mismatched types
  L2454    [E0593] closure is expected to take 2 arguments, but it takes 1 argument
  L2469    [E0599] no method named `ensure_harness_digest_context` found for struct `std::sync::Arc<agent_session::AgentSession>` in the current scope
  L2477    [E0599] no method named `refresh` found for reference `&std::sync::Arc<std::sync::Mutex<McpManager>>` in the current scope
  L2522    [E0308] mismatched types
  L2524    [E0599] no method named `replace_acp_servers` found for reference `&std::sync::Arc<std::sync::Mutex<McpManager>>` in the current scope
  L2541    [E0599] no method named `can_release_acp_servers` found for struct `std::sync::Arc<std::sync::Mutex<McpManager>>` in the current scope
  L2544    [E0599] no method named `replace_acp_servers` found for struct `std::sync::Arc<std::sync::Mutex<McpManager>>` in the current scope
  L2563    [E0308] arguments to this method are incorrect
  L2583    [E0277] the `?` operator can only be applied to values that implement `Try`
  L2584    [E0277] `MutexGuard<'_, Pin<Box<dyn Future<Output = Result<(), ...>> + Send>>>` is not a future
  L2611    [E0277] `?` couldn't convert the error to `std::string::String`
  L2612    [E0308] mismatched types
  L2663    [E0599] no method named `get_acp_servers` found for reference `&std::sync::Arc<std::sync::Mutex<McpManager>>` in the current scope
  L2673    [E0599] no method named `build_runtime` found for reference `&agent_session::AgentSession` in the current scope
  L2691    [E0592] duplicate definitions with name `get_required_request_auth`
  L2696    [E0609] no field `ok` on type `impl futures::Future<Output = ResolvedRequestAuth>`
  L2697    [E0609] no field `error` on type `impl futures::Future<Output = ResolvedRequestAuth>`
  L2703    [E0609] no field `api_key` on type `impl futures::Future<Output = ResolvedRequestAuth>`
  L2707    [E0609] no field `headers` on type `impl futures::Future<Output = ResolvedRequestAuth>`
  L2735    [E0593] closure is expected to take 2 arguments, but it takes 1 argument
  L2746    [E0277] `MutexGuard<'_, Pin<Box<dyn Future<Output = Result<(), ...>> + Send>>>` is not a future
  L2748    [E0308] mismatched types
  L2749    [E0061] this method takes 1 argument but 3 arguments were supplied
  L2760    [E0593] closure is expected to take 2 arguments, but it takes 1 argument
  L2772    [E0061] this method takes 1 argument but 6 arguments were supplied
  L2780    [E0277] the `?` operator can only be used on `Result`s, not `Option`s, in an async block that returns `Result`
  L2783    [E0308] mismatched types
  L2784    [E0308] mismatched types
  L2799    [E0631] type mismatch in closure arguments
  L2919    [E0599] no method named `push_agent_event_task` found for reference `&std::sync::Arc<agent_session::AgentSession>` in the current scope
  L3145    [E0599] `ActionLifecycleState` doesn't implement `std::fmt::Display`
  L3155    [E0308] mismatched types
  L3159    [E0308] mismatched types
  L3161    [E0609] no field `id` on type `&Vec<SessionAction<agent_session::QueuedActionPayload>>`
  L3173    [E0599] no method named `replace_payload` found for struct `std::sync::MutexGuard<'_, ActionStore<SessionAction<agent_session::QueuedActionPayload>>>` in the current sc
  L3178    [E0308] mismatched types
  L3179    [E0609] no field `id` on type `&Vec<SessionAction<agent_session::QueuedActionPayload>>`
  L3180    [E0609] no field `payload` on type `&Vec<SessionAction<agent_session::QueuedActionPayload>>`
  L3186    [E0308] mismatched types
  L3189    [E0308] mismatched types
  L3194    [E0308] mismatched types
  L3199    [E0308] mismatched types
  L3203    [E0609] no field `payload` on type `&Vec<SessionAction<agent_session::QueuedActionPayload>>`
  L3204    [E0609] no field `payload` on type `&Vec<SessionAction<agent_session::QueuedActionPayload>>`
  L3211    [E0308] mismatched types
  L3222    [E0308] mismatched types
  L3236    [E0609] no field `payload` on type `Vec<SessionAction<agent_session::QueuedActionPayload>>`
  L3243    [E0277] a value of type `std::collections::HashSet<std::string::String>` cannot be built from an iterator over elements of type `usize`
  L3246    [E0599] no method named `replace_payload` found for struct `std::sync::MutexGuard<'_, ActionStore<SessionAction<agent_session::QueuedActionPayload>>>` in the current sc
  L3246    [E0609] no field `id` on type `&Vec<SessionAction<agent_session::QueuedActionPayload>>`
  L3246    [E0609] no field `payload` on type `Vec<SessionAction<agent_session::QueuedActionPayload>>`
  L3250    [E0308] mismatched types
  L3259    [E0599] no method named `is_empty` found for enum `Result<T, E>` in the current scope
  L3262    [E0308] mismatched types
  L3551    [E0599] no method named `append_durable_status_message` found for reference `&agent_session::AgentSession` in the current scope
  L3637    [E0599] no variant, associated function, or constant named `from_str` found for enum `GoalContextKind` in the current scope
  L3648    [E0308] mismatched types
  L3687    [E0308] mismatched types
  L3690    [E0308] mismatched types
  L3693    [E0308] mismatched types
  L3698    [E0560] struct `GoalUsage` has no field named `cache_read`
  L3699    [E0560] struct `GoalUsage` has no field named `cache_write`
  L3700    [E0560] struct `GoalUsage` has no field named `total_tokens`
  L3768    [E0599] no method named `threshold_context_tokens` found for reference `&agent_session::AgentSession` in the current scope
  L3774    [E0599] no method named `compaction_settings` found for reference `&agent_session::AgentSession` in the current scope
  L3781    [E0599] no method named `maybe_auto_refine` found for reference `&agent_session::AgentSession` in the current scope
  L3783    [E0599] no method named `schedule_auto_refine_after_compaction` found for reference `&agent_session::AgentSession` in the current scope
  L3784    [E0599] no method named `compact` found for reference `&agent_session::AgentSession` in the current scope
  L3817    [E0308] mismatched types
  L3823    [E0308] mismatched types
  L3827    [E0308] mismatched types
  L3827    [E0308] mismatched types
  L3853    [E0308] mismatched types
  L3856    [E0308] mismatched types
  L3962    [E0599] no method named `compaction_settings` found for reference `&agent_session::AgentSession` in the current scope
  L4048    [E0308] `match` arms have incompatible types
  L4064    [E0599] no method named `maybe_start_serialized_background_plan` found for reference `&std::sync::Arc<agent_session::AgentSession>` in the current scope
  L4066    [E0599] no method named `maybe_start_serialized_background_plan` found for reference `&std::sync::Arc<agent_session::AgentSession>` in the current scope
  L4135    [E0308] mismatched types
  L4161    [E0308] mismatched types
  L4189    [E0308] mismatched types
  L4265    [E0061] this method takes 0 arguments but 1 argument was supplied
  L4276    [E0599] no method named `snapshot` found for struct `std::sync::Arc<dyn AgentObserveController>` in the current scope
  L4299    [E0308] mismatched types
  L4401    [E0308] mismatched types
  L4404    [E0308] mismatched types
  L4476    [E0609] no field `tasks` on type `std::sync::MutexGuard<'_, agent_session::RlmContinuationState>`
  L4635    [E0599] no method named `queue_pending_rlm_continuation` found for reference `&agent_session::AgentSession` in the current scope
  L4656    [E0599] no method named `queue_pending_rlm_continuation` found for reference `&agent_session::AgentSession` in the current scope
  L4732    [E0609] no field `relationship` on type `&&AgentSessionMessageAgentSummary`
  L4814    [E0277] `MutexGuard<'_, Pin<Box<dyn Future<Output = Result<(), ...>> + Send>>>` is not a future
  L4867    [E0277] `MutexGuard<'_, Pin<Box<dyn Future<Output = Result<(), ...>> + Send>>>` is not a future
  L5003    [E0308] mismatched types
  L5023    [E0308] mismatched types
  L5062    [E0599] no method named `replace_payload` found for struct `std::sync::MutexGuard<'_, ActionStore<SessionAction<agent_session::QueuedActionPayload>>>` in the current sc
  L5071    [E0277] the trait bound `std::collections::HashSet<usize>: Extend<std::string::String>` is not satisfied
  L5078    [E0599] no method named `replace_payload` found for struct `std::sync::MutexGuard<'_, ActionStore<SessionAction<agent_session::QueuedActionPayload>>>` in the current sc
  L5084    [E0308] mismatched types
  L5114    [E0599] no method named `replace_payload` found for struct `std::sync::MutexGuard<'_, ActionStore<SessionAction<agent_session::QueuedActionPayload>>>` in the current sc
  L5117    [E0308] mismatched types
  L5147    [E0599] no method named `replace_payload` found for struct `std::sync::MutexGuard<'_, ActionStore<SessionAction<agent_session::QueuedActionPayload>>>` in the current sc
  L5148    [E0308] mismatched types
  L5166    [E0599] no method named `push_agent_event_task` found for reference `&std::sync::Arc<agent_session::AgentSession>` in the current scope
  L5288    [E0308] mismatched types
  L5301    [E0277] the trait bound `std::collections::HashSet<usize>: Extend<std::string::String>` is not satisfied
  L5306    [E0308] mismatched types
  L5422    [E0599] no method named `maybe_start_serialized_background_plan` found for reference `&std::sync::Arc<agent_session::AgentSession>` in the current scope
  L5450    [E0308] mismatched types
  L5451    [E0308] mismatched types
  L5473    [E0308] mismatched types
  L5522    [E0277] the `?` operator can only be applied to values that implement `Try`
  L5523    [E0061] this method takes 1 argument but 3 arguments were supplied
  L5524    [E0277] the `?` operator can only be used in an async function that returns `Result` or `Option` (or another type that implements `FromResidual`)
  L5531    [E0308] mismatched types
  L5531    [E0277] the `?` operator can only be used in an async function that returns `Result` or `Option` (or another type that implements `FromResidual`)
  L5569    [E0599] no method named `schedule_session_input_pump` found for reference `&agent_session::AgentSession` in the current scope
  L5622    [E0308] mismatched types
  L5628    [E0308] mismatched types
  L5633    [E0308] mismatched types
  L5645    [E0308] mismatched types
  L5656    [E0308] mismatched types
  L5664    [E0308] mismatched types
  L5674    [E0308] mismatched types
  L5686    [E0308] mismatched types
  L5701    [E0308] mismatched types
  L5717    [E0308] mismatched types
  L5770    [E0593] closure is expected to take 2 arguments, but it takes 1 argument
  L5836    [E0599] no method named `run_serialized_refine` found for reference `&std::sync::Arc<agent_session::AgentSession>` in the current scope
  L5864    [E0308] mismatched types
  L5884    [E0308] mismatched types
  L5894    [E0599] no method named `run_serialized_refine_checkpoint` found for reference `&std::sync::Arc<agent_session::AgentSession>` in the current scope
  L5896    [E0308] mismatched types
  L5921    [E0308] mismatched types
  L6279    [E0308] mismatched types
  L6355    [E0599] no method named `model_visible_skills` found for reference `&agent_session::AgentSession` in the current scope
  L6429    [E0599] no method named `execute_extension_command` found for reference `&agent_session::AgentSession` in the current scope
  L6444    [E0308] arguments to this method are incorrect
  L6457    [E0308] mismatched types
  L6461    [E0599] no method named `compaction_settings` found for reference `&std::sync::Arc<agent_session::AgentSession>` in the current scope
  L6463    [E0061] this method takes 1 argument but 2 arguments were supplied
  L6479    [E0599] no method named `wait_for_refine_idle` found for reference `&std::sync::Arc<agent_session::AgentSession>` in the current scope
  L6496    [E0308] mismatched types
  L6514    [E0599] no method named `wait_for_refine_idle` found for reference `&std::sync::Arc<agent_session::AgentSession>` in the current scope
  L6582    [E0609] no field `return_after_accepted` on type `PromptOptions`
  L6724    [E0277] the `?` operator can only be applied to values that implement `Try`
  L6738    [E0308] mismatched types
  L6954    [E0061] this method takes 0 arguments but 1 argument was supplied
  L7063    [E0609] no field `return_after_accepted` on type `PromptOptions`
  L7111    [E0061] this method takes 0 arguments but 1 argument was supplied
  L7403    [E0609] no field `handler` on type `ResolvedCommand`
  L7438    [E0615] attempted to take value of method `name` on type `&&Skill`
  L7453    [E0615] attempted to take value of method `name` on type `Skill`
  L7463    [E0599] no method named `emit_error_value` found for struct `std::sync::Arc<runner::ExtensionRunner>` in the current scope
  L7505    [E0277] the `?` operator can only be applied to values that implement `Try`
  L7548    [E0308] mismatched types
  L7627    [E0560] struct `PreparedTurnActionOptions` has no field named `records`
  L7974    [E0560] struct `RuntimeActivity` has no field named `is_streaming`
  L7975    [E0560] struct `RuntimeActivity` has no field named `is_compacting`
  L7976    [E0560] struct `RuntimeActivity` has no field named `is_retrying`
  L7977    [E0560] struct `RuntimeActivity` has no field named `is_bash_running`
  L7978    [E0560] struct `RuntimeActivity` has no field named `queued_work_paused`
  L7979    [E0560] struct `RuntimeActivity` has no field named `session_input_pump_suspended`
  L8005    [E0061] this method takes 1 argument but 0 arguments were supplied
  L8026    [E0308] mismatched types
  L8039    [E0599] no method named `await_agent_event_queue` found for reference `&std::sync::Arc<agent_session::AgentSession>` in the current scope
  L8046    [E0599] no method named `wait_for_refine_idle` found for reference `&std::sync::Arc<agent_session::AgentSession>` in the current scope
  L8111    [E0609] no field `execution_policy` on type `SessionTurnPayload`
  L8111    [E0609] no field `execution_policy` on type `SessionTurnPayload`
  L8130    [E0599] no method named `replace_lifecycle` found for struct `std::sync::MutexGuard<'_, ActionStore<SessionAction<agent_session::QueuedActionPayload>>>` in the current 
  L8139    [E0599] no method named `action_state_of` found for reference `&std::sync::Arc<agent_session::AgentSession>` in the current scope
  L8145    [E0599] no method named `mark_delivery_record_durable` found for reference `&std::sync::Arc<agent_session::AgentSession>` in the current scope
  L8146    [E0599] no method named `replace_lifecycle` found for struct `std::sync::MutexGuard<'_, ActionStore<SessionAction<agent_session::QueuedActionPayload>>>` in the current 
  L8154    [E0599] no method named `action_state_of` found for reference `&std::sync::Arc<agent_session::AgentSession>` in the current scope
  L8155    [E0599] no method named `replace_lifecycle` found for struct `std::sync::MutexGuard<'_, ActionStore<SessionAction<agent_session::QueuedActionPayload>>>` in the current 
  L8173    [E0308] mismatched types
  L8177    [E0599] no method named `mark_matching_records_durable` found for reference `&std::sync::Arc<agent_session::AgentSession>` in the current scope
  L8178    [E0599] no method named `filter_records_after_dispatch_failure` found for reference `&std::sync::Arc<agent_session::AgentSession>` in the current scope
  L8188    [E0599] no method named `action_state_of` found for reference `&std::sync::Arc<agent_session::AgentSession>` in the current scope
  L8221    [E0308] mismatched types
  L8224    [E0599] no method named `action_state_of` found for reference `&std::sync::Arc<agent_session::AgentSession>` in the current scope
  L8226    [E0599] no method named `replace_lifecycle` found for struct `std::sync::MutexGuard<'_, ActionStore<SessionAction<agent_session::QueuedActionPayload>>>` in the current 
  L8252    [E0609] no field `queue_visible` on type `SessionTurnPayload`
  L8261    [E0599] no method named `action_state_of` found for reference `&std::sync::Arc<agent_session::AgentSession>` in the current scope
  L8306    [E0599] no method named `action_state_of` found for struct `std::sync::Arc<agent_session::AgentSession>` in the current scope
  L8311    [E0599] no method named `wait_for_refine_idle` found for struct `std::sync::Arc<agent_session::AgentSession>` in the current scope
  L8328    [E0599] no method named `replace_lifecycle` found for struct `std::sync::MutexGuard<'_, ActionStore<SessionAction<agent_session::QueuedActionPayload>>>` in the current 
  L8342    [E0599] no method named `replace_lifecycle` found for struct `std::sync::MutexGuard<'_, ActionStore<SessionAction<agent_session::QueuedActionPayload>>>` in the current 
  L8353    [E0599] no method named `replace_lifecycle` found for struct `std::sync::MutexGuard<'_, ActionStore<SessionAction<agent_session::QueuedActionPayload>>>` in the current 
  L8385    [E0308] mismatched types
  L8437    [E0599] no method named `emit_error_value` found for struct `std::sync::Arc<runner::ExtensionRunner>` in the current scope
  L8459    [E0599] no method named `action_state_of` found for reference `&std::sync::Arc<agent_session::AgentSession>` in the current scope
  L8471    [E0609] no field `execution_policy` on type `SessionTurnPayload`
  L8488    [E0277] `{closure@crates\pi-coding-agent\src\core\agent_session.rs:8495:21: 8495:68}` is not a future
  L8489    [E0061] this method takes 3 arguments but 2 arguments were supplied
  L8491    [E0277] `{closure@crates\pi-coding-agent\src\core\agent_session.rs:8495:21: 8495:68}` is not a future
  L8506    [E0277] `{closure@crates\pi-coding-agent\src\core\agent_session.rs:8495:21: 8495:68}` is not a future
  L8512    [E0599] no method named `strip_next_turn_records` found for reference `&std::sync::Arc<agent_session::AgentSession>` in the current scope
  L8517    [E0600] cannot apply unary operator `!` to type `std::option::Option<PreparedPromptPreparation>`
  L8555    [E0599] no method named `strip_next_turn_records` found for reference `&std::sync::Arc<agent_session::AgentSession>` in the current scope
  L8563    [E0599] no method named `await_agent_event_queue` found for reference `&std::sync::Arc<agent_session::AgentSession>` in the current scope
  L8567    [E0599] no method named `action_state_of` found for reference `&std::sync::Arc<agent_session::AgentSession>` in the current scope
  L8606    [E0599] no method named `action_state_of` found for reference `&std::sync::Arc<agent_session::AgentSession>` in the current scope
  L8619    [E0308] mismatched types
  L8648    [E0599] no method named `insert_records` found for struct `std::sync::MutexGuard<'_, ActionStore<SessionAction<agent_session::QueuedActionPayload>>>` in the current sco
  L8670    [E0061] this method takes 1 argument but 2 arguments were supplied
  L8671    [E0061] this method takes 2 arguments but 1 argument was supplied
  L8682    [E0599] no method named `replace_lifecycle` found for struct `std::sync::MutexGuard<'_, ActionStore<SessionAction<agent_session::QueuedActionPayload>>>` in the current 
  L8722    [E0599] no method named `refine_with_options` found for reference `&std::sync::Arc<agent_session::AgentSession>` in the current scope
  L8722    [E0277] the size for values of type `str` cannot be known at compilation time
  L8723    [E0277] the size for values of type `str` cannot be known at compilation time
  L8724    [E0277] the size for values of type `str` cannot be known at compilation time
  L8724    [E0277] the size for values of type `str` cannot be known at compilation time
  L8727    [E0599] no method named `emit_refine_failed` found for reference `&std::sync::Arc<agent_session::AgentSession>` in the current scope
  L8728    [E0308] mismatched types
  L8745    [E0061] this method takes 1 argument but 2 arguments were supplied
  L8748    [E0599] no method named `is_empty` found for enum `std::option::Option<T>` in the current scope
  L8749    [E0277] `std::option::Option<std::string::String>` doesn't implement `std::fmt::Display`
  L8793    [E0308] mismatched types
  L8794    [E0063] missing field `command_entry_id` in initializer of `SessionSlashCommandResultDetails`
  L8804    [E0308] mismatched types
  L8806    [E0308] mismatched types
  L8822    [E0308] mismatched types
  L8823    [E0063] missing field `command_entry_id` in initializer of `SessionSlashCommandResultDetails`
  L8842    [E0061] this function takes 4 arguments but 2 arguments were supplied
  L8851    [E0308] mismatched types
  L8856    [E0308] mismatched types
  L8859    [E0308] mismatched types
  L8861    [E0308] mismatched types
  L8893    [E0277] the trait bound `messages::CustomMessage: std::default::Default` is not satisfied
  L8901    [E0599] no method named `content_as_parts` found for struct `messages::CustomMessage` in the current scope
  L8923    [E0599] no method named `content_as_parts` found for struct `messages::CustomMessage` in the current scope
  L8962    [E0308] mismatched types
  L8998    [E0599] no method named `prompt_with_options` found for reference `&std::sync::Arc<agent_session::AgentSession>` in the current scope
  L9004    [E0308] mismatched types
  L9022    [E0609] no field `queue_visible` on type `SessionTurnPayload`
  L9027    [E0308] mismatched types
  L9045    [E0308] mismatched types
  L9073    [E0599] no method named `clear_prepared` found for struct `std::sync::MutexGuard<'_, ActionStore<SessionAction<agent_session::QueuedActionPayload>>>` in the current sco
  L9094    [E0308] mismatched types
  L9095    [E0308] mismatched types
  L9111    [E0308] mismatched types
  L9112    [E0308] mismatched types
  L9113    [E0308] mismatched types
  L9114    [E0308] mismatched types
  L9131    [E0308] mismatched types
  L9137    [E0308] mismatched types
  L9171    [E0308] mismatched types
  L9193    [E0308] mismatched types
  L9289    [E0609] no field `content` on type `SessionTurnPayload`
  L9290    [E0609] no field `content` on type `SessionTurnPayload`
  L9297    [E0609] no field `content` on type `SessionTurnPayload`
  L9304    [E0609] no field `content` on type `SessionTurnPayload`
  L9338    [E0599] no method named `replace_payload` found for struct `std::sync::MutexGuard<'_, ActionStore<SessionAction<agent_session::QueuedActionPayload>>>` in the current sc
  L9400    [E0308] mismatched types
  L9400    [E0308] mismatched types
  L9400    [E0308] mismatched types
  L9400    [E0308] mismatched types
  L9409    [E0308] mismatched types
  L9410    [E0308] mismatched types
  L9479    [E0560] struct `SessionActionRecoverySnapshot` has no field named `version`
  L9516    [E0308] mismatched types
  L9609    [E0599] the method `clone` exists for struct `MutexGuard<'_, Pin<Box<dyn Future<Output = Result<(), ...>> + Send>>>`, but its trait bounds were not satisfied
  L9648    [E0599] no method named `schedule_session_input_pump` found for reference `&agent_session::AgentSession` in the current scope
  L9654    [E0599] no method named `schedule_session_input_pump` found for reference `&agent_session::AgentSession` in the current scope
  L9778    [E0599] no method named `emit_model_select_value` found for struct `std::sync::Arc<runner::ExtensionRunner>` in the current scope
  L9791    [E0599] the method `clone` exists for struct `MutexGuard<'_, Pin<Box<dyn Future<Output = Result<(), ...>> + Send>>>`, but its trait bounds were not satisfied
  L9824    [E0308] mismatched types
  L9841    [E0599] the method `clone` exists for struct `MutexGuard<'_, Pin<Box<dyn Future<Output = Result<(), ...>> + Send>>>`, but its trait bounds were not satisfied
  L9849    [E0609] no field `defer_emit` on type `&ModelSelectOptions`
  L9854    [E0599] the method `clone` exists for struct `MutexGuard<'_, Pin<Box<dyn Future<Output = Result<(), ...>> + Send>>>`, but its trait bounds were not satisfied
  L9884    [E0308] mismatched types
  L9892    [E0063] missing fields `is_scoped` and `service_tier` in initializer of `ModelCycleResult`
  L9894    [E0308] mismatched types
  L9911    [E0308] mismatched types
  L9916    [E0063] missing fields `is_scoped` and `service_tier` in initializer of `ModelCycleResult`
  L9918    [E0308] mismatched types
  L9924    [E0308] mismatched types
  L9934    [E0308] mismatched types
  L9949    [E0308] mismatched types
  L9951    [E0308] mismatched types
  L9968    [E0308] mismatched types
  L9971    [E0599] no variant, associated function, or constant named `Standard` found for enum `std::option::Option<T>` in the current scope
  L9993    [E0308] mismatched types
  L9993    [E0308] mismatched types
  L10011   [E0308] mismatched types
  L10033   [E0308] mismatched types
  L10041   [E0308] mismatched types
  L10074   [E0308] mismatched types
  L10310   [E0308] mismatched types
  L10311   [E0560] struct `AutoRefineReviewRequest` has no field named `branch_version`
  L10312   [E0560] struct `AutoRefineReviewRequest` has no field named `instructions`
  L10335   [E0308] mismatched types
  L10341   [E0609] no field `approved` on type `&AutoRefineReview`
  L10349   [E0560] struct `PlanRefinementRequest<'_>` has no field named `instructions`
  L10349   [E0308] mismatched types
  L10350   [E0560] struct `PlanRefinementRequest<'_>` has no field named `source`
  L10350   [E0599] no associated function or constant named `Auto` found for struct `std::string::String` in the current scope
  L10352   [E0277] `?` couldn't convert the error to `std::string::String`
  L10353   [E0308] mismatched types
  L10353   [E0277] the `?` operator can only be applied to values that implement `Try`
  L10354   [E0061] this function takes 3 arguments but 1 argument was supplied
  L10354   [E0277] the `?` operator can only be applied to values that implement `Try`
  L10373   [E0560] struct `AutoRefineReview` has no field named `approved`
  L10415   [E0061] this function takes 2 arguments but 1 argument was supplied
  L10416   [E0599] no associated function or constant named `default` found for struct `HarnessState` in the current scope
  L10418   [E0061] this function takes 2 arguments but 1 argument was supplied
  L10427   [E0308] mismatched types
  L10428   [E0061] this function takes 1 argument but 0 arguments were supplied
  L10429   [E0061] this function takes 2 arguments but 1 argument was supplied
  L10445   [E0308] mismatched types
  L10478   [E0308] mismatched types
  L10487   [E0308] mismatched types
  L10494   [E0308] mismatched types
  L10498   [E0599] no method named `unwrap_or` found for type `f64` in the current scope
  L10514   [E0369] binary operation `<` cannot be applied to type `ContextUsageEstimate`
  L10536   [E0308] mismatched types
  L10547   [E0308] mismatched types
  L10659   [E0599] no associated function or constant named `default` found for struct `messages::CustomMessage` in the current scope
  L10683   [E0615] attempted to take value of method `file_path` on type `&Skill`
  L10688   [E0615] attempted to take value of method `file_path` on type `&Skill`
  L10759   [E0277] the trait bound `messages::CustomMessage: std::default::Default` is not satisfied
  L10761   [E0599] no associated function or constant named `default` found for struct `messages::CustomMessage` in the current scope
  L10783   [E0599] no variant, associated function, or constant named `Completed` found for enum `GoalStatus` in the current scope
  L10784   [E0599] no variant, associated function, or constant named `Failed` found for enum `GoalStatus` in the current scope
  L10785   [E0599] no variant, associated function, or constant named `Cleared` found for enum `GoalStatus` in the current scope
  L10808   [E0308] mismatched types
  L10811   [E0308] mismatched types
  L10816   [E0599] no method named `unwrap_or` found for type `f64` in the current scope
  L10822   [E0609] no field `service_tiers` on type `&pi_ai::index::Model`
  L10840   [E0308] mismatched types
  L10850   [E0063] missing field `execution_policy` in initializer of `SessionActionRecoveryPayload`
  L10873   [E0609] no field `content` on type `SessionTurnPayload`
  L10878   [E0609] no field `queue_visible` on type `SessionTurnPayload`
  L10890   [E0308] mismatched types
  L10891   [E0308] mismatched types
  L10892   [E0308] mismatched types
  L11012   [E0277] `(dyn Fn() -> Result<(), std::string::String> + std::marker::Send + Sync + 'static)` doesn't implement `Debug`
  L11139   [E0609] no field `id` on type `std::option::Option<pi_ai::index::Model>`
  L11142   [E0308] mismatched types
  L11144   [E0308] mismatched types
  L11208   [E0308] mismatched types
  L11228   [E0061] this function takes 3 arguments but 2 arguments were supplied
  L11243   [E0308] mismatched types
  L11256   [E0061] this method takes 8 arguments but 7 arguments were supplied
  L11323   [E0609] no field `delivery_mode` on type `&agent_session::AgentCronJob`
  L11330   [E0609] no field `label` on type `&agent_session::AgentCronJob`
  L11334   [E0609] no field `created_at` on type `&agent_session::AgentCronJob`
  L11335   [E0609] no field `updated_at` on type `&agent_session::AgentCronJob`
  L11403   [E0063] missing fields `custom_instructions` and `harness_digest` in initializer of `CompactionSessionEntry`
    --- rendered ---
      error[E0308]: mismatched types
          --> crates\pi-coding-agent\src\core\agent_session.rs:1349:9
           |
      1348 |     match value {
           |           ----- this expression has type `f64`
      1349 |         Some(value) if value.is_finite() => Some(value),
           |         ^^^^^^^^^^^ expected `f64`, found `Option<_>`
           |
           = note: expected type `f64`
                      found enum `std::option::Option<_>`
    --- rendered ---
      error[E0599]: no method named `map_err` found for type parameter `T` in the current scope
          --> crates\pi-coding-agent\src\core\agent_session.rs:1857:38
           |
      1851 | pub async fn wait_for_promise_or_abort<T>(
           |                                        - method `map_err` not found for this type parameter
      ...
      1857 |         None => return promise.await.map_err(|error| error.to_string()),
           |                                      ^^^^^^^ method not found in `T`
           |
           = help: items from traits can only be used if the type parameter is bounded by the trait
      help: the following traits define an item `map_err`, perhaps you need to restrict type parameter `T` with one of them:
           |
      1851 | pub async fn wait_for_promise_or_abort<T: TryFutureExt>(
           |                                         ++++++++++++++
    --- rendered ---
      error[E0599]: no method named `map_err` found for type parameter `T` in the current scope
          --> crates\pi-coding-agent\src\core\agent_session.rs:1864:34
           |
      1851 | pub async fn wait_for_promise_or_abort<T>(
           |                                        - method `map_err` not found for this type parameter
      ...
      1864 |         value = promise => value.map_err(|error| error.to_string()),
           |                                  ^^^^^^^ method not found in `T`
           |
           = help: items from traits can only be used if the type parameter is bounded by the trait
      help: the following traits define an item `map_err`, perhaps you need to restrict type parameter `T` with one of them:
           |
      1851 | pub async fn wait_for_promise_or_abort<T: TryFutureExt>(
           |                                         ++++++++++++++
    --- rendered ---
      error[E0609]: no field `rlm_depth` on type `serde_json::Map<std::string::String, serde_json::Value>`
          --> crates\pi-coding-agent\src\core\agent_session.rs:2182:39
           |
      2182 |             .and_then(|header| header.rlm_depth);
           |                                       ^^^^^^^^^ unknown field
    --- rendered ---
      error[E0599]: no method named `as_str` found for enum `std::option::Option<T>` in the current scope
          --> crates\pi-coding-agent\src\core\agent_session.rs:2210:39
           |
      2210 |             Some(session_artifact_dir.as_str()),
           |                                       ^^^^^^ method not found in `std::option::Option<std::string::String>`
           |
      note: the method `as_str` exists on the type `std::string::String`
          --> /rustc/48a229ceaefd4985c50990b14116b6d856af0985/library\alloc\src\string.rs:1057:4
      help: consider using `Option::expect` to unwrap the `std::string::String` value, panicking if the value is an `Option::None`
           |
      2210 |             Some(session_artifact_dir.expect("REASON").as_str()),
           |                                      +++++++++++++++++
    --- rendered ---
      error[E0308]: mismatched types
          --> crates\pi-coding-agent\src\core\agent_session.rs:2444:77
           |
      2444 |                     session.pending_next_turn_messages.lock().unwrap().push(message);
           |                                                                        ---- ^^^^^^^ expected `messages::CustomMessage`, found `goals::CustomMessage`
           |                                                                        |
           |                                                                        arguments to this method are incorrect
           |
           = note: `goals::CustomMessage` and `messages::CustomMessage` have similar names, but are actually distinct types
      note: `goals::CustomMessage` is defined in module `crate::core::goals` of the current crate
          --> crates\pi-coding-agent\src\core\goals.rs:112:1
           |
       112 | pub struct CustomMessage {
           | ^^^^^^^^^^^^^^^^^^^^^^^^