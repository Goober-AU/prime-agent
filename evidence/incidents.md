# Incidents and corrections during the port

Chronological, factual, with what was changed. Kept because these are the real
risks a reviewer should know about.

## 1. Worker output-limit deaths (2026-09-11)

Several early workers were killed by their own output limit after writing part of a
slice, leaving stubs and no build evidence. Slices affected: agent-core,
ca-session-core, ca-kernel, ca-root, ca-settings, ca-interactive-b, ai-mcp.

Correction: slices were split (agent-core -> a/b, ca-settings -> a/b,
ca-interactive-b -> 4 component slices, ca-kernel -> ca-kernel-b) and every later
brief mandates chunked writes of ~150-250 lines, no echoing file contents, and a
final message of 10 lines or fewer.

## 2. Provider concurrency limit (429)

At 25 concurrent sub-agents the model provider returned
`429 too many concurrent requests`. The cap counts every depth, so the limit is
20 total agents including grandchildren.

Correction: at most 18 depth-1 children run at once; sub-agents are forbidden to
spawn their own children. Recorded in docs/PORT-RULES.md and
evidence/subagent-cap.txt.

## 3. Cross-slice file pollution

The agents-view worker appended helpers to `crates/pi-coding-agent/src/utils/paths.rs`,
which belongs to the ca-utils slice, and invented three functions
(`resolve_absolute`, `normalize_absolute`, `basename`) that do not exist anywhere in
the TypeScript source.

Correction: ownership rules added to docs/PORT-RULES.md ("write only your own mapped
files", "never invent functions that do not exist in the TypeScript"), broadcast to
all workers. The agents-view worker was ordered to remove its helpers from paths.rs
and to port `reconcileUnifiedSessions` into its own module.

## 4. Corrupted Cargo manifest

A worker pasted 547 lines of Rust function bodies into
`crates/pi-coding-agent/Cargo.toml`. Cargo then failed to load the workspace member,
so no crate could be checked.

Correction: manifest restored; the misplaced functions were recovered into their real
module, `crates/pi-coding-agent/src/core/messages.rs` (17 functions, no duplicates).
All workers were told that a Cargo.toml holds only TOML and that they may edit only
their own crate's manifest.

## 5. Wrong "offline registry" report

One worker reported that the crates.io registry was offline and that reqwest, url,
sha2 and base64 were unavailable, and proposed local stand-ins.

Correction: verified directly - `cargo add reqwest && cargo fetch && cargo build`
succeeded in a scratch crate. The registry is online; every crate now declares the
full workspace dependency set, and stand-ins were rejected.

## 2026-09-12 - lead ran `git stash` with 14 live writers (SELF-INFLICTED)

While comparing whether my own `core/extensions/mod.rs` fix had caused 5 errors, I ran
`cargo check` from a stashed tree (`git stash` / `git stash pop`). `git stash` is a
WHOLE-TREE operation: it reverted uncommitted work belonging to every concurrently
writing sub-agent, not only the file under test. `git stash pop` then aborted with a
merge conflict on Cargo.lock, core/agent_session.rs and core/sdk.rs.

Reverted: daemon_mode.rs (740 B of newer work), scoped_models_selector.rs (3 B),
telegram/bridge.rs (1 B), evidence/status/ca-interactive-components-1.json (881 B),
and Cargo.toml (lost the `v7` uuid feature workers need).

Recovered by extracting all 10 files with `git show stash@{0}:<path>` into
`.port-env/stash-restore/` while workers kept writing, then restoring only the copies
whose live version had NOT already advanced past the stash. Live-newer files were left
untouched. Stash dropped only after every file was verified present on disk.
A secondary error: I set `GIT_DIR=''` in the shell env, which broke the extraction
redirects (0-byte files) and made git report "not a git repository"; fixed and re-run.

Rules added: never run git stash/reset/checkout while sub-agents are live; to compare a
change against its base, copy the file to a temp path; confirm zero live writers before
any tree-level git operation.
