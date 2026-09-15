# Memory imports and terminal completion status

## Changes

Python `memory.import_run()` now receives the owning session's current model
through `model.info`, including model changes made after kernel startup. The
memory host resolves the model and credentials from the same agent profile as
the TypeScript implementation. A model registered only in the owning session
still reports the TypeScript instruction to use `/memory import-run` there.
Non-extracting memory operations do not request model information or credentials.

The Python and slash-command import extractors share completion plumbing. They
record input plus cache-read/cache-write tokens and output tokens from each
planning/repair response, retain the configured provider retry policy, and return
only cited `create memory` edits. The existing prepare/run/apply separation and
revision checks remain in force. Source references:

- `packages/coding-agent/src/core/agent-session.ts`: `model.info` host handler.
- `packages/coding-agent/src/core/tools/ipython.ts`: `createMemoryHostHandlers`.
- `packages/coding-agent/src/core/memory/service.ts`: `import_run` extractor.
- `packages/coding-agent/src/core/extensions/builtin/memory.ts`: `extractor`.
- `packages/coding-agent/src/core/refinement/refinement.ts`: planning/repair
  response usage accounting after `completeWithProviderRetry`.

The daemon connection previously spawned a separate Tokio task for every
transport message. A later sequenced status event could advance the cursor
before `agent_end` ran, causing the completion event to be discarded as stale.
Messages now enter one ordered queue. Each handler runs through its first
suspension in arrival order, matching the TypeScript callback. Its asynchronous
completion remains independent so listeners and recovery can await later socket
messages without blocking their delivery.

While the TUI shows activity, it also performs at most one background state read
every five seconds, with a 30-second request timeout. Busy cached snapshots are
queried against the worker. A reply is applied only if its request generation,
session identity, and local state revision still match. Unrelated side-question,
recap, and extension events cannot invalidate that reply. A confirmed idle reply
clears loaders; a timeout or transient socket failure does not imply completion.
The existing terminal worker-failure diagnostic remains visible.

No daemon command, capability, event, or response shape changed; the existing
`get_connection_state` request supplies reconciliation. No protocol version or
schema revision change is required.

## Incident observations and limits

Read-only inspection of the latest installed worker on 2026-09-15 found a final
assistant message at 02:50:53 UTC and subsequently confirmed `isStreaming=false`,
`isRunningTools=false`, and `isCompacting=false`. The responsive TUI symptom is
consistent with a lost completion event. The event race above is verified in
code; this observation alone does not prove which event raced in that UI.
The running worker and its transcript were not interrupted or modified.

Build and static-check results are recorded in
`evidence/status/memory-import-waiting.json`. No tests, fake-provider runs, real
provider calls, or candidate model sessions were executed, per user instruction.
Live rendered completion, reconnect/attach races, model changes before Python
imports, repair-response usage, and import/apply behavior remain for user testing.

This change targets the Optimus Rust host and TUI. Shared memory behavior also
serves its existing non-TUI clients. The separate `vr-ai-chat` 3D and Web surfaces
are unaffected; no cross-product rendering parity is claimed.
