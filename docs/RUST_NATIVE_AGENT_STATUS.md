# Persistent native agent status

The interactive Rust host now always displays its agent status tray beneath the
editor, in both normal terminal mode and the fullscreen dock. The title gives the
total (`4 agents`), followed by running, idle, and inactive counts. An empty
session displays `0 agents` and zero counts instead of hiding the tray. Selection
and the scoped agents view remain available when there are children to open.

The TypeScript reference mounts `SubagentSummaryLine` after the editor in
`packages/coding-agent/src/modes/interactive/interactive-mode.ts`; its component
deliberately hides the box at zero children. Always-visible native chat is an
intentional change requested by the user. Direct-child filtering, queued/running
classification, cancelled-child exclusion, authoritative public roster selection,
snapshot fallback, and configured keyboard controls retain the reference behavior.

The reported session had no registered native children. Read-only inspection of
its public roster and `get_rlm_children` response confirmed that. Its history
showed an old `runRlmChild is not implemented` failure, then four external
`codex exec` processes started through Bash. On resume it continued that fallback.
Those processes are not native subagents and are not counted as such. The missing
native implementation was already replaced by PR #28 in the installed build.

The delegation guidance now directs agents to native `rlm(...)` children unless
the user requests an external agent. It tells resumed sessions to recover native
handles, use the current native interface, report current failures, and account
for existing external work before creating overlapping tasks. This is model
guidance, not a shell-command prohibition or an automatic conversion of running
external processes. Existing worker processes and transcripts are preserved.

This changes the Rust TUI presentation and the shared delegation prompt; daemon
protocol and Python API shapes are unchanged. The separate VR project's 3D/Web
surfaces are unaffected. TypeScript is retained as a read-only reference.

Validation is limited to build, static checks, and read-only inspection. No tests,
provider calls, native child launches, or live terminal rendering checks were run;
the user requested that testing remain deferred.
