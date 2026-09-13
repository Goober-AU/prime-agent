# HANDOFF — Rust port of the first-party TypeScript app layer

> **Latest checkpoint (2026-09-13):** continue from [RUST_CLI_NEXT_STEPS.md](RUST_CLI_NEXT_STEPS.md).
> It supersedes the current-state claims below: the final CLI repair checkpoint has one test compile error,
> saved but unverified deadlock fixes, and an unfinished supervisor roster. The older results below are historical.

**Target:** `C:/Users/openclawuser/optimus-rust-port`, branch `fix/rust-port-completion`
**Pinned reference:** commit `9f547ceaba079dee45ee415a430d57fb86fa940d` (build optimus-main-20260911, v0.9.3)
**Scope:** behaviourally faithful Rust port of `packages/agent`, `packages/ai`,
`packages/coding-agent`, `packages/tui`.

## Current state (2026-09-12)

**`pi-coding-agent` compiles with 0 errors.** This is the first time the crate has compiled.

| Gate | Reading | Notes |
|------|---------|-------|
| fresh handoff baseline | 1,108 unique | pre-repair |
| best intermediate | 27 unique (gate20) | pack phase peak |
| **gate43 (current)** | **0 errors, 708 warnings, `Finished`** | complete run, `--tests` |

Test corpus in the crate: **2,062** `#[test]` / `#[tokio::test]` attributes.
Workspace totals: pi-ai 582, pi-agent-core 37, pi-tui 174, pi-coding-agent 2,062.

## What the repair phase changed

Fifteen packs (a-m) drove the crate from 1,108 to 0 unique errors. Highlights:

- **`ExtensionBindings` Value seam deleted** (`b8c0bae14`). The Rust code had invented a
  `serde_json::json!({"uiContext": ...})` stand-in. The TypeScript passes LIVE objects
  (`in-process-agent-connection.ts:655-672`, `daemon-extension-binding.ts:84`), so the seam
  silently dropped `commandContextActions`, `shutdownHandler`, and `onError`. Retyped to the
  canonical owners.
- **`ActiveSessionState.clients` shape fixed** (`8215e8347`): `Vec<Arc<DaemonSocketClient>>` →
  `Vec<Arc<StdMutex<DaemonSocketClient>>>`, verified against `daemon-mode.ts:3533-3545`.
- **Duplicate-stub migration finished** (`d50fa50e5`, `bd111dfb6`): lossy local `SessionState` /
  `AgentStatusRecord` / `SessionInfo` / `AgentCronJob` stubs deleted in favour of the canonical
  `core::session_manager` and `core::cron_jobs` owners. The invented `heartbeat: bool` field was
  replaced with the real `is_heartbeat_cron_job(job)` predicate.
- **Send/guard-across-await family fixed.** Proven shapes:
  1. `drop(guard)` is NOT enough — the binding lives to end of scope. Use a scoped block that
     produces an owned value.
  2. A guard in a `while COND { ... await ... }` predicate stays live across the body's await.
     Read the predicate into owned booleans first (3 sites).
  3. When a callee takes `&mut T` and awaits inside its own body, no caller-side trick works.
     The callee must take `&Arc<StdMutex<..>>` and lock only in a synchronous prologue.
  4. Recursive async cycles need `Box::pin` on ONE cycle edge (E0733).
- **Serde tag duplication fixed** in `pi_ai::types`: internally-tagged enums whose payloads also
  carried the tag emitted the key twice, so those values could never deserialize.
- **`daemon_adapter.rs`** (the last blocker): all **57** missing `DaemonSession` trait members
  written. 31 are real forwards to a canonical owner; 26 return an explicit `Err`/empty value
  with a `blocked_on:` reason because the owner lives on `AgentSessionRuntime` (which this adapter
  does not hold) or is private. No `todo!()`, no `unimplemented!()`, nothing fabricated.
- **`in_process_adapter.rs`**: all 15 omitted members written (`db0cbc132`).

## Measurement gate (mandatory)

Every count quoted in this repo passed all four rules:

1. **Complete run** — the stderr tail says `could not compile ... due to N previous errors` or
   `Finished`. A partial run is not a reading.
2. **Crate-wide warnings > 0** — zero warnings means rustc aborted early; discard the reading.
3. **`--tests`** — a lib-only check hides test-module errors.
4. **UNIQUE `(file, line, message)`** counts — never a raw or repeated count.

Add two more checks when a writer may be live:

5. **mtime-stability** — record file mtimes before and after the run; if a live file changed
   during it, the reading may be mid-write.
6. **rustfmt parse probe** — `rustfmt --edition 2021 --emit stdout <file>` must exit 0 before any
   result is reported.

## Commit policy (learned the hard way)

- NEVER stage a file a live pack owns. A mid-save capture truncated `daemon_mode.rs`
  (incident 2026-09-12).
- After a pack closes, **prove its fixes are in a commit**. `safe_commit` withheld finished-pack
  work once; `773bdd0b6` had to land it separately.
- Never run whole-tree git operations (`stash`, `reset`, `checkout`, `clean`) while any writer is
  live. Compare via a copy instead.

## Windows hazards

- `> NUL` creates a real 68 KB file named `NUL`. Use `> /dev/null`.
- Commit messages containing backticks get command-substituted by bash. Avoid backticks in `-m`.

## Remaining work

1. Run `cargo test` for the workspace and record executed/failed counts honestly.
2. Refresh `docs/MIGRATION-MAP.md` against the compiled tree.
3. Run the differential parity harness against `evidence/parity-*.json` baselines.
4. Port the remaining TS test corpus for full parity (`evidence/test-corpus-inventory.json`
   records 527 TS test files).
5. Resolve the 26 `blocked_on` seams recorded in `daemon_adapter.rs` and `in_process_adapter.rs`
   by giving the adapters a real runtime handle.

## Known unrelated issue

`pi-ai` provider tests allocate ~37 GB and OOM. This is pre-existing and unrelated to the port
repair.
