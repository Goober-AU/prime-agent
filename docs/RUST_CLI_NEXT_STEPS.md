# Rust CLI continuation handoff — 2026-09-13

This checkpoint continues PR #13 and is intentionally a draft. **The final source has one test compile error. No successful end-to-end Rust prompt has been demonstrated.** The user stopped implementation and requested this handoff for the next model. Do not deploy, merge, replace `prime-agent`, or describe this as full TypeScript parity.

Repository: `telemusai/optimus-agent`, not PrimeIntellect upstream. Branch: `fix/rust-cli-runtime`. Parent checkpoint: `f34aafb5fc1e451e4bd3fec77be9f4bf7f2872a0` (PR #13). Read-only TypeScript reference: `9f547ceaba079dee45ee415a430d57fb86fa940d`, available under `packages/**` in this checkout. The large PR diff against main includes the inherited Rust port; review this repair separately with `git diff f34aafb5fc1e451e4bd3fec77be9f4bf7f2872a0 HEAD`.

Read `AGENTS.md`, `docs/PORT-RULES.md`, and `docs/BASELINE.md` first. Read complete files before editing; register exact ownership under `evidence/slices/`. Keep Python and `packages/**` unchanged. Use fake providers only, private configuration/session/socket/kernel paths, and at most four Cargo jobs. Preserve real behavior and errors; do not make tests pass by inventing successful responses, disabling assertions, bypassing the daemon, or advertising unimplemented capabilities.

## First action: fix the final compile error

The completed, source-stable final command was:

```sh
cargo check --workspace --tests --locked --message-format=json
```

It exited 101 with **one unique error**:

```text
crates/pi-coding-agent/src/core/kernel/repl_manager.rs:3865
no field `message` on type `kernel::shared::KernelError`
```

In `first_execution_and_followup_reach_the_kernel_write`, `result.unwrap_err().message` incorrectly treats the `KernelError` enum as a struct. Inspect `core/kernel/shared.rs` and assert its real variant or Display output (likely `result.unwrap_err().to_string()`). Keep both first and subsequent execution assertions and their timeout. Recheck the whole workspace with `--tests`; do not omit this test.

## What already changed

- `cli_entry.rs` now creates Tokio and calls `cli_main_entry::run_cli`; previously every CLI invocation exited successfully with empty output. `native_main_host.rs` implements the real startup adapters. Native subprocess launch no longer sends the executable path as a prompt argument. Early daemon startup now actually runs its future.
- `modes/daemon/native_supervisor.rs` implements initial supervisor ownership, durable worker descriptors, authenticated worker channels, public transport, create/attach/list, command recovery, input pauses, lifecycle handling, and catalog subprocesses. `daemon_server.rs` implements actual sockets, framed worker traffic, subscriptions, shutdown and cleanup. These are substantial new paths, not full supervisor parity.
- Runtime construction, SDK prompt wiring, ordered event delivery, live daemon snapshots, print/RPC adapters, and multiple nested-mutex defects were repaired. Flat/slim streamed events recover their typed partial message at deserialization.
- Native interactive/agents bridges implement terminal input/rendering, login, basic extension dialogs, and chat/roster return results. The roster server needed by agents view is still absent; see below.
- The default Python tool now reaches `ReplKernelManager`; native owned-worker process/stdio/owner-watch adapters are implemented. The initial kernel execution queue now starts resolved, as the TS `Promise.resolve()` queue does.
- Memory, tools, MCP, provider serialization/signatures, registry ordering, session-tree construction, ledger async reads and transcript chunk loss received fixes. The Codex WebSocket error iterator now terminates instead of allocating indefinitely. Several bad port-test fixtures were corrected against the pinned TS; details are in `evidence/status/runnable-*.json`.

## Exact verification state

These results are from different source checkpoints. **Do not combine them into a claim that the final tree passes.**

| Run | Observed result |
|---|---|
| Unmodified PR #13 on this Linux host | Workspace check/build passed; coding-agent 1,903 passed / 166 failed / 2,069 executed. Help/version/print all produced empty output. |
| Integrated repair check #3, before later fixes | Workspace `--tests` check passed; 343 unique coding-agent warnings. |
| Later test executable build, before final fixes | `cargo test --workspace --tests --no-run --locked` passed in 1m35s. |
| Terminal and agent-core tests from that build | `pi-tui`: 176/176; `pi-agent-core`: 37/37. |
| Full provider run | Bounded at 180s because four helper-stream tests never ended their test-owned streams. Memory was about 34 MB, not the previous ~37 GB OOM. |
| Provider diagnostic with those four tests filtered | 525 passed / 55 failed / 4 filtered, complete. Repairs for these failures and hangs are saved but not rerun. |
| Full coding-agent run | Bounded at 180s on the new real SDK/faux regression. |
| Coding diagnostic filtering only that SDK test | 1,953 passed / 144 failed / 1 filtered, complete. 142 failures were theme-initialization/poison cascades, plus Ctrl+C routing and diff-spacing assertions. Repairs are saved but not rerun. |
| Real SDK/faux regression alone | Timed out after 30s in session construction. GDB identified the double settings lock below; fix saved, not rebuilt into a tested artifact. |
| Actual CLI integration test | Reached a real daemon hello, then failed an unnecessary `agent_roster` assertion. Test now checks canonical protocol compatibility; text/JSON response path has not run successfully. |
| Direct Python protocol smoke | Passed using `/usr/bin/python3`, existing Python source, and private state: ready, persistent arithmetic, Unicode, host request/reply, shutdown. |
| Rust default-Python-tool smoke | Timed out before first arithmetic result after 15s. The unresolved initial queue was found and fixed; requires rebuild and rerun. |
| **Final source-stable workspace check** | **Exit 101, one test compile error described above; unique warnings: pi-ai 16, pi-tui 5, pi-coding-agent 337.** |
| `npm run check` | Exit 127: `biome: not found`; Node dependencies are absent. No TS files were edited. |

`evidence/diagnostics/runnable-cli-handoff.json` records the final summary and diagnostic failure names. Local full logs remain in `.port-env/tmp/runnable-validation/`; they are not shipped or required for reproducing the commands below. Do not treat old `docs/HANDOFF.md` or slice-local pre-final success notes as final-head results.

## Rebuild and prove a real first response

On Linux, use private build/runtime roots. Rust/Cargo 1.95.0 built this checkout; workspace metadata says 1.98 but member crates currently do not inherit that requirement. Do not lower manifests just to silence the metadata.

```sh
cd /path/to/optimus-agent
export CARGO_BUILD_JOBS=4 CARGO_INCREMENTAL=0
export CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0
export CARGO_TARGET_DIR="$PWD/.port-env/target"
mkdir -p .port-env/tmp/next-run
cargo check --workspace --tests --locked
cargo test --workspace --tests --no-run --locked --message-format=json \
  > .port-env/tmp/next-run/test-build.jsonl
```

On the original Linux workstation only, the warm target cache is `/home/anthony/optimus-rust-port/.port-env/target`; the current repair checkout is `/home/anthony/optimus-rust-pr13`. Do not accidentally edit the older `optimus-rust-port` checkout. Neither cache path is required on another computer.

Use the test executable emitted by Cargo, not an old binary found by a wildcard. This helper extracts it:

```sh
artifact() {
  python3 - "$PWD/.port-env/tmp/next-run/test-build.jsonl" "$1" <<'PY'
import json, sys
paths = []
for line in open(sys.argv[1]):
    try: row = json.loads(line)
    except ValueError: continue
    if row.get('reason') == 'compiler-artifact' and row.get('profile', {}).get('test') and row.get('executable') and row['target']['name'] == sys.argv[2]:
        paths.append(row['executable'])
assert paths, 'No fresh test artifact found'
print(paths[-1])
PY
}
```

Run candidates with external networking and the real home directory hidden. This Linux/Bubblewrap function retains loopback for the synthetic HTTP fixture. Use a fresh `next-run` root for a second concurrent test process so process-ID-based fixtures cannot collide.

```sh
repo_dir="$PWD"
run_root="$repo_dir/.port-env/tmp/next-run"
mkdir -p "$run_root"/{agent,sessions,tmp,workspace}
run_isolated() {
  test_cwd="$1"; shift
  env -i PATH="$PATH" HOME="$HOME" LANG=C.UTF-8 TERM=xterm-256color \
    PI_OFFLINE=1 PI_SKIP_VERSION_CHECK=1 RUST_BACKTRACE=0 \
    PRIME_AGENT_CODING_AGENT_DIR="$run_root/agent" \
    PRIME_AGENT_SESSION_DIR="$run_root/sessions" \
    bwrap --ro-bind / / --unshare-net --unshare-pid --die-with-parent \
      --tmpfs "$HOME" --ro-bind "$repo_dir" "$repo_dir" \
      --ro-bind "$CARGO_TARGET_DIR" "$CARGO_TARGET_DIR" \
      --bind "$repo_dir/.port-env/tmp" "$repo_dir/.port-env/tmp" \
      --bind "$run_root/tmp" /tmp --dev /dev --proc /proc \
      --chdir "$test_cwd" -- "$@"
}
run_isolated "$repo_dir/crates/pi-coding-agent" timeout 35s \
  "$(artifact pi_coding_agent)" \
  core::sdk::tests::real_session_prompts_the_faux_provider_and_delivers_events \
  --exact --nocapture
```

The SDK test must complete, show the genuine user prompt reaches the faux provider, and assert streamed/persisted assistant text and lifecycle events. **A bare CLI `--provider faux` does not register the faux provider**; the test registers it in process.

The already-saved first blocker is in `core/agent_session/runtime_members.rs`, `AgentSession::build_runtime`, around line 593: the `IpythonToolOptions` initializer formerly locked `settings_manager` for `command_prefix`, then relocked it for `shell_path` while the first temporary guard was alive. Both values now come from a single scoped read. If the test still hangs, obtain a bounded all-thread backtrace and find the next concrete lock/await; do not increase the timeout indefinitely or skip the real prompt.

## Then exercise the executable, Python, and terminal

1. Run the actual CLI test:

   ```sh
   run_isolated "$repo_dir/crates/pi-coding-agent" timeout 120s \
     "$(artifact native_cli)" --nocapture
   ```

   `crates/pi-coding-agent/tests/native_cli.rs` creates a private localhost SSE model and starts the actual `optimus-rust --mode daemon`, then actual text and JSON `--print` clients. It asserts `LOCAL_HTTP_FIXTURE_RESPONSE`, real HTTP receipt of both prompts, final assistant JSON events, and child cleanup. Follow create → authenticated worker → attach snapshot → prompt → streamed events if it fails. Fix the wire/runtime owner rather than bypassing the supervisor. The earlier failure was solely the fixture's optional-roster assertion; that assertion is now replaced with canonical protocol equality.

2. Build the command (`cargo build -p pi-coding-agent --bin optimus-rust --locked`) and run isolated `--help` and `--version`. They must produce useful text/version and exit zero; malformed arguments must return a useful error. There is no final-head help/PTY smoke yet.

3. Rerun `core::kernel::repl_manager::tests::first_execution_and_followup_reach_the_kernel_write` after fixing its compile assertion. Then exercise the **default** Rust IPython factory with the existing Python runtime. Verify first execution, a persistent variable across calls, Unicode stdout, host request/reply, namespace, and clean shutdown, with per-operation timeouts. Queue initialization at `repl_manager.rs:504` now calls `settle(Ok(()))`; TS `repl-manager.ts:215` uses `Promise.resolve()`. Do not substitute a mock manager or run/install in the production venv. Original-machine scratch proof/harness is under `.port-env/tmp/kernel-protocol-smoke/`.

4. Implement the roster blocker **before claiming the default terminal works**. `modes/agents_view/roster_store.rs:287` requires `agent_roster`; `modes/daemon/native_supervisor.rs` still rejects `roster_subscribe` / `roster_unsubscribe`. No roster implementation was started in this checkpoint. Use the canonical `modes/daemon/agent_roster.rs` model. Register worker frame listeners **before authentication** so the initial worker roster snapshot cannot race them. Consume `DaemonWorkerRosterOutbound` snapshots/deltas/heartbeats, maintain real owned-worker visibility and lifecycle passivation/removal, serve subscriptions and public `roster_update`, merge saved-session catalog rows into `list(all=true)`, and derive busy counts from actual state. Only then advertise the capability. TS anchors: `daemon-supervisor.ts:2133`, `2844`, `4569–4930`; Rust helpers: `daemon_worker_protocol.rs`, `daemon_session_list.rs`, `daemon_catalog_process.rs`. Do not remove the client's capability gate to make the UI appear to open.

5. Perform an actual 80×24 terminal smoke: open private Rust CLI, render header/editor, type a mock-model prompt, observe streaming/final response, interrupt, navigate to/from the agent list, and quit with the configured keys. Verify raw mode/cursor are restored and owned processes exit. Native adapters: `modes/interactive/native_host.rs`, `modes/agents_view/native_host.rs`, `pi-tui/src/terminal.rs`. The saved theme fixture initializes the theme before real splash components; earlier missing initialization poisoned the shared mutex and cascaded into 142 failures. Ctrl+C must use the correct app action, not the generic select-cancel binding.

Always pass an explicit private `--daemon-socket` for runtime launches. The binary's default socket has the same name as the TypeScript daemon. A generic wrapper that prepends this flag breaks several public subcommands; the public shutdown command can enumerate multiple daemons. Do not use it to clean up a smoke test. Test-owned PID namespaces/process handles are the cleanup boundary.

## Finish verification and remaining runtime gaps

Run all four test executables with `--test-threads=4` and external networking disabled. The four previously hanging provider tests now explicitly end their test-owned helper streams; retain them in the final run. Diagnostic `--skip` runs above were triage, not acceptance. Run the native CLI integration test separately and capture full output/exit codes. Final acceptance requires no failures/timeouts, successful real CLI mock response, actual terminal behavior, and real default-Python-tool execution.

After first-response proof, repair post-compaction continuation in `core/agent_session.rs` against TS `agent-session.ts:8429–8658`. Current settlement does not clear its ownership slot, cancellation can discard it without waking waiters, and the scheduled runner never calls `agent.continue_()`. Preserve queued-input settlement and typed `AgentContinueError` handling. The first-response SDK fixture disables auto-compaction, so its success will not prove this path.

The original 26 blocked adapter seams are only partially resolved. Audit `core/agent_session_runtime/{daemon_adapter,in_process_adapter}.rs` against real runtime owners. Remaining gaps include child roster/watch/cancel/register, context-tree projection, scoped model mutation, transport settings, manual compact/refine results, navigation results/options, JSONL export, side-question start/abort, and daemon execution/environment forwarding. Some `blocked_on` comments beside implemented forwards are stale: inspect bodies and callers before deciding which are still missing.

Further supervisor work remains: chunk/history cache, global heartbeat, idle eviction/scheduled wake, update/restart transaction, full retry/recovery ladder, dead-worker owning-environment recovery, and complete backpressure/RLM lifecycle parity. The native UI also lacks some advanced extension status/widget/indicator behavior. Windows named-pipe/startup-gate/process adapters have source implementations but no Windows execution proof; Vertex ADC retains inherited gaps. Passing Linux unit tests is not evidence these features work.

Record final exact source/check/test results, repair remaining failing cases against TS, then mark the PR ready. Keep this checkpoint a draft until that work is done.
