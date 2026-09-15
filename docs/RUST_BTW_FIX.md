# Rust side-conversation pane and worker stack fix

The Rust host created a `/btw` component as an inline TUI child, but fullscreen
rendered only the transcript and editor dock. The pane was therefore invisible
in fullscreen even while a side question ran. It now belongs to the transcript's
prompt context, so both layouts render the same pane once, with the configured
editor padding and immediate repaint on open, streaming updates and dismissal.

The TypeScript reference is
`packages/coding-agent/src/modes/interactive/interactive-mode.ts`:

- `handleSideQuestion` / `handleSideQuestionEvent` (4603–4664): streaming and
  completed turns, fresh parent context and answered side turns for follow-ups.
- `setupEditorSubmitHandler` (4793–4816, 5050–5144): recognized slash commands
  produce an in-pane notice; absolute paths remain side replies; blocked drafts
  are restored; images receive the text-only notice. Main queue navigation is
  bypassed while the pane owns the editor.
- `finishSideQuestionBash` / `clearSideQuestion` and bash event handlers
  (4665–4735, 5550–5628): `!` / `!!` execute transient shell commands in the
  pane. Only `!` output seeds side follow-ups. Neither run enters main-session
  context. Run IDs keep foreign and closed-pane output out of the main chat;
  cancellation waits for ownership of the session's bash slot.
- `getPromptContextContainers` / `applyFullscreen` (7494–7522): the pane is
  scrollable content above the prompt, not a modal overlay.

## Worker closure

The installed `c3f591e6e` worker log at 2026-09-15 10:58 local time records
`tokio-rt-worker` stack overflow and process abort. This explains the subsequent
`Daemon worker socket closed` message. The worker already used 16 MiB stacks.
No core dump was available to identify the exact crash stack.

Code inspection found unbounded recursive polling in `push_agent_event_task`:
each `Shared` future contained and awaited its predecessor's work. A backlog
could poll the whole chain on one native stack. `schedule_session_input_pump`
used the same pattern. This differs from JavaScript Promise continuations
(`agent-session.ts:4075–4079`), which schedule subsequent work without recursively
polling earlier operations. Compiled worker inspection found individual async
poll frames up to hundreds of KiB, increasing the impact of a deep chain.

Both queues now publish shared completion receivers and run the work in separate
Tokio tasks. Waiting on a tail never polls predecessor work. Tail replacement
stays atomic under the existing mutex; ordering, snapshot barriers and continuation
after a failed predecessor are preserved. The implementation removes the verified
stack-growth defect; attributing the exact recorded crash remains an inference
until live validation. Worker stack size was not increased again.

## Validation and scope

No tests or real provider calls were run, per the user's instruction. Build and
static checks are recorded in `evidence/status/rust-btw-pane.json`. Live fullscreen
rendering, side follow-ups, shell cancellation, and long streaming sessions remain
for user validation. No daemon protocol, Python runtime source, or TypeScript
reference changes were required. The native changes are shared across operating
systems; the candidate was compiled on Linux. This repository has no 3D surface
involved in this fix; Web behavior is unchanged, and the Rust TUI follows the
TypeScript terminal behavior above.
