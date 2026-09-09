# Selective upstream synchronization: 2026-09-09

This review uses the TelemusAI fork as the authority for existing behavior. It
selectively incorporates official upstream changes; it does not replace the fork
with upstream or identify this source build as an official release.

## Reviewed source identities

- Fork: `telemusai/prime-agent`, starting at
  `3773cb5afa409062067b4cd25411d73c66a263a7`.
- Upstream: `PrimeIntellect-ai/prime-agent`, reviewed through
  `bcdcd6e65e10959c9904ec4467747528303493b0`.
- Common ancestor: `9c8230df67b378aaedc032f90e1ae8ba687cfe4a`.
- Thirteen upstream-only commits were reviewed: ten were incorporated, including
  fork-aware adaptations, and three were intentionally skipped.
- Existing package versions and released changelogs are unchanged. New changes
  are recorded in per-package changelog fragments.

The fork history includes Astra compaction and Fast support (`c971060e`), the
Windows stability follow-ups and 250K policy (`6df9886a`), fork-main update
discovery (`8d036466`), paired Telegram sessions (`015edddc`), and Optimus branding.
Those features are retained, not treated as conflicts to remove.

## Per-commit decisions

Commit links refer to the official upstream repository.

| Commit | Classification | Decision and reason |
| --- | --- | --- |
| [bf8894af](https://github.com/PrimeIntellect-ai/prime-agent/commit/bf8894afa55832f7cfa2094c8a0d041bc680a691): session-relative Markdown links | Safe and useful | Ported session cwd through live assistant output, thinking blocks and transcript replay to Markdown link resolution. Retained the fork's Mermaid handling and interactive integrations. Windows drive paths, spaces and existing URL schemes are covered. |
| [0894de1d](https://github.com/PrimeIntellect-ai/prime-agent/commit/0894de1ded368f175b41baeb16ba3a58ec66f828): committed model catalog during builds | Safe and useful | Removed incidental model-catalog fetching from the AI build command. Explicit `generate-models` remains available. Compilation now uses the committed catalog instead of introducing unrelated model changes or requiring a catalog network request. |
| [0c687325](https://github.com/PrimeIntellect-ai/prime-agent/commit/0c6873255a31ed09fe999d013fd347d134e6bd65): visible inactive sessions | Useful, adapted | Saved sessions are visible by default, including when legacy UI state contains `inactiveExpanded: false`. Unlike upstream, the fork retains Alt+I and deliberate persistent collapse. An optional `inactiveVisibilityExplicit` distinguishes a new explicit choice from an old implicit default. Stable catalog loading and one final publication are unchanged. |
| [4ec05a09](https://github.com/PrimeIntellect-ai/prime-agent/commit/4ec05a0969825a32261ffed43ab5740071a73ae6): Prime Inference GLM 5.3 default | Safe and useful | Updated the default model constant and corresponding login/status fixtures. Existing saved model choices and custom provider routes are not rewritten. |
| [f771dfce](https://github.com/PrimeIntellect-ai/prime-agent/commit/f771dfcedd684d1afff84ca2c6fa95c7a21efbc2): official v0.9.4 release preparation | Not applicable; potentially regressive if copied | Skipped official version/release metadata. This is a selective fork integration, not the official v0.9.4 release. Retained the fork's versions, release history and main-build updater. |
| [363eb619](https://github.com/PrimeIntellect-ai/prime-agent/commit/363eb61920621c53bd4313deb91360fa3d188e5c): agents-view visual hierarchy | Not relevant to this stability integration | Skipped muted metadata and search-chrome styling changes. Kept the fork's current presentation; the saved-session visibility fix is integrated separately. |
| [e6b79144](https://github.com/PrimeIntellect-ai/prime-agent/commit/e6b79144c23e2d73df41cbba5335a8db65ddffe3): contribution ticket gate | Not applicable | Skipped changes tied to the official repository's engineering/research ticket workflow. They do not improve runtime behavior or belong to the fork's release configuration. |
| [71766abb](https://github.com/PrimeIntellect-ai/prime-agent/commit/71766abb2c1e427382871c77d2dc3896164a3456): static system prompt and in-context harness state | Useful, substantially adapted at fork overlaps | Integrated stable system prompts, mechanically delivered memory digests and applied-refinement notices. Preserved native opaque checkpoints, context hooks, strict refinement repair, durable child recovery and legacy SDK/session readability. The native-checkpoint adaptation is described below. |
| [31ebd50c](https://github.com/PrimeIntellect-ai/prime-agent/commit/31ebd50c791d6e016b059c9bc35e9ba4d845672d): OpenCode identity | Safe and useful | Added application/conversation headers only for `opencode` and `opencode-go`. Conversation identity remains independent of cache affinity, and explicit request/model headers retain precedence. Other providers are unchanged. |
| [24519c30](https://github.com/PrimeIntellect-ai/prime-agent/commit/24519c30856965a832551206b3e9ed87e814d602): literal prompt arguments | Safe and useful | Adopted single-pass replacement so dollar sequences inside argument values are not reinterpreted as further template substitutions. Integrated the regression with the new mechanical-digest fixture convention. |
| [a6625e17](https://github.com/PrimeIntellect-ai/prime-agent/commit/a6625e17a042716fca4527c9297b972da3eb2142): orphaned tool results | Safe and useful, with fork regression coverage | Dropped tool results whose matching assistant call did not survive an interrupted turn. Retained complete exchanges, cross-provider ID normalization, checkpoint ownership checks and opaque checkpoint contents. Added checkpoint-specific regression cases. |
| [55ade48b](https://github.com/PrimeIntellect-ai/prime-agent/commit/55ade48b73f636d992855b7cab797d71dc1f6f1c): LiteLLM context errors | Safe and useful | Recognized the specific maximum-context rejection as overflow so the agent compacts and recovers. Existing rate-limit exclusions still take precedence; token-rate-limit errors are not misclassified as context overflow. |
| [bcdcd6e6](https://github.com/PrimeIntellect-ai/prime-agent/commit/bcdcd6e65e10959c9904ec4467747528303493b0): goals after manual compaction | Useful, adapted | Resumed active goals after successful manual compaction using the operation's captured abort signal. Preserved the fork's queued parent input, RLM recovery and scheduler ownership. Cancellation does not spuriously resume a goal. |

These changes were applied as reviewed source hunks rather than an automatic
merge or a whole-file upstream replacement. Overlapping implementations were
resolved by preserving the fork contracts below. No conflict was resolved by
blindly selecting the upstream side.

## Important overlap decisions

### Harness state, refinement and native compaction

`AgentSession` now keeps its system prompt stable across refinements. The latest
persisted harness state is delivered through a mechanical, model-visible digest
on the first committed turn and refreshed at cold session boundaries as needed.
Empty sessions stay empty. Cancelled or cleared first-turn delivery re-arms the
digest without duplicating it. Digest freshness uses timestamps, not display
position after compaction.

Successful refinements deliver a separate notice describing edits that were
actually applied. The fork's durable `refinement_outcome` remains the authoritative
audit record. Zero applied edits do not produce a misleading applied notice.
Strict-first JSON parsing, bounded scalar compatibility, one corrective retry,
schema validation and truthful queued-versus-saved semantics are preserved.

Upstream's summary-text recipe alone is insufficient for the fork's native
compaction: the Responses converter replaces a checkpoint carrier's visible text
with its opaque provider items. The integration therefore replays an unchanged
native checkpoint followed by a **separate user digest**, rather than silently
placing memories in text that the provider converter drops. A regression uses
the actual Responses converter to verify this distinction.

An optional `harnessDigest` is persisted on compaction entries. Mechanical digest
copies are excluded from summarizer input and regenerated outside the summary.
Provider compaction still passes through the fork's context hooks and configured
message conversion. Opaque checkpoint bytes are not edited or presumed readable.

Compatibility is intentional: `createCompactionSummaryMessage` keeps its existing
provider-context argument position, and `buildSystemPrompt` retains the deprecated
optional `harnessState` argument for explicit SDK callers. Normal AgentSession
operation no longer uses that legacy injection. No incompatible daemon wire
change is introduced.

### Checkpoint and child-recovery readability

The integration retains current checkpoint/provider/model ownership validation
and adds read compatibility for the older extension checkpoint shape. It does
not rewrite saved opaque items or enable a second compaction owner.

The legacy RLM reader recognizes the older continuation-state record alongside
the current canonical record. It selects the newest eligible entry on the
current branch, retains reserved attempts and task-result correlation, and uses
recorded recovery messages to distinguish pending work from already-started
work. A malformed newer record does not silently fall back to an older state.
Unprovable execution identity becomes an explicit partial failure or validation
error, never speculative replay. Reopening tests verify one parent report and
no duplicated recovery/model turn.

### Cancellation regression found during integration

An existing fork wait on `_agentEventQueue` could block a new root request behind
a cancelled request whose extension `agent_start` event remained held. The wait
now applies only to spawned children, where durable task correlation requires
it. Root work no longer waits on that child-ledger boundary. The previously
failing cancellation regression and the durable RLM stability cases pass
together. This is an integration-discovered fork fix, not attributed to an
unrelated upstream commit.

### Session visibility without removing controls

The fork retains its last complete catalog while new records load, updates
progress during the scan, and publishes the completed catalog once. Upstream's
visibility change is adapted around that behavior instead of replacing it.
A 234-record test passes the catalog through actual reconciliation and row
building, verifies one final publication and every saved chat's visibility, and
checks that a rebuild does not hide the chats again. Explicit Alt+I collapse,
search reveal and subsequent remount remain supported.

## Fork functionality protected

- Astra model/provider handling, native checkpoint compaction, save/reload/replay,
  compatible context reconstruction, endpoint checks and the existing 20-minute
  compaction allowance.
- Optional Fast behavior without enabling it or changing saved user choices.
- Cached and incremental Codex WebSockets, bounded pre-event reconnect and no
  replay after partially received provider output.
- GitHub Copilot Responses `service_tier` omission and existing behavior for
  other providers. No custom model route, reasoning level, output allowance,
  vision declaration or authentication setting is changed by this integration.
- The inclusive global compaction trigger:
  `contextTokens >= Math.min(250_000, contextWindow - reserveTokens)`.
  Tests retain the 249,999/250,000 boundary and smaller-window reserve precedence;
  full model context windows remain unchanged.
- Three-attempt child recovery, special length-recovery reporting, durable
  continuation through threshold compaction, runtime-owned parent result delivery
  and deduplication, structured exhaustion and observation compatibility.
- Public Windows kernel completion/interrupt protections, live Bash-handle
  snapshot exclusion, captured-launcher guard, narrow directory-fsync handling,
  session persistence, lease recovery and graceful process cleanup.
- Fork-main update discovery, Telegram pairing and session hooks, background Bash
  completion delivery, Mermaid rendering and Optimus branding.

Installation-specific launchers, private model configuration and independent
gateways are outside this public patch. Preserving their deployment behavior is
a separate isolated-build and activation gate, not implied by source tests.

## Changed implementation areas

| Area | Implementation files |
| --- | --- |
| Deterministic AI build | `packages/ai/package.json` |
| Provider identity | `packages/ai/src/providers/opencode-headers.ts`, `anthropic.ts`, `google.ts`, `openai-completions.ts`, `openai-responses.ts` |
| Tool results and overflow | `packages/ai/src/providers/transform-messages.ts`, `packages/ai/src/utils/overflow.ts` |
| Harness/refinement/session lifecycle | `packages/coding-agent/src/core/agent-session.ts`, `messages.ts`, `system-prompt.ts`, `session-manager.ts`, `refinement/refinement.ts`, `modes/headless-completion.ts` |
| Compaction/read compatibility | `packages/coding-agent/src/core/compaction/compaction.ts`, `branch-summarization.ts`, `checkpoint.ts`, `packages/coding-agent/src/core/legacy-rlm-continuation.ts` |
| Model defaults and prompt arguments | `packages/coding-agent/src/core/model-resolver.ts`, `prompt-templates.ts` |
| Session links and visibility | `packages/coding-agent/src/modes/interactive/components/assistant-message.ts`, `conversation-components.ts`, `packages/coding-agent/src/modes/interactive/interactive-mode.ts`, `packages/coding-agent/src/modes/agents-view/agents-view-mode.ts`, `packages/tui/src/components/markdown.ts` |

The associated tests include upstream regressions, checkpoint/digest conversion,
legacy replay readers, catalog visibility and mechanical-digest fixture
migrations. Fixture updates exclude only the mechanical digest where a test is
asserting conversation content; raw persisted counts and event-order assertions
continue to include it. Existing child protocol requirements are not weakened
to satisfy old fake-provider fixtures.

## Validation recorded for this review

Tests used isolated configuration directories and fake/mock providers; the
OpenCode header tests used a loopback fixture server. These results do not claim
live model availability or measured external latency improvements.

| Focused batch | Result |
| --- | --- |
| AI providers, interrupted exchanges, overflow, native compaction, Copilot and Codex transport | 169 passed across seven files |
| Markdown links, agent-message UI, session visibility/catalog and LiteLLM agent recovery | 102 passed across five files |
| Existing TUI Markdown and LaTeX suites | 95 passed using the package's Node test runner |
| Extension binding, interactive status, queue, print mode, model extension, runtime and retry event fixtures | 384 passed across seven executed files |
| Harness, refinement, goal/compaction, native digest/checkpoint, legacy RLM reload and child recovery | 425 passed across fifteen files; two existing skipped tests |
| Recursive-agent lifecycle | 124 fake-provider cases passed; the separately selected real candidate-kernel asyncio/RLM case also passed |
| Model defaults, literal templates, compatibility readers, action races and existing regressions | 267 passed across fifteen files (some compatibility-reader cases overlap the harness batch) |
| Repository validation | `npm run check` passed: Biome, TypeScript, installer checks and browser smoke checks |

Representative commands, run from the named package directories with a fresh
test HOME/configuration and production runtime overlays disabled:

```text
# packages/ai
npx tsx ../../node_modules/vitest/dist/cli.js --run test/opencode-headers.test.ts test/transform-messages-interrupted-tools.test.ts test/overflow.test.ts test/transform-messages-copilot-openai-to-anthropic.test.ts test/openai-compaction.test.ts test/openai-responses-copilot-provider.test.ts test/codex-transport-reliability.test.ts --maxWorkers=2

# packages/coding-agent
npx tsx ../../node_modules/vitest/dist/cli.js --run test/suite/regressions/2108-assistant-message-links.test.ts test/suite/regressions/4531-agent-message-ui.test.ts test/agents-view-mode.test.ts test/agents-view-catalog-stability.test.ts test/suite/regressions/6011-litellm-context-overflow.test.ts --maxWorkers=2
npx tsx ../../node_modules/vitest/dist/cli.js --run test/suite/harness-native-compaction.test.ts test/suite/legacy-rlm-session-reload.test.ts test/legacy-rlm-continuation.test.ts test/suite/rlm-continuation-stability.test.ts --maxWorkers=2

# packages/tui
node --test --import tsx --test-concurrency=2 test/markdown.test.ts test/markdown-latex.test.ts
```

Initial failures and their corrections were retained in the review evidence:
old assertions needed the new digest/event ordering; a Windows display fixture
required native path separators in its isolated HOME; and the root cancellation
deadlock above required a scoped runtime fix. TUI Markdown tests use Node's test
runner, not the package's narrowly configured Vitest suite.

Deduplicating final test identities across these overlapping batches gives
**1,448 passing Vitest cases plus 95 passing Node/TUI cases: 1,543 distinct
passing tests**, with two existing credential-dependent compaction cases skipped.
Three additional isolated source-characterization cases verified the daemon's
explicit-resume contract after failed manual compaction.

## Standalone Windows acceptance

The integrated source was compiled into a separate, version-identified native
Windows runtime and fresh managed Python environment. All twelve final packaged
acceptance suites passed. These tests used fake providers and isolated homes,
session directories, named pipes and ownership registries; the running
installation was not stopped or changed.

| Packaged suite | Result |
| --- | --- |
| Model/checkpoint/250K invariants | 78 assertions; custom model definitions retained and legacy checkpoint replay validated |
| Atomic persistence and background Bash | 11 scenarios; narrow directory-fsync tolerance and one wakeup without duplicate awaited-handle delivery |
| PowerShell, CMD and Git Bash launchers | All three selected the exact candidate, kernel and native temporary directory |
| Compaction retry/cancellation | Transient, empty, permanent, exhausted and aborted requests; stable per-request retry identity and successful UI-shaped input after failure |
| Real kernel | 100 cells, long execution, interrupt/reuse, delayed events, snapshots, restart and nonresponsive Wait/Kill |
| Bundled kernel | 100 cells, one-shot interruption, passive reuse barriers and exactly-once bundle validation |
| Live-handle snapshots | 100 cells; background handles excluded without losing subsequent execution |
| Production-sized namespace | Three valid snapshots of 331 names / 69,592,859 bytes; no interrupt storm and immediate following commands succeeded |
| Saved-session catalog | All 234 synthetic main chats, guarded catalog role/IPC checks and actual bundled attach/detach/reopen |
| Concurrent daemon lifecycle | No-worker and concurrent-worker cases, archive/reopen, graceful and force shutdown, no shutdown-as-user-message |
| RLM | Length/ordinary/terminal threshold paths, reused tasks, bounded recovery/exhaustion, automatic results and explicit-reply deduplication |
| Refinement | Two durable successes and two deliberate durable failures, applied notices only, stable system prefix and no external network attempts |

Process-owning suites verified exact shutdown and zero surviving test processes.
The fresh environment imported all 25 required skill/runtime/extra modules.
The package retained source-based Windows fixes plus two scoped, hash-guarded
installation-only observation adapters. Those adapters and private model routes
are not introduced into the public daemon wire protocol by this PR.

Acceptance failures were preserved, diagnosed and corrected in new test-only
revisions rather than edited away. They included an obsolete converter import,
checking version output only on stdout, a non-native `cmd.exe` executable path,
and a bare daemon prompt that deliberately did not resume suspended input.
The latter was not a new runtime regression: the real UI sends explicit resume
options. Both rejection and successful explicit resumption are now asserted.
No runtime safeguard was weakened to satisfy those tests.

## Remaining boundaries

- This ledger records selective source integration and focused tests, not a
  released package or permission to change a running installation.
- The standalone gates above passed against the separately identified build.
  Final production activation still needs a fresh preservation backup, a scoped
  identity recheck and independent-launch verification at the authorized
  cutover boundary. Source or standalone tests do not authorize that cutover.
- The Unix-oriented `daemon-supervisor-process.test.ts` fixture remains unchanged
  from the fork and unexecuted here. Its filesystem sockets fail with `EACCES` on
  this Windows host; it also uses POSIX process groups and SIGSTOP. Its digest
  count assertions still need a Linux fixture follow-up. It is not included in
  the passing modified-test inventory or claimed as Windows lifecycle coverage.
- The batch results above are not a claim that every test in the repository was
  run. The real-kernel recursion test
  verifies asynchronous RLM calls after a scheduling cell becomes idle, not the
  complete standalone bundled lifecycle.
- Provider cacheability should improve with a stable system prefix, but live
  Astra speed and remote compaction latency remain unmeasured here.
- Uncorrelatable legacy recovery records deliberately fail closed. They require
  explicit diagnosis rather than automatic replay of possibly completed work.
- A production cutover still requires the separately approved, preserved-state
  activation procedure. No private gateway, credential, session or VM path is
  part of this public review.
