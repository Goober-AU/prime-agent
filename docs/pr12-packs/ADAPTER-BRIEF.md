You are the LEAD-ASSIGNED worker for the single highest-priority remaining seam on the
optimus-rust-port project (Rust port of a TypeScript app layer).

Repo: C:/Users/openclawuser/optimus-rust-port   (branch fix/rust-port-completion)
TypeScript reference (READ-ONLY): C:/Users/openclawuser/optimus-main-20260911/packages/coding-agent/src

## READ FIRST (mandatory)
1. docs/PR12-LEAD-DECISIONS.md - especially root cause #1, which the lead got WRONG once and corrected.
2. docs/pr12-packs/adapter-trait.txt - the full 69-method `InProcessRuntimeHost` trait, extracted for you.
3. docs/RUST_PORT_HANDOFF.md section "Resume in this order" -> item 1 (this is that item).
4. The TypeScript sources named below.

## THE JOB
`crates/pi-coding-agent/src/core/agent_session_runtime.rs:33-34` declares:

    mod daemon_adapter;
    mod in_process_adapter;

Neither file exists (E0583 "file not found for module"). Create
`crates/pi-coding-agent/src/core/agent_session_runtime/in_process_adapter.rs` (and the
`daemon_adapter.rs` companion if you can) that IMPLEMENT the two traits below by forwarding to the
real Rust types.

Traits with NO implementor today:
- `InProcessRuntimeHost` (69 methods) in crates/pi-coding-agent/src/modes/agent_connection/in_process_agent_connection.rs
- `DaemonSession` (97 methods) in crates/pi-coding-agent/src/modes/daemon/daemon_mode.rs
  (currently implemented ONLY by `MissingSession`)

## WHY THIS IS REAL AND NOT INVENTED (do not "clean up" these mod lines - the lead already made that mistake once)
In TypeScript the seam is implicit duck typing: `InProcessAgentConnection`'s constructor takes the
CONCRETE class `AgentSessionRuntime` (modes/agent-connection/in-process-agent-connection.ts) and calls
8 of its members directly: session, switchSession, newSession, fork, importFromJsonl,
setRebindSession, setBeforeSessionInvalidate, dispose. TypeScript resolves that structurally.
Rust cannot, because the trait lives in modes/ and the concrete type lives in core/. So Rust needs
an explicit trait PLUS a real adapter impl. The compiler says it directly:
  "no method named `runtime_switch_session` found for `&Arc<AgentDaemon>`"
  "`InProcessRuntimeHost` defines an item `runtime_switch_session`, perhaps you need to implement it"

## WHAT ALREADY EXISTS (use it, do not reinvent it)
- `core/agent_session_runtime.rs` defines `AgentSessionRuntime` with PUBLIC methods:
  cwd, diagnostics, dispose, fork, import_from_jsonl, list_subagent_runtimes, metadata,
  model_fallback_message, new, new_session, on_session_replaced, runtime_config, services,
  session, set_before_session_invalidate, set_rebind_session, set_runtime_env_scope,
  set_session_extension_runner, set_subagent_runtime_host, switch_session, top_level
  -> the runtime_* trait methods map onto these almost 1:1.
- `modes/agent_connection/snapshot.rs` has `AgentSessionRuntimeSnapshotSource { session: AgentSessionSnapshotSource }`
  plus `create_agent_connection_state(runtime, active_session_id)` and
  `create_agent_connection_snapshot(runtime, active_session_id)` - these show how to build the
  snapshot/state shapes the trait wants. Read that file; it is the worked example.
- 34 of the 69 `InProcessRuntimeHost` methods have a same-named `AgentSession` method
  (e.g. session_prompt -> prompt, session_steer -> steer, session_execute_bash -> execute_bash).
  For those, the adapter is a thin forward.
- The other 35 (session_queue, session_context_tree, session_rlm_children, session_stats,
  session_export_to_html, session_tool_definition, session_available_models, session_header, ...)
  have NO same-named Rust method. For each one, FIND the real owner in the crate first
  (grep the crate and the TypeScript member the trait name mirrors). If genuinely no owner exists,
  that is a missing piece to report - do NOT fabricate a value.

## RULES (non-negotiable)
- PORT IT, DO NOT REIMAGINE IT. Never invent a function, method, type, field or module.
- FORBIDDEN: todo!(), unimplemented!(), returning empty/default values to satisfy the compiler,
  cfg-gating an error away, deleting the `mod` lines, deleting or weakening a test.
- A method that returns `Value::Null` or an empty Vec where the TypeScript returns real data is a
  FALSE FIX. If you cannot implement it truthfully, leave the method out and report it.
- One writer per file. Your files: the two NEW adapter files, plus `core/agent_session_runtime.rs`
  ONLY if you must adjust the `mod` lines. Do NOT edit other packs' files
  (agent_session.rs, runtime_members.rs, daemon_mode.rs, main_entry.rs are owned by other workers
  and are being edited RIGHT NOW - touching them will cause a collision).
- No git commit/push/checkout/stash/reset.

## BUILD / MEASURE
    cd /c/Users/openclawuser/optimus-rust-port
    export CARGO_BUILD_JOBS=4 CARGO_INCREMENTAL=0 CARGO_TARGET_DIR=.port-env/target
    cargo check -p pi-coding-agent --message-format=json > .port-env/tmp/w-adapters.jsonl 2>/dev/null
Count errors whose PRIMARY span is in your two new files. Other files will show errors; ignore them.
NOTE: other workers are editing the crate concurrently, so a run may ABORT before type-checking
(no pi-coding-agent warnings emitted = aborted run, not a clean result). Re-run if that happens.

## SCOPE
This is a big job (69 + 97 methods). Work in passes: get the module to EXIST and compile first,
then implement method groups (session_* forwards, then snapshot/state, then runtime_* ), re-measuring
between passes. If you run out of budget, leave a `// REPAIR CURSOR:` at the exact next step.

## FINAL REPLY
- Files created/changed; count of trait methods genuinely implemented vs left unimplemented.
- For every unimplemented method: its name and why no real owner exists.
- Anything you could not do. Be honest; a truthful partial result beats a false complete claim.
