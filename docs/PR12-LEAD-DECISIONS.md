# PR #12 completion - lead decisions (read before editing)

Source: PR #12 handoff `docs/RUST_PORT_HANDOFF.md` (branch `fix/rust-port-completion`).
Baseline TypeScript: `packages/coding-agent/src` at commit 9f547cea. TS is the behavioural reference.

## Verified state (measured by the lead, not copied from the handoff)

- `cargo check --workspace --locked` -> exit 101, **1108 errors** (handoff said 1106; same snapshot).
- Errors by crate: pi-coding-agent 1104, pi-agent-core 1, 3 in macro spans.
- By code: E0308 459, E0599 231, E0609 90, E0277 85, E0560 60, E0061 49.
- Concentrated: `core/agent_session.rs` 355, `modes/daemon/daemon_mode.rs` 220,
  `core/agent_session/runtime_members.rs` 108. Then 422 across 86 other files.

## ROOT CAUSES the lead confirmed (fix these, do not patch symptoms one by one)

1. **CORRECTION (the lead was wrong here - do not act on the earlier claim).**
   `core/agent_session_runtime.rs:33-34` declares `mod daemon_adapter; mod in_process_adapter;` and
   neither file exists. The handoff is CORRECT that they must be implemented. The lead first claimed
   these were "invented" and deleted the two `mod` lines. That was WRONG and has been reverted.

   Evidence: the TypeScript seam IS real, it is just implicit. `InProcessAgentConnection`'s
   constructor takes the concrete class `AgentSessionRuntime` and calls 8 of its members directly
   (`session`, `switchSession`, `newSession`, `fork`, `importFromJsonl`, `setRebindSession`,
   `setBeforeSessionInvalidate`, `dispose`). TypeScript resolves this structurally; Rust cannot,
   because the consumer trait `InProcessRuntimeHost` lives in `modes/agent_connection/` and the
   concrete `AgentSessionRuntime` lives in `core/`. So Rust needs an explicit trait plus a real
   adapter IMPL that forwards to `AgentSessionRuntime`.

   Current state: `InProcessRuntimeHost` (69 methods) has ZERO implementors. `DaemonSession`
   (97 methods) is implemented ONLY by `MissingSession`. The compiler states this directly:
   `no method named runtime_switch_session found for &Arc<AgentDaemon>` /
   "`InProcessRuntimeHost` defines an item `runtime_switch_session`, perhaps you need to implement it".

   -> IMPLEMENT the adapters. Do NOT create empty modules. Do NOT delete the `mod` lines.
   The adapter struct wraps `Arc<AgentSessionRuntime>` and forwards each method to the real member.
   For `daemon_adapter`, implement the runtime-facing part over the daemon's session state.

2. **`Box` vs `Arc` callback drift (64 errors).** e.g.
   `assistant_message.rs:58` declares `fn rc(value: Box<dyn Fn(&str)->String + Send + Sync>)`
   but call sites pass `theme_source.heading` which is `Arc<dyn Fn(&str)->String + Send + Sync>`.
   Same in branch_summary_message.rs, injected_prompt_message.rs, side_question.rs, daemon_mode.rs,
   rpc_mode.rs. Decide ONE canonical ownership per type from the definition site and align callers.
   Do not add `Box::new` wrappers where the field is an `Arc` - the field type is canonical.

3. **`KeyTextOptions` argument drift (18 errors).** `render_key_text`-style helpers take
   `&KeyTextOptions` (struct with `primary_only: bool`) but callers pass a bare `bool` or an owned
   `KeyTextOptions`. Pass `&KeyTextOptions { primary_only: ... }`.

4. **`ActionLifecycleState` vs `&str` (28 errors).** The store exposes the enum with `.as_str()`.
   Compare enum values or call `.as_str()`; never compare the enum to a string literal.

5. **Action-store API does NOT exist: `replace_payload` (7), `replace_lifecycle` (8),
   `insert_records` (1), `clear_prepared` (1).** The real API is
   `update_action(&action) -> Result<(), String>`. Mutate a CLONE of the action, then
   `update_action(&clone)` and keep the owned action. Do NOT add the invented methods.

6. **Missing session helpers (no Rust def, no TS member of that name).** Do NOT invent them and do
   NOT no-op them: find the real TS flow and port it.
   - `wait_for_refine_idle` -> TS `this._waitForRefineIdle()` exists.
   - `maybe_start_serialized_background_plan` -> TS `this._maybeStartSerializedBackgroundPlan()` exists.
   - `action_state_of`, `mark_delivery_record_durable`, `mark_matching_records_durable`,
     `filter_records_after_dispatch_failure`, `strip_next_turn_records`, `await_agent_event_queue`,
     `push_agent_event_task` -> locate the corresponding TS flow in `core/agent-session.ts`
     (queue/dispatch/record-durability/event-queue sections) and port its behaviour.

7. **`on_session` callback ownership.** Declared `Box<dyn Fn...>` in one place and
   `Arc<dyn Fn...>` in another (daemon_catalog_process.rs, rlm_ledger.rs, saved_session_catalog.rs).
   Pick the type from the definition that the TS closure shape maps to (`onSession?: (session: SessionInfo) => void`
   is a plain callback that is stored and cloned) and align callers.

8. **Missing serde derives.** `daemon_mode.rs:828 ModelIdentity` lacks `Serialize` but is put into
   `serde_json::json!`. Add the derive only if the TS type is serialized on the wire there.

## HARD RULES (from docs/PORT-RULES.md and AGENTS.md)

- PORT IT, DO NOT REIMAGINE IT. 1:1 with the TypeScript. Preserve names, constants, JSON field names.
- NEVER invent a function, method, type or field that the TypeScript does not have.
  If you cannot find it: say so and report, do not guess.
- NEVER modify `packages/**` (read-only reference) or any `.py`/`prime-agent-runtime/**`.
- Do NOT use `cfg` gates, empty modules, `todo!()`, `unimplemented!()`, or default stubs to hide an error.
- Do NOT weaken or delete a test to make it pass.
- One writer per file. Write ONLY the files assigned to you.
- Do NOT run git commit/push/checkout/stash/reset. The lead commits.
- Write no more than a short comment where meaning is unclear; do not add narrative comments.

## BUILD COMMANDS (private toolchain; env already set in the lead kernel)

    cd /c/Users/openclawuser/optimus-rust-port
    export CARGO_BUILD_JOBS=4 CARGO_INCREMENTAL=0
    export CARGO_TARGET_DIR=.port-env/target
    cargo check -p pi-coding-agent --message-format=short 2>&1 | grep "error" | head -40

Per-file diagnostics:

    jq -r 'select(.file|test("agent_session.rs")) | .rendered' .port-env/tmp/check-next.jsonl | head -80

## REPORTING (required)

Report: files changed; `cargo check -p pi-coding-agent` error delta measured by you; every symbol
you could NOT resolve and why; anything you invented (should be nothing). If you cannot finish,
leave a `// REPAIR CURSOR:` comment with the exact next step and report honestly.

## MEASUREMENT GATE (both rules are mandatory; each cost a false reading)

1. **The run must be COMPLETE, not just warnings>0.** A jsonl can be read while it is still being
   appended. `w-run7-tests.jsonl` showed "pack E = 0" mid-write; the finished file showed 2 errors.
   Always `await` the bash handle, then require the stderr tail to contain
   `could not compile ... due to N previous errors` or `Finished`.
2. **Measure with `--tests`.** Plain `cargo check` never type-checks `#[cfg(test)]` modules, so
   test-module errors are invisible. 5 of pack E's 11 errors only appeared under `--tests`.
3. **Zero warnings on the pi-coding-agent crate = ABORTED run.** rustc stops at the first parse
   failure and reports nothing, so every file reads 0. Discard such a measurement.

Reference command:

    cargo check -p pi-coding-agent --tests --message-format=json > out.jsonl 2> out.log
    # then: await completion, assert warnings>0 AND "could not compile"/"Finished" in out.log

4. **Count UNIQUE (file, line, message) sites.** rustc emits the same error once per target
   (lib and lib test), so raw message counts roughly double: gate run 1 was 950 messages =
   527 unique.
