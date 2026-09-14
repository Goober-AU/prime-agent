# Rust TUI parity handoff

This is an unfinished development checkpoint, prepared at the user's request for
another agent. Keep the PR in draft until the checklist below is resolved and
validated. Do not install this checkpoint over the working release as part of
the handoff.

## Branch and reference

- Repository: `telemusai/optimus-agent`; branch: `fix/pr18-completion`; base: `main`.
- This branch includes both commits from [PR #18](https://github.com/telemusai/optimus-agent/pull/18),
  ending at `3344cc9484afc766f8fb4460a8bbe53215a2f752`, plus the checkpoint changes.
  PR #18 remains open; do not merge the two PRs independently without reconciling them.
- The TypeScript under `packages/` is the behavioral reference. Read `AGENTS.md`,
  `docs/PORT-RULES.md`, and `docs/BASELINE.md` before continuing. Python remains Python.
- PR #18 mentions 130 audit rows, but those reports were not supplied or found in
  the PR. The user explicitly requested proceeding from its description. This
  handoff reconstructs the outstanding feature families; it does not claim to
  account for or close each of those 130 rows.
- File ownership: `evidence/slices/pr18-completion.json`. Current verification:
  `evidence/status/pr18-completion.json`. Extend the manifest before editing new files.

## Changes in this checkpoint

- Rendering: exact RGI emoji recognition with pinned `emojis = 0.9.0`; borrowed
  grapheme scans; UTF-8-safe malformed escape handling; JavaScript whitespace in
  LaTeX; empty/overlapping math delimiters, paragraph boundaries, list indentation,
  and Markdown cache identity fixes.
- Rendering fixtures: TypeScript-generated Unicode 17.0 outputs covering 24,932
  width cases, 19 LaTeX cases, 126 Markdown cases, 630 truncations, 12 wraps, and
  five ANSI stripping cases. The Rust integration file adds huge-input and cache
  regressions. These are bounded samples, not exhaustive rendering parity.
- Native autocomplete: installs the shared completion provider on the real editor,
  translates byte/scalar cursor positions, and polls the debounce. Builtin names,
  aliases, file completion, and effort/heartbeat arguments are connected.
- Queue: browse/edit/delete/reorder wiring, draft restoration, serialized mutations,
  attachment-preserving payloads, session guards, and queue revision tracking.
- History: recent-first window validation, older-page requests and merging, separate
  paged transcript rows, editor history population, and viewport-preserving repaint.
- Menus: real settings selector and persistence callbacks; provider/model/MCP tabs
  using actual selector bodies; independent search; OAuth selector rendering and
  focus; heartbeat manager mounting, action polling, catalog updates, and refresh.
- Terminal: overlay visibility/focus changes schedule repaint and synchronize mouse
  state; settings apply editor layout, cursor, fullscreen, images, thinking, theme,
  and progress preferences to native components.

## Fix these known code-level defects first

These are identified from the current control flow; they have not yet been
reproduced through a full native PTY session.

1. **Streaming response lost on initial attach.** In `native_host.rs`, startup adds
   `snapshot.streaming_message` before `HistoryRuntime::reset`, which replaces the
   transcript and discards it. Restore streaming content after resetting history,
   matching the existing `SessionResynced` ordering. Add a regression for attaching
   while a response is streaming, with and without recent-first metadata.
2. **Authentication state remains stale after login in the configuration menu.**
   `HostEvent::LoginFinished` refreshes the menu's auth storage, but the outer
   `models`/`configured_providers` used by the selection loop are not refreshed.
   Selecting a newly authenticated model can prompt for login again. Refresh the
   daemon catalog and both menu/host state; test login, return to Models, select.
3. **Custom API-key provider selection is overly restricted.** The model selection
   path checks the builtin display-name list, although `provider_options` accepts
   custom API-key providers. Use the reference eligibility rules consistently,
   including providers that must be configured externally.
4. **`/fullscreen` ignores arguments and toggles persisted state.** The command
   handler reads a fresh settings manager, not the current terminal mode, and
   ignores `on`/`off`. Match TypeScript parsing, live-state toggling, persistence,
   and invalid-argument behavior, including CLI-forced fullscreen.
5. **Malformed optional history can abort startup/resync.** `HistoryRuntime::reset`
   returns an error propagated with `?` from the host. Compare TypeScript recovery
   and preserve usable session attachment when optional history is invalid.
6. **Session slash commands are classified locally but not executed.**
   `/compact`, `/refine`, `/goal`, and `/autonomous` become `SlashDispatch::Builtin`;
   the submit path calls `run_builtin_command`, which has no arms for them.
   Route them through the existing session execution path using the reference's
   semantics. Add behavior tests that assert the connection call, not only names.

## Outstanding implementation and validation

- [ ] **Commands.** Complete `/logout`, `/tree`, `/fork`, `/scoped-models`, `/traces`,
  `/update`, `/share`, `/btw` (alias `/side`), `/debug`, and `/mcp` with arguments.
  They currently reach the explicit "native host has no handler" fallback.
  Audit all **37** canonical entries in `core/slash_commands.rs`, plus aliases and
  debug; the older PR's 32-command classification test is not handler coverage.
  Fix the four session commands identified above and verify login/hotkeys/quit
  use their intended owner paths. Verify `/new` alias and
  argument behavior. Prefer the existing core APIs and real components.
- [ ] **Autocomplete catalogs.** Add extension, skill, and prompt commands plus
  model/provider argument sources; refresh after reload, auth/model changes, and
  skill setting changes. Review the synchronous `block_on` filesystem scan against
  TypeScript cancellation/debounce and ensure large directories do not freeze input.
- [ ] **Queue behavior.** In `native_host_queue.rs`, test rapid moves, rejected and
  unsupported mutations, empty edits, lane changes, attachments, dequeue during
  editing, draft restoration, and session switches. Verify event/reply ordering:
  newer queue events must survive late mutation snapshots. The existing test covers
  payload shape, not the full mutation state machine or a real terminal.
- [ ] **History behavior.** In `native_host_history.rs`, exercise actual older-page
  loading, generation/tip changes, overlapping IDs, simultaneous streaming,
  cancellation/session switches, retry after failure, and viewport anchoring. Ensure
  >400 paged messages survive without losing live rows; verify user input history.
  The host test currently validates window shape, not the asynchronous lifecycle.
- [ ] **Heartbeats.** Validate manager actions end to end with a fake connection,
  including failure/retry, session scoping, edit workflows, and timer boundaries.
  Coalesce catalog refreshes and guard late results across session changes; current
  refresh requests can overlap. Review date parsing for malformed/overflow inputs.
- [ ] **Settings and configuration.** Finish the complete TypeScript comparison of
  settings items and effects, search, resize, key hints, and nested overlays.
  Test remote-setting rejection and persistence; settings currently await remote
  changes on the owner loop, so responsiveness needs review. Verify theme cancel
  and preview, model search/recents/scoped cycling, OAuth/manual/API-key flows,
  failed/cancelled login, and callback ordering after a newer login begins.
  `ModelSelectorComponent`'s `has_header` calculation still only checks static
  `header_rows`, despite the new dynamic header support. Review and remove obsolete
  private selector paths only after behavior is verified.
- [ ] **Terminal/overlay core.** Complete the 17-area audit from PR #18. `tui.rs`
  and text utilities were read; this checkpoint does not complete terminal,
  terminal-colors, terminal-image, or stdin-buffer parity. Port the corresponding
  TypeScript tests for capture/non-capture, short overlays, style leaks, mouse
  routing, ANSI wrap, colors, images, and fragmented stdin.
  Inspect `TUI::hide_overlay` for stale handle focus/removal state. The native
  viewport shortcut gate uses `has_overlay()`; verify non-capturing autocomplete
  does not block scrolling. Cover resize and fullscreen transitions at narrow sizes.
- [ ] **Rendering remainder.** Extend differential cases to column slicing,
  extracted segments, visible spans, hyperlink hit testing, and terminal output
  normalization. Add true streamed-prefix rendering assertions: the current test
  named `inline_display_and_streamed_math_match_typescript` renders fresh instances
  for fixture inputs, not every prefix through a reused instance. Add consecutive
  empty-cache renders and richer styled Markdown. Measure huge-input time/memory
  as well as the already-checked bounded truncated output.
- [ ] **Daemon caveats inherited from PR #18.** Recheck that direct peer transport
  remains unadvertised before accepting the missing `DaemonWorkerClient`
  `reconnect_boxed` path. Review the missing TypeScript forward-to-worker recovery
  touch in `daemon-supervisor.ts` around lines 5349-5355. Do not describe these as
  fixed by this checkpoint. No new wire commands or schema changes are introduced
  by the new UI adapters; preserve existing capability checks when continuing.
- [ ] **Final release gate.** Run full isolated Rust suites after fixes, required
  `npm run check`, focused regressions, and a real native PTY/fake-provider smoke.
  Validate Unicode rendering, model search/login, settings, queue edits, history,
  heartbeats, resize, mouse, and fullscreen. Build the actual launcher binary;
  this checkpoint has only library/test compilation evidence. Add release
  changelog fragments through the repository's agreed port/release workflow,
  reconcile PR #18, refresh the PR description, and request review before merge.

## Reproducing verification

This machine's worktree is `/home/anthony/optimus-rust-pr18`; the shared target is
`/home/anthony/optimus-rust-port/.port-env/target`. Rust 1.95.0 and Node 25.5.0
(Unicode 17.0) were used. Keep builds at one job on this machine: disk space is
limited. Do not clean unrelated worktrees, caches, SDKs, or the user's data.

```sh
export CARGO_TARGET_DIR=/home/anthony/optimus-rust-port/.port-env/target
export CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0
export CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0
cargo test -p pi-coding-agent --lib --no-run --message-format=json
cargo test -p pi-tui --no-run --message-format=json
npm run check
```

Build from the repo root. Run compiled test executables from their crate roots in
an isolated environment. For the coding-agent binary, focused filters are
`native_host`, `configuration_menu`, `heartbeat_manager`, `oauth_selector`,
`model_selector`, and `menu_panel`; pass `--test-threads=1` because some tests use
global theme/environment state. Run both the pi-tui unit and `pr18_rendering`
executables. Obtain executable paths from Cargo's `compiler-artifact` JSON records.

The Linux verification uses `bwrap --unshare-net --unshare-pid`, a private writable
HOME and temporary directory under `.port-env/tmp/pr18`, and a 6 GiB address-space
limit via `prlimit --as=6442450944`. `/home/anthony` is hidden with tmpfs and only
the worktree, target, and scratch directory are remounted; the repository and target
are read-only. Use equivalent isolation on another machine. Never run these tests
against real login files, provider tokens, an existing daemon, or the installed
Optimus/Prime profile. Use the repository's faux provider for PTY integration.

Regenerate reference rendering data only with a Node runtime using Unicode 17.0:

```sh
node --import tsx crates/pi-tui/tests/fixtures/pr18_rendering.ts
```

Local raw build/test logs are under `.port-env/tmp/pr18/handoff-*.log`. Older
non-handoff logs include failed intermediate runs and are not current evidence.
Only the counts and limitations recorded in the committed status file are claimed.

## Local worktree cautions

The unrelated `scripts/rust-env.cmd` line-ending change and `node_modules` symlink
are intentionally excluded from the checkpoint. Do not stage them. The `origin`
remote points to `PrimeIntellect-ai/prime-agent`; use the `telemusai` remote for this
handoff branch. Do not force-push, rewrite PR #18's commits, or merge on the basis
of the older PR's test totals. No production config, login, installation, or running
daemon was changed for this handoff.
