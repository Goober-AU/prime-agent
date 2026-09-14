# PR #20 implementation handoff

This records the implementation added after PR #20 head
`f40da72a818f594a49101fa4ceb2e241a2f7abb1`. It supersedes the outstanding-code
list in the earlier PR description. Earlier test results describe their original
commits; they are not validation of this change.

## Windows supervisor registry

The worker replacement path was using the supervisor ownership registry itself
as a launch lock. It now uses a socket-specific `.supervisor-launch-*.lock` file.
Windows named pipes select the filesystem daemon socket directory for this file;
the pipe name is never treated as a filesystem directory.

Lock reclamation refuses directories and symbolic links. Windows identity checks
use the volume serial number and file index from a pinned handle. Recovery checks
identity again after awaiting the owner-liveness decision, removes only the
matching regular file, and preserves unexpected replacements. Launch cleanup
removes only a file still containing this process's ownership marker. There is no
recursive registry deletion. The existing PowerShell isolation contract remains
in place. No production registry or running daemon was touched during this work.

Legacy directory locks now require owner intervention instead of automatic
reclamation. A concurrent replacement detected after a rename is preserved at
its unique `.stale-*` path if a no-clobber restoration is impossible; it is never
recursively removed.

## Command and runtime implementation

- `/btw` and `/side` use the side-question pane, support follow-up turns, stream
  responses, and cancel the actual side agent. Both in-process and daemon
  adapters now start the real side-question lifecycle with the parent agent's
  provider/auth callbacks. Registration precedes startup so completion and
  cancellation cannot race a placeholder abort handler.
- `/fork` and `/tree` mount their selectors, perform session operations, refresh
  the transcript, and restore editor text. Tree navigation supports labels,
  summary choices, custom summary instructions, and cancellation.
- `/scoped-models` mounts the model selector, updates the live session scope,
  resolves persisted model patterns, and reports persistence errors.
- `/logout` mounts the searchable provider selector and removes the selected
  stored credentials through the shared auth flow, including Prime CLI logout
  when selected. It refreshes the auth/model catalog and affected MCP state.
- `/mcp` keeps its configuration menu and routes subcommands to the existing MCP
  management and auth operations, reloading tools when configuration changes.
- `/share` exports the session and runs the existing GitHub gist workflow with
  cancellation and an explicit result URL.
- `/traces` implements status, preview, login, on/off, current upload, and bulk
  upload through the trace APIs. Credential and settings writes are verified.
- `/update` hands the terminal to the existing updater, coordinates daemon
  restart through its existing API, and relaunches with session resume state.
  The pre-existing native supervisor rejection of `prepare_update_restart`
  remains an explicit warning/fallback path; supervisor transaction parity is
  separate follow-up work, not a newly implemented capability.
- `/debug` records actual TUI render lines and session messages.

The native supervisor now runs a bounded periodic idle sweep. It refreshes
worker state before considering eviction, excludes busy/attached/owned workers
and schedules it cannot wake, fences public commands and worker opens during the
final decision, and uses graceful archive/shutdown. Eligible children of retained
workers use the existing bounded passivation command. Shutdown cancels the sweep.

## Native extension UI and rendering

The in-process interactive connection now binds a native extension UI bridge.
Status, widgets, working indicators, custom headers/footers, raw input listeners,
and local custom component factories reach the owner thread. Custom components
receive input/focus and complete their awaiting callback. Session changes, reload,
and shutdown reset owned surfaces and dispose/cancel overlays and pending replies.
Overlay handles are closed by their owner; hiding an unrelated top overlay is not
used as a substitute for lifecycle cleanup.

Daemon/RPC/headless `custom()` retains the TypeScript no-UI behavior. Native
custom editor/autocomplete-provider APIs still delegate to the existing RPC
bridge; full parity for those APIs is outside this handoff's requested surfaces.

Mermaid fences now use `mermansi = 0.1.6` for Unicode layouts. A source-offset
Markdown transform is wired into assistant messages and preserves unrelated
Markdown bytes. Unsupported, invalid, or overly wide diagrams retain their source
instead of disappearing. All 48 newly resolved registry dependency versions were
checked against crates.io metadata and meet the seven-day release-age rule.

## Existing fixes preserved

All six original handoff defects were already fixed in the starting PR head:
streaming snapshot ordering, post-login catalogs, custom-provider eligibility,
fullscreen arguments, malformed optional history, and session slash-command
routing. Their code was retained. Onboarding, shortcut/hotkey guides, configurable
keybindings, and the shared prompt stash also already had production call sites;
the older contrary audit notes do not describe this starting head.

No daemon wire command or response shape was added. The side-question ID added to
an internal Rust options structure carries the existing wire ID. TypeScript and
Python sources are unchanged. This change targets the Optimus Rust terminal and
its daemon; it does not change the separate VR application's 3D or Web surfaces.

## Checks and deferred validation

Passed on Linux:

- `cargo check -p pi-coding-agent` (existing port warnings remain).
- `node_modules/.bin/tsgo --noEmit`.
- `node_modules/.bin/biome check --error-on-warnings .` (1,072 files; no writes).
- `git diff --check`.

The user explicitly requested **no tests** and subsequently authorized merging
PR #20. No tests, live providers, application smoke runs, installation, or
self-update were executed. The `npm run check` wrapper currently includes
installer and browser smoke suites despite the older AGENTS.md description;
its typecheck and lint steps were run separately. The commit hook was disabled
for this commit solely to honor that no-tests instruction.

The next validation pass must cover:

1. Rebaseline and execute the slash-command matrix. Its previous 32 OK / 1
   DIVERGES / 9 UNHANDLED classification is historical, not a new result.
2. Update the legacy-directory lock expectation to fail closed; validate regular
   stale locks, live directories, symlinks, replacement races, and isolated
   Windows named-pipe recovery. Windows compilation/runtime are unverified here.
3. Drive both the in-process runtime and daemon with fake providers, including
   side questions, cancellation, reconnects, idle eviction, and extension UI.
4. Capture terminal output with a VT emulator for style leakage and the 29
   render cases. Add overlay, terminal handoff, stdin buffering, mouse, terminal
   image, selection metadata, and render-cache fixtures.
5. Run the long-session/soak suites without shared-build contention and exercise
   runtime update/share/auth flows in a disposable configuration.

No fixture coverage or runtime parity result is claimed by this implementation
handoff. Existing formatting debt in large pre-existing Rust modules remains;
new helper modules were formatted without reformatting unrelated modules.
