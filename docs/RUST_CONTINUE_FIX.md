# Continue the last conversation

Fixed `optimus-agent --continue` selecting a recently modified empty session
instead of the last conversation in the current working directory.

The Rust and read-only TypeScript selectors both previously sorted valid session
headers by filesystem modification time without checking for conversation history
(`packages/coding-agent/src/core/session-manager.ts`, `findMostRecentSession` and
`findMostRecentSessionForCwd`). A draft persisted with only settings or daemon
bookkeeping could therefore displace a conversation. Local read-only inspection
found exactly that situation: the newest file had zero message entries, while the
next file contained 78 messages.

Both Rust selectors now examine candidates in the existing modification-time
order and skip files without conversation history. The scan stops at the first
message, custom message, nonempty summary, or valid provider checkpoint. It reads
one JSONL row at a time, stops at the captured file size, ignores malformed rows,
and does not open a session manager or mutate session files. User-only interrupted
turns and histories preserved through compaction remain eligible.

The shared selector covers interactive startup, noninteractive continuation, and
daemon `continueRecent` requests. Explicit `--resume <path>` and the session picker
retain access to empty drafts. With no saved conversation, continuation retains
the existing new-session fallback. Session paths, credentials, and daemon protocol
are unchanged. This is a shared Rust CLI/TUI selection fix; no Web or 3D surface
exists in this application that needs a separate change.

Validation: compile and static checks only; no tests, provider calls, live model
sessions, or production session edits. Live resume remains for user validation.
