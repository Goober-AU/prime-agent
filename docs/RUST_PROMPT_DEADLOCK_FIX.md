# Prompt submission blocked by idle maintenance

The installed `bf40c72af` worker remained alive, but its log recorded repeated
`worker_passivate_idle_children` timeouts. The supervisor's cached session list
still responded. Read-only `get_state` requests to both open sessions each timed
out after four seconds. One session had a completed assistant response on disk;
the other had no persisted user or assistant messages. No prompts were submitted
and no sessions were restarted during inspection.

The idle sweep computes `session_passivation_snapshot` before deciding whether a
session can be passivated. Rust computed attached clients by adding
`state.lock().clients.len()` and `state.lock().pending_attaches` in one expression.
The first temporary mutex guard survives while the second lock is acquired,
deadlocking the same non-reentrant session mutex. Later prompts, state reads and
maintenance requests then block on that mutex. This also affects root sessions
that would never be eligible for child passivation.

The fix copies both counters under one scoped guard and releases it before
constructing the snapshot and walking descendants. It preserves the TypeScript
calculation at `packages/coding-agent/src/modes/daemon/daemon-mode.ts:2987`:
`state.clients.size + state.pendingAttaches`. Eviction eligibility, pending
attachment accounting, and the maintenance sweep remain enabled.

No daemon protocol, Python source, TypeScript reference, or UI behavior changes
were required. The fix applies to the shared Rust daemon on all platforms. Web
and 3D surfaces are unaffected; the TUI benefits from prompt and state requests
being able to complete again.

Tests remain deferred at the user's request. Native compilation, TypeScript,
Biome and diff checks are recorded in `evidence/status/rust-prompt-deadlock.json`.
Debugger attachment was unavailable, so this report combines observed live
timeouts with the deterministic lock defect verified in source; it does not
claim an acquired live backtrace. User validation should leave the new worker
open through idle maintenance, then submit another prompt and confirm it appears
and completes normally. Existing deadlocked processes cannot load the fix;
reopening the installed launcher uses the new release's separate daemon socket.
