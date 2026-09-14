# Rust port: reconnaissance inventory (2026-09-11)

Scope: first-party TS/JS/CJS/MJS in packages/agent, packages/ai,
packages/coding-agent, packages/tui at pinned commit
9f547ceaba079dee45ee415a430d57fb86fa940d (build optimus-main-20260911-9f547cea).

## Size

| Package | TS files | in src/ | test files | JS/CJS/MJS | total TS LOC | src LOC |
|---|---|---|---|---|---|---|
| packages/agent | 13 | 6 | 6 | 0 | 6,522 | 2,841 |
| packages/ai | 153 | 59 | 89 | 1 | 59,886 | 37,665 |
| packages/coding-agent | 845 | 315 | 433 | 8 | 313,692 | 136,735 |
| packages/tui | 71 | 32 | 38 | 0 | 28,680 | 14,691 |
| total | 1,082 | 412 | 566 | 9 | 408,780 | 191,932 |

src LOC excludes tests. `packages/ai/src/models.generated.ts` is 22,884 of the
37,665 ai LOC and is generated data, not hand-written logic.

## First-party JS/CJS/MJS (hidden runtime helpers)

- packages/coding-agent/postinstall.cjs (install-time bootstrap)
- packages/coding-agent/scripts/bundle.mjs, scripts/summarize-performance-metrics.mjs
- packages/coding-agent/src/core/export-html/template.js (+ vendor/highlight.min.js,
  vendor/marked.min.js) - embedded browser assets
- packages/coding-agent/examples/extensions/doom-overlay/doom/build/doom.js - example asset
- packages/ai/src/cli.js equivalent entry points

## Largest coding-agent subsystems (src LOC)

| Subsystem | LOC | Files |
|---|---|---|
| modes/interactive | 25,566 | 73 |
| modes/daemon | 23,914 | 30 |
| core/agent-session.ts | 13,194 | 1 |
| modes/agents-view | 4,432 | 4 |
| modes/agent-connection | 4,284 | 6 |
| core/extensions | 4,274 | 9 |
| core/tools | 3,691 | 14 |
| core/kernel | 3,398 | 7 |
| core/session-manager.ts | 2,761 | 1 |
| core/package-manager.ts | 2,443 | 1 |
| modes/rpc | 1,855 | 5 |
| core/model-registry.ts | 1,803 | 1 |
| main.ts | 1,780 | 1 |
| core/cron-jobs.ts | 1,719 | 1 |
| modes/acp | 1,692 | 6 |
| cli/daemon-command.ts | 1,688 | 1 |
| core/compaction | 1,568 | 5 |
| core/memory | 1,400 | 8 |
| modes/telegram | 1,332 | 6 |
| core/refinement | 1,293 | 2 |

## npm runtime dependencies that need Rust counterparts

agent: typebox
ai: @anthropic-ai/sdk, @aws-sdk/client-bedrock-runtime, @aws-sdk/credential-provider-node,
  @google/genai, @mistralai/mistralai, @smithy/core, @smithy/signature-v4, chalk,
  openai, partial-json, proxy-agent, typebox, undici, zod-to-json-schema
coding-agent: @agentclientprotocol/sdk, @silvia-odwyer/photon-node, chalk, cli-highlight,
  diff, extract-zip, file-type, glob, grok-mermaid, hosted-git-info, ignore, jiti,
  marked, minimatch, proper-lockfile, strip-ansi, typebox, undici, uuid, yaml
tui: chalk, get-east-asian-width, marked, mime-types

`jiti` and the extension host are the known residual-runtime conflict: unchanged
JS/TS extensions cannot run inside a pure-Rust host without a JS engine or Node
sidecar. Reported, not silently resolved.

## Python boundary (must stay Python)

- transport: newline-delimited JSON over the child's stdin/stdout, protocol
  version 3, started as `python -m rlm.repl` (prime-agent-runtime/src/rlm/repl.md)
- requests: execute, interrupt, host_reply, snapshot, restore,
  snapshot_export_legacy, list_names, shutdown
- events: ready, stdout, stderr, result, display, host_request, error, done
- interpreter selection: PRIME_AGENT_KERNEL_PYTHON, PRIME_AGENT_KERNEL_VENV
- 13 Python-backed skill package roots, 26 successful import checks in the
  accepted build (kernel-constraints.txt / kernel.json)
