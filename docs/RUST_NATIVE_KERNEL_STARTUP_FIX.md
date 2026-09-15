# Native subagent startup and roster settlement

The Rust daemon could freeze on the first `await rlm(..., name=...)`. Python
had already completed its ready handshake, restored state and executed the
runtime bootstrap. The blocking operation was the queued child roster update.

`AgentDaemon::queued_child_roster_entry` read `cwd` under the parent session
manager mutex inside a struct initializer. Rust kept that temporary guard alive
until the entire initializer ended. Its later `session_id()` and
`session_file()` calls acquire the same mutex, causing a self-deadlock. The
shared worker stopped publishing events, so the UI retained a Python startup
message and no child handle returned. Reading `cwd` in a separate statement
releases the guard before those getters run. The TypeScript reference is
`packages/coding-agent/src/modes/daemon/daemon-mode.ts:7216-7245`.

A live TUI validation also found stale roster activity after successful child
completion: the footer kept showing two running agents and “Waiting for 2
subagents.” Rust published cached event-time session views, whereas TypeScript
reads the live session when composing the roster. The roster flush now refreshes
activity from the native sessions before composing its payload. Agent start and
end events also trigger publication, with a second refresh after the native run
and action queue settle. `agent_end` is delivered before `Agent::finish_run`
clears the streaming flag; this final refresh also clears the parent's actual busy flags. The top-level
`activity` label separately waits for the summarizer verdict, as in TypeScript;
that semantic classification is not used as proof of actual work in the probe. Existing roster fields and protocol shapes
are unchanged; this is a backward-compatible behavior correction.

## Reproduction and validation

Run on Linux with Node, tmux, a built Rust binary and a private Python environment
containing the existing `prime-agent-runtime` and its default dependencies:

```sh
node scripts/test-native-subagents.mjs /absolute/path/to/optimus-rust /absolute/path/to/private-venv/bin/python
```

The probe uses the actual CLI, supervisor, Rust workers and Python REPL. Only the
model is scripted, through a loopback HTTP endpoint. It clears inherited
environment variables, supplies a literal fixture credential and isolates home,
agent/session data, caches, sockets and the supervisor ownership registry under
`.port-env/tmp/`. A `/proc/<pid>/fd` directory alias keeps Unix socket names short.
Cleanup targets only processes carrying the probe's exact private profile.
Production credentials and saved sessions are not used.

Coverage includes two concurrent named native children, Python host requests,
explicit child-to-parent replies, completion, reopening the persisted parent,
and a real tmux TUI driven by paste and Enter. Terminal captures verify the
always-visible zero-agent footer, running child counts, the transition to two
idle agents, received-message notices and removal of the waiting indicator.
The probe preserves request transcripts, roster frames, CLI output and terminal
captures in its printed evidence directory, including on failure.

The previous installed binary reproduces the startup timeout. Final validation
results and the exact revision are recorded in the PR and slice evidence.

The fix is shared Rust daemon code and applies to Windows, macOS and Linux.
This machine's empirical run is Linux only; Windows and macOS runtime validation
is not claimed. Optimus TUI/daemon behavior is covered. The separate vr-ai-chat
3D and Web surfaces are unaffected; no files in that repository are changed.
