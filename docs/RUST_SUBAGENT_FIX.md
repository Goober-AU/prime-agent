# Rust subagent runtime and status bar

The terminal now mounts the existing TypeScript-derived subagent summary beneath the editor, including the fullscreen dock. It displays running, idle and inactive child counts, shows when the parent is waiting for children, and opens the scoped agents view with Down then Enter. The editor draft survives that handoff. Initial snapshots, child events and the capability-gated daemon roster update the bar after attachment and reconnection.

Received agent messages use the dedicated message component. Sent receipts retain their ID, text, relationship and destination and appear in the Python tool component. The configured message-expansion shortcut applies to both, including loaded history.

The missing runtime path was also connected: `rlm.run` admits and starts real child sessions with inherited model, thinking, tools, depth and environment; daemon-owned children publish their durable parent edge before becoming available to messaging. `rlm.find_models`, `rlm.list_subagents`, `rlm.delete_subagent`, agent messaging and observation reach their existing handlers. Child events, cancellation, completion/failure notices, retained sessions and in-process child inspection now have production callers. Completed child usage is attributed to the durable parent message.

Deletion settles the retained completion promises before removing tracking and releases child resources. Registry deletion now matches the ledger's parent path rather than its child path. This change does not access or repair the supervisor ownership registry.

## TypeScript references

- `packages/coding-agent/src/core/agent-session.ts`: `_startRlmChildRun`, `_awaitPendingRlmChildPublication`, `registerRlmChildSession`, `releaseRlmChildSession`, `_finishRlmRunDeletion`, `getRlmChildSnapshots`, `getRlmChildSession`, `cancelRlmChildRun`.
- `packages/coding-agent/src/modes/daemon/daemon-mode.ts`: `createSubagentRuntimeHost`, `createRlmSubagentRuntime`, `recordRlmSubagentDeletion`.
- `packages/coding-agent/src/modes/interactive/interactive-mode.ts`: summary mounting, `subscribeToRosterBar`, child-event handling, received/sent messages and scoped agents handoff.
- `packages/coding-agent/src/modes/interactive/components/subagent-summary-line.ts`: the existing Rust component supplies count classification and rendering.

## Scope and validation

This change restores existing protocol surfaces; it adds no daemon command, event or capability. Roster subscription checks the existing `agent_roster` capability and falls back to session child snapshots when unavailable. Connection projections preserve existing numeric token values and the complete kernel receipt payload.

Tests were explicitly deferred by the user. No tests or provider requests were run, and no test assertions were changed. Compilation and static checks are recorded in `evidence/status/subagent-runtime-status.json`; they do not establish live rendering or end-to-end behavior. Windows runtime validation remains outstanding. Durable usage is written per completion; the TypeScript batching and immediate in-memory parent usage folding are not reproduced here.

The affected surfaces are the Rust TUI, its agents view, shared session runtime and daemon, plus the ACP event projection. This is the Optimus repository; it does not contain the separate VR project's 3D/Web surfaces. Existing TypeScript source is unchanged.

An installed update applies to newly launched clients and workers. Existing daemon workers keep their loaded executable until restarted; installation preserves running sessions and their configuration.
