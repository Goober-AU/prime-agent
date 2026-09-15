# Rust session activity fix

The native TUI could retain `Waiting` after a turn completed. A command completion
read the entire connection state on the UI owner loop and installed that response
without a generation or session-event guard. A late response could restore an
older streaming flag after `agent_end`. A slow response also delayed event
handling and repainting. Separately, native session events never reached the
existing activity tracker, leaving its label at the default `Waiting` throughout
a response.

The host now:

- Feeds local and daemon message/tool events into the existing activity tracker,
  including thinking, writing, tool execution, and token progress.
- Runs post-command and post-turn state reads in a background task with a
  30-second bound, replacing the previous pending task when superseded.
- Rejects results from older requests, older event revisions, or another session.
- Keeps a completed lifecycle flag clear when a metadata response still reports
  the run as active. An otherwise-current idle response may clear activity.
- Preserves current activity flags across model-selection replies, so selecting a
  model cannot stop a newer turn's indicator or restart a completed one.
- Repaints on accepted refreshes and cancels the pending task on host teardown.

A message ending is not treated as the whole agent finishing: subsequent tools,
retries, and assistant turns continue to own their normal lifecycle. Initial
attachment and resync snapshots remain authoritative and can restore an active
session. No daemon wire format or runtime lifecycle semantics changed.

The TypeScript reference already feeds its activity tracker in
`interactive-mode.ts:5501`; its post-turn stats refresh runs asynchronously at
`:5796` and uses generation/session guards at `:2757-2774`.

Validation: `cargo check --locked -p pi-coding-agent`, TypeScript `tsgo --noEmit`,
Biome, and `git diff --check` passed. Existing Rust port warnings remain. Test
suites are deferred under the current no-tests instruction. The report's UI was
still responsive; daemon diagnostics reported `isStreaming: false` and
`isSessionActive: false`. A direct connection-state read timed out. These checks
do not capture the original client's exact event ordering or prove the rendered
fix; manual validation follows installation.

The installed build is unchanged. After installing the merged fix, check a plain
answer, a tool-using answer, interruption, and a second prompt: the indicator
should follow active phases, disappear on completion, and start again for the
next turn. Also check that a model change or delayed state response does not
restore the completed turn's indicator.
