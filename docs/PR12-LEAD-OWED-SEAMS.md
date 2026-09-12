# PR #12 lead-owed seam fixes (not claimed by any live pack)

Recorded 2026-09-12. Each item is verified against the canonical TS and the crate owners.

## 1. `ExtensionBindings` / `AgentSession` extension fields are untyped `Value` (OWED - lead)

Verified defect. TS at `agent-session.ts:565-570` is TYPED:
```ts
export interface ExtensionBindings {
  uiContext?: ExtensionUIContext;
  commandContextActions?: ExtensionCommandContextActions;
  shutdownHandler?: ShutdownHandler;
  onError?: ExtensionErrorListener;
}
```
Rust `core/agent_session.rs:699-703` stores all four as untyped JSON:
- `ui_context: Option<Value>`            -> should be `Option<Arc<dyn ExtensionUiContext>>`
- `command_context_actions: Option<Value>` -> should be `Option<ExtensionCommandContextActions>`
- `shutdown_handler: Option<Arc<dyn Fn(Value) -> BoxFuture<()> + Send + Sync>>` -> `Option<ShutdownHandler>`
- `on_error: Option<Arc<dyn Fn(Value) + Send + Sync>>` -> `Option<ExtensionErrorListener>`

Same defect in `AgentSession` itself:
- `agent_session.rs:2233 extension_command_context_actions: Option<Value>`  -> `Option<ExtensionCommandContextActions>`
- `agent_session.rs:2234 extension_error_listener: Option<Arc<dyn Fn(Value)> + Send + Sync>` -> `Option<ExtensionErrorListener>`

All canonical owners EXIST: `ExtensionUiContext` trait (`core/extensions/types.rs:414`),
`ExtensionCommandContextActions` (`types.rs:2103`, derives Clone), `ExtensionErrorListener`
(`core/extensions/runner.rs:126`), `ShutdownHandler` (`runner.rs:156`).

Error sites produced: `runtime_members.rs` L128, L129, L130, L173, L174 (5 of pack B2's 76).

WHY IT IS NOT FIXED YET: `ExtensionBindings` has exactly one live consumer that would break -
`core/agent_session_runtime/in_process_adapter.rs:829` (pack H, LIVE) builds the struct from
`options.get("uiContext").cloned()`, i.e. from JSON. A `dyn ExtensionUiContext` trait object cannot be
built from JSON, so typing the field is an architectural seam change at that boundary, not a 1-line edit.
The daemon is NOT affected: it has its own separate, already-typed `ExtensionBindingInput`
(`modes/daemon/daemon_extension_binding.rs:75-79`).

DO IT AFTER pack H stops writing. Then re-measure (the 5 sites should clear).

## 2. `_startRlmChildRun` is unported (OWED - needs an owner)

TS `core/agent-session.ts:11448-11600` (`private async _startRlmChildRun`) has NO Rust owner
(grep for `start_rlm_child_run` / `spawn_rlm_child` / `start_rlm_child` = 0 hits).
It is a ~150-line port. Until it exists, `runtime_members.rs:1664 run_rlm_child` must be honest:
either port it, or return an explicit `Err` with a REPAIR CURSOR. It must NOT return a fabricated
`Ok(RlmSpawnHandle { .. })` - that is a stand-in, which the port rules forbid.
