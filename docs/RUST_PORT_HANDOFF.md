# Rust port integration handoff — 12 September 2026

**This is a stopped, incomplete development checkpoint. It does not compile and has not produced a working application response. Keep the replacement PR in draft; do not merge, install, or deploy this checkpoint.**

The user stopped implementation and requested that the current repairs and specific remaining work be preserved in a PR. No implementation should be inferred complete merely because a replacement type or adapter has been written.

## Repository and source of truth

- Hard fork: `telemusai/optimus-agent`; use remote `telemusai`, not the original PrimeIntellect upstream.
- Working branch: `fix/rust-port-completion`.
- Original PR: [#11](https://github.com/telemusai/optimus-agent/pull/11), 55 commits, head `8556461133acae3bca1dd56f7dec43b113b4571a`.
- Main/TypeScript baseline: `9f547ceaba079dee45ee415a430d57fb86fa940d`.
- Local worktree: `/home/anthony/optimus-rust-port`. The unrelated `/home/anthony/vr-ai-chat` repository is not this project.
- Read `AGENTS.md`, `docs/PORT-RULES.md`, `docs/BASELINE.md`, and `docs/COMPATIBILITY.md` before resuming. The TypeScript under `packages/` is the behavioral reference. Python remains the existing protocol-v3 runtime.
- TypeScript, Python, production Prime installations, Axiom, credentials, and GitHub Actions settings were not changed by these repairs. Actions remain disabled. No live provider calls were made.

## Verified checkpoint state

| Check | Result | Evidence |
|---|---|---|
| Final `cargo check --workspace --locked` | Failed, exit 101; **1,106 compiler errors** | [Complete error records](../evidence/diagnostics/rust-port-handoff-errors.jsonl), [build stderr](../evidence/diagnostics/rust-port-handoff-check.txt) |
| Current `pi-agent-core` library tests | **36 passed, 1 failed**; all five new tests executed | [Full test output](../evidence/diagnostics/rust-port-agent-core-tests.txt) |
| Current private framing module tests | **12 passed**, compiled directly from the real module with `rustc --test` and built dependency rlibs | [Output](../evidence/diagnostics/rust-port-private-framing-tests.txt) |
| Earlier `pi-tui` library tests | **174 passed**; this crate's source was not changed afterward | Local `.port-env/tmp/foundation-tests.log`; application UI modes are a different, unvalidated layer |
| `npm run check` | Failed before checks: `biome: not found`; checkout has no `node_modules` | [Output](../evidence/diagnostics/rust-port-npm-check.txt) |
| Application response / CLI / RPC / daemon smoke | **Not run; application build is blocked** | No working-executable claim |
| Newly edited application tests | **Not run** because the application crate does not compile | Run after its build is repaired |

The original check stopped at 67 early name-resolution errors. A later broad type-check exposed 1,501 errors; the frozen checkpoint check reports 1,106. These are different compiler snapshots, not a percentage of port completion. Some repairs introduce new contracts whose callers have not yet been converted.

The failed foundation test is `types::tests::agent_events_round_trip_the_flat_typescript_wire_shape`, at `crates/pi-agent-core/src/types.rs:796`: deserialization fails with `data did not match any variant of untagged enum AgentMessage`. Trace `AgentEvent` → `AgentMessage` → `pi_ai::types::Message`/`AssistantMessage` round trips. Preserve the flat TypeScript event envelope and optional-field semantics; do not weaken the assertion to make it pass.

The final diagnostic snapshot's largest groups are:

| File, under `crates/pi-coding-agent/src/` | Primary errors |
|---|---:|
| `core/agent_session.rs` | 355 |
| `modes/daemon/daemon_mode.rs` | 224 |
| `core/agent_session/runtime_members.rs` | 108 |
| `main_entry.rs` | 32 |
| `cli/daemon_command.rs` | 21 |
| `modes/telegram/commands.rs` | 21 |
| `modes/interactive/interactive_mode.rs` | 20 |
| `modes/interactive/components/tree_selector.rs` | 18 |
| `package_manager_cli.rs` | 17 |

All file counts, the exact 40 Rust files changed in this session, and their frozen SHA-256 values are in [the status manifest](../evidence/status/rust-port-completion.json). Slice manifests reserve some files that were never edited; use the status manifest's `code_files_changed_this_session` for the actual inventory. Diagnostic line numbers refer to this checkpoint and will move during repairs.

## Repairs preserved in this PR

Except for the specifically passing checks above, these are implementation changes awaiting application-level validation.

### Foundation and framing

- `pi-agent-core/{agent.rs,agent_loop.rs,types.rs}` now await conversion, context transformation, credential lookup, tool hooks, queue/continuation callbacks, and post-turn stop decisions. Pending post-turn decisions can be cancelled. The previous synchronous contract could not support the session's async hook safely.
- Added a real `AgentHandle` implementation for `Arc<pi_agent_core::agent::Agent>` in `core/agent_session/agent_handle.rs`. Parent hook aliases were changed to match it; old session call sites still need conversion.
- Added async stop true/false/cancellation, async pipeline ordering, and flat-event serde tests. The four async tests pass; the serde test fails as recorded above.
- `modes/session_worker/private_framing.rs`: repaired unused generic tracking, shared unsubscribe ownership, duplicate listener registration, and typed-header decode failure propagation. Corrected the existing encoded-header length expectation from 14 to 18 bytes. All 12 module tests pass.
- `daemon_worker_client.rs`: repaired command imports, Unix-only match arms, response decoding, invalid-response settlement, and a lost-wakeup race by preserving a notification permit. Its new response-notification regression remains unrun.

### Shared session/runtime contracts

- Main `AgentSession` now reuses canonical resource-loader, RLM runtime/result, bash-result, kernel-handler, and restore-result types instead of several incompatible local copies. Added missing prepared-turn option types and a generic payload adapter.
- Moved approximately 1,530 late session methods into the child module `core/agent_session/runtime_members.rs` so runtime integration could have separate ownership. This was an integration split, not a completed rewrite.
- `session_action_store.rs`: added `SessionAction<P>`, the `SessionPayload` constraint, shared/cloneable ticket futures, snapshot synchronization, and cancellation candidate ordering. Canonical custom messages and slash commands replace local duplicates.
- `resource_loader.rs`, `package_manager.rs`, settings, services, and SDK now share `Arc<std::sync::Mutex<SettingsManager>>`. Added synchronous reload/flush plumbing for synchronous callers and safely `Send + Sync` queued writes. Corrected absent-versus-null persistence and nested provider timeout lookup.
- SDK defaults now construct concrete Agent, ResourceLoader, and MCP objects. Runtime result nesting and subagent-host forwarding were partially aligned with canonical types.

### Tools, extensions, RLM, and modes

- Added conversion from generic builtin tool definitions into canonical extension tool definitions, preserving execution callbacks, metadata, and live context. Repaired runner context/listener ownership and selected builtin extension type/lifetime errors.
- Runtime members now call real builtin factories, construct an actual inline child Agent/AgentSession, and call `execute_bash_with_operations`. Child prompting/settlement and several runtime helpers remain absent.
- `rlm_runtime.rs`: removed invalid `Model<Api>` usage, fixed explicit-null search-limit rejection, and corrected the expected last-eight-character child-name suffix.
- Daemon routing now uses raw JSON command maps where private worker commands require them. Public protocol types remain canonical; private mode adapters preserve the command body. Fixed duplicate invalid session lookup and several response/ownership mismatches. Intended wire classification: compatibility-preserving port alignment, not a new protocol feature; compatibility tests are still required.
- Restored missing session-search exports; repaired shared connection/listener ownership, saved-session operations, queue serialization, and several TUI/theme/type mismatches. Preserved the qualified `Drop` fix in Armin, generic tool-definition import, and RPC UI request spelling correction.

## Resume in this order

### 1. Resolve the explicit unfinished integration seams

1. `core/agent_session_runtime.rs:33–34` declares `daemon_adapter` and `in_process_adapter`, but **neither file/directory was created**. Implement real adapters in `core/agent_session_runtime/`; do not add empty modules or feature-gate the errors away.
2. `core/agent_session/runtime_members.rs` references **unimplemented helpers** `runtime_extension_actions`, `runtime_extension_context_actions`, `runtime_source_info`, and `RuntimeModelRegistry`. Implement them against `core/extensions/{runner,types}.rs`. Reuse `agent_session_services::provider_config_input` for provider configuration and `sdk::with_model_registry` for asynchronous registry access.
3. `ExtensionBindings` and the stored binding fields in main `agent_session.rs` still have old types. Match canonical `Option<Arc<dyn ExtensionUiContext>>`, `Option<ExtensionCommandContextActions>`, and `Option<ExtensionErrorListener>` instead of JSON values or old local callbacks. Preserve rebind/reload behavior.
4. **`start_rlm_child_run` was moved out of main while its replacement was being assigned, and no replacement was completed.** Its old, already-incomplete body is preserved in [this reference file](../evidence/reference/start-rlm-child-run-before-extraction.rs.txt), so continuation does not depend on local scratch files. Implement the real TypeScript detached admission, child prompt, publication, event subscription, settlement, and cleanup in `runtime_members.rs`. The reference only emitted running state and must not be treated as a working child runner.
5. `core/extensions/builtin/memory.rs::refinement_retry_policy` currently reads nonexistent `retry.max_retry_delay_ms`. Read `settings.get_provider_retry_settings().max_retry_delay_ms`; `modes/daemon/daemon_session_summarizer.rs` contains an existing example. This error was introduced in the last unfinished batch.

### 2. Finish the shared contract conversion before patching consumers individually

- **Async hooks:** parent session install/subscribe callbacks and SDK `convert_to_llm`/`transform_context` still include synchronous closures. Match the now-async `AgentHandle`/`AgentOptions` contracts. In SDK, await `emit_context` and return its transformed messages; currently its future/output is discarded. Do not add blocking `block_on` on a Tokio executor thread.
- **Registry authentication:** two `get_required_request_auth` definitions remain in main session. Consolidate them around the real async `ResolvedRequestAuth` result. `sdk::with_model_registry` runs exclusive registry work on `spawn_blocking` with a runtime handle; use it rather than retaining a standard mutex guard across `.await`. In `model_registry.rs`, flatten optional cached results (`and_then`), and reconcile `stream_simple`'s options callback with its canonical provider signature.
- **Queue payloads:** `QueuedSessionAction = SessionAction<QueuedActionPayload>` now exists. Change stale `turn.base.execution_policy`, `.queue_visible`, and `.content` accesses to fields on `PreparedTurnPayload`; `base` only contains records/text/preview. Compare `ActionLifecycleState` enum values or `.as_str()`, not enums with string literals. Convert `DeliveryOutcome`, `ActionSource`, `WakePolicy`, and delivery policy consistently.
- **Action store API:** it exposes `update_action(&action) -> Result<(), String>`, not the old invented `replace_payload`, `replace_lifecycle`, `insert_records`, and `clear_prepared` methods. Preserve state transitions and update the owned action after mutating a clone. Handle `Result` from selection/removal/rollback and await `ticket.completed.clone().await` without moving a future out of an `Arc`.
- **Missing session helpers:** implement/align `action_state_of`, `mark_delivery_record_durable`, `mark_matching_records_durable`, `filter_records_after_dispatch_failure`, `strip_next_turn_records`, `await_agent_event_queue`, `push_agent_event_task`, and `wait_for_refine_idle` using the corresponding TypeScript flow. Do not replace them with no-ops.
- **Promise ownership:** session event, retry, model-selection, and refinement barriers still contain noncloneable boxed futures behind mutexes and attempts to await guards. Use an owned completion representation with the same ordering/error semantics, then drop guards before waiting. Resolve moved one-shot senders inside `Fn` callbacks through once-only ownership.
- **Model/settings types:** `session.model()` returns `Option<Model>`, model context limits are numeric, supported thinking levels need conversion to the canonical enum, and service tier has nested optional semantics. Several callers still assume incompatible shapes. Make scoped-model mutation work through `Arc<AgentSession>`; `set_scoped_models(&mut self, ...)` cannot serve that API as written.
- **Remaining local duplicates:** heartbeat, autonomous state, goal usage/context, observation inputs, and extension callbacks still differ across modules. Use their canonical modules and preserve messages/errors instead of fabricating matching fields. Session refinement/compaction/navigation calls also have wrong arities and unfinished scheduling; use the diagnostic list and pinned TS methods as the checklist.

### 3. Complete the runtime adapters and real execution paths

**In-process adapter:** implement `InProcessRuntimeHost` against `AgentSessionRuntime::session()` and `services()`. The updated trait needs `session_header`, `session_subscribe`, `session_wait_for_headless_completion`, a `Send + Sync` input-pause handle, and async model cycle/tree navigation/RLM-depth setters. Wire session replacement, rebinding, invalidation, event conversion, and disposal. Required public session capabilities still include headless idle, context tree/last assistant text, refinement, exports, RLM snapshots/quiescence, and bash. In-process side-question startup is still a no-op.

**Daemon adapter:** only `MissingSession` currently implements the daemon's large `DaemonSession` trait. Supply a real adapter to the canonical runtime and reconcile missing binder methods. Fix the `ActiveSessionState.clients` snapshot/live socket ownership mismatch (`Arc<DaemonSocketClient>` versus `Arc<Mutex<DaemonSocketClient>>`) without losing identity. Supervisor verification needs the actual minimal `{generation,pid,processStartId?,socketPath}` claim, not a fabricated full owner record.

**Daemon transport:** fix the async reconnect cycle (`connect → notify_closed → auto_reconnect → connect`), `&self` versus `&Arc<Self>` receiver mismatch, and the classifier's unnecessary `&'static str` requirement. Reconcile SessionManager/cron/ledger/replay/snapshot/event APIs. `AgentDaemon::start` currently does not bind or accept sockets. `daemon_supervisor.rs` is only 270 lines versus 7,389 lines in its TS reference; supervisor operation is not implemented. A CLI/SDK response alone will not prove daemon support.

**Kernel/tool path:** `core/tools/ipython.rs::default_kernel_client_factory` still panics. Adapt the actual `core/kernel/repl_manager.rs::ReplKernelManager`, then fix the non-`Send` provisioner future and `acp_mcp.rs` call to nonexistent `.ensure`. Repair bash child wait/ID borrow, temporary command-value lifetime and numeric truncation access; repair edit preview borrowing. Keep Python protocol v3 and production kernels untouched.

**RLM results:** use canonical `RlmSubagentRuntime { session }`, `RlmListSubagentsResult { subagents }`, and `RlmDeleteSubagentResult { subagent, outcome }`; stale `.handle`, `.agents`, `.deleted`, `.child_id`, and `.message` accesses remain. Then finish child registry, retained-child, deletion, usage, retry, and snapshot conversions.

### 4. Finish remaining compiler groups and targeted tests

- Use the complete final JSONL diagnostics for remaining CLI/main, Telegram, ACP, RPC, agents-view, theme, tree-selector, component, resource, and extension errors. Some new canonical theme interfaces exposed further component errors.
- Check the daemon tree builder: it clones parents before attaching descendants, so nested trees may be incomplete. Prompt highlighting still uses look-ahead unsupported by Rust's regex crate. Invalidation/rebind wiring is incomplete; some daemon saved-session/heartbeat methods remain stubs.
- SDK recorder cleanup currently calls `monotonic_now()` instead of closing. Services telemetry setup remains a no-op. SDK authentication and registry construction still use separate AuthStorage instances. These are explicit runtime follow-ups, not completed repairs.
- Fix the failing foundation serde test, then run the newly edited action-store, extension, settings, resource, tool, RLM, search, and private-framing tests. Test names are embedded in their edited modules; the application tests could not run in this checkpoint.
- `git diff --check` currently reports two trailing-whitespace lines in `extensions/runner.rs` and `rlm_runtime.rs`; formatting cleanup remains. No implementation was continued after the stop instruction.

### 5. Prove basic operation before revisiting merge

1. Build/check the **entire** workspace with all existing application modules enabled. Do not hide failures with `cfg`, remove modules, or replace intentional behavior with default responses.
2. Run the relevant library/module regressions and an isolated executable `--help` launch.
3. Add/run an offline session response smoke through the actual SDK/runtime factory and `AgentHandle`. Use `pi_ai::providers::faux::register_faux_provider`, queue a known assistant response, and assert text, message-end/agent-end ordering, idle settlement, and cleanup. The faux provider requires in-process registration/queued responses; do not assume a bare `--provider faux` CLI flag performs that setup.
4. Cover a tool turn, cancellation, a second turn, and session persistence/resume in the same isolated harness. Then validate RPC and daemon framing/attach/reconnect if those execution modes are claimed working.
5. Only then prepare a mergeable PR with exact passing commands and explicit remaining limitations. Do not install this checkpoint locally or on Axiom.

## Reproduction commands

Use an isolated checkout and private state. The commands below do not require production credentials:

```sh
mkdir -p .port-env/tmp
export CARGO_BUILD_JOBS=4
export CARGO_INCREMENTAL=0
export CARGO_PROFILE_DEV_DEBUG=0
export CARGO_PROFILE_TEST_DEBUG=0
export CARGO_TARGET_DIR=.port-env/target

cargo check --workspace --locked --message-format=json > .port-env/tmp/check-next.jsonl 2> .port-env/tmp/check-next.log
cargo test -p pi-agent-core --lib --locked
cargo test -p pi-tui --lib --locked
# After the application crate compiles:
cargo test -p pi-coding-agent --lib --locked core::session_action_store::tests
cargo test -p pi-coding-agent --lib --locked modes::session_worker::private_framing::tests
cargo test -p pi-coding-agent --lib --locked core::settings_manager::tests
cargo test -p pi-coding-agent --lib --locked core::rlm_runtime::tests
cargo run -p pi-coding-agent --bin optimus-rust -- --help
```

Run checks after coherent integration batches, not after every file. The local compiler was Rust/Cargo 1.95.0; the workspace declares Rust 1.98 but member inheritance/enforcement is incomplete. Record the toolchain actually used; do not silently edit the minimum merely to satisfy a check. The existing `npm run check` uses `biome --write`; install the repository's locked development dependencies in an isolated reference checkout before running it, and preserve the read-only TypeScript baseline. Do not run the prohibited `npm run dev`, `npm run build`, or `npm test` commands.

To inspect a file's frozen diagnostics without opening the entire log:

```sh
jq -r 'select(.file == "crates/pi-coding-agent/src/core/agent_session.rs") | .rendered' evidence/diagnostics/rust-port-handoff-errors.jsonl
```

All implementation agents were stopped. Their ownership manifests remain for provenance; they are not active locks or evidence that every reserved file was edited. Coordinate new ownership before restarting parallel work.
