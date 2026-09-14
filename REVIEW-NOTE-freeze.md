# Rust port — review note (freeze point)

**Branch:** `fix/rust-cli-runtime` — **PR #15** (cross-repository, head `Goober-AU/prime-agent` → `telemusai/optimus-agent`)
**Freeze commit:** `6ee4f93f5`
**TypeScript reference:** `9f547ceaba079dee45ee415a430d57fb86fa940d` (v0.9.3)

## What is DONE and verified

| Check | Result |
|---|---|
| `cargo check --workspace --tests` | **EXIT=0, 0 errors** (379 warnings) |
| `pi-coding-agent` test suite | **2160 passed / 0 failed** (stable at 1, 4 and 8 threads) |
| `pi-ai` test suite | **667 passed / 0 failed** (threads=1) |
| `pi-tui` test suite | **217 passed / 0 failed** |
| No stubs | zero `todo!()` / `unimplemented!()` in the port |
| Mapped files | 412 / 412 present |

The user's original directive — **all 178 originally-failed tests** — is complete.

## What is NOT done (read this before reviewing)

### 1. Known FLAKY tests under parallel execution (`--test-threads>1`)
Both crates are **green single-threaded and green in the common case**, but two non-env races remain:

- `cli_entry::tests::failed_login_reports_the_error_message_not_the_provider_id`
  — a peer test calls `reset_oauth_providers()` (a process-global OAuth registry) and clears the provider this test registered. Observed: ~3 failures in 10 runs at `--test-threads=4`.
- `session_resources::tests::cleanups_run_in_registration_order_and_unregister` and `failing_cleanup_is_reported`
  — two tests share one process-global cleanup registry, so one sees the other's panicking callback (count 2, expected 1).

Both need the same body-held-lock treatment already applied to env vars. **Files:** `crates/pi-ai/src/cli_entry.rs`, `crates/pi-ai/src/session_resources.rs`.
**Not started.** These are test-isolation defects, not production behaviour defects.

### 2. Env-isolation fix — landed but WITH NO TEETH PROOF
`crates/pi-ai/src/test_env.rs` + 14 converted files are committed. The conversion was verified by the compiler and by `git grep` (zero raw `env::set_var` remain in `crates/pi-ai/src`), and `pi-ai` is 667/0 single-threaded.
**However the break-and-revert teeth probe was cancelled by the freeze, so the isolation fix is NOT independently proven.** Treat it as compiled-and-green, not as proven.

### 3. Rounds of audit still open
- **Two slices have never been audited:** `telemetry.rs` (~lines 1180-1741) and `core/refinement/`.
- **~12 low/medium audit items** not actioned, incl. S-02 (MCP servers dropped), S-03 (`pi update` exits 0), C4-01 (tool_call veto bypassed), TUIR-7 (fullscreen mouse hardcoded), TUIR-10 (loader never requests a render), TUIR-23..34.
- **Markdown math:** 53 of 63 differential cases byte-exact; **10 known divergences remain** (empty `$$$$`, task lists, missing blank line before a list, `~single~` strikethrough, leading/trailing blank runs, html-block blank lines).

### 4. Deliberate, documented capability gaps (not bugs)
These are honest divergences, named in code with comments, and **must not be read as working features**:
- AWS SDK `defaultProvider` chain (IMDS, ECS, web-identity, SSO, `credential_process`) — not implemented; `<authenticated>` advertising was corrected to match what IS implemented.
- NFKC normalisation in edit-diff (no NFKC implementation in the workspace).
- Stale `Emoji_Presentation` table feeding `has_emoji_presentation`.
- 2.5 / 2.9 from the round-1 audit: capabilities deliberately **withheld** rather than faked (advertising them would require implementing them).

### 5. Pre-existing, environmental test failures
- 178 pre-existing failures (recorded in `evidence/diagnostics/gt1-*.json`) — **all now fixed**.
- Tests that depend on ambient credentials: handled by restoring the TypeScript's own env preconditions; they now pass **with** ambient keys present.
- `pi-ai` provider suites at very high parallelism previously OOM'd (~37 GB) on this host; they are safe at the configurations used here.

## Verification method used throughout
- Every fix cites a TypeScript `file:line`.
- New tests were proven to have teeth (break → observe failure naming the real symptom → restore), except where noted above.
- Commits were scanned for leftover mutation markers before staging (two near-misses were caught this way).
- No test was weakened or deleted; the few assertion changes are each justified from **measured** pinned-TypeScript behaviour (run under Node on this host), never to match the Rust.

## Test-isolation caveat for CI
If CI runs with default parallelism (`--test-threads` > 1), expect **occasional** failures from item 1. Use `--test-threads=1` for a deterministic result until that item is fixed.
