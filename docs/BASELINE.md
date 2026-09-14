# BASELINE - pinned reference for the optimus-rust port

This file pins what "parity" means for this port. Nothing here may be changed
by silently fetching a newer main.

## Pinned source

| Item | Value |
|---|---|
| Fork remote | https://github.com/telemusai/optimus-agent.git |
| Source commit | 9f547ceaba079dee45ee415a430d57fb86fa940d |
| Build ID | optimus-main-20260911-9f547cea |
| Displayed version | 0.9.3 |
| Port branch | rust-port/current-windows-20260911 |
| Reference checkout (read-only) | C:/Users/openclawuser/optimus-main-20260911 |
| Accepted runtime root (read-only) | C:/Users/openclawuser/.prime/agent/update-staging/optimus-main-20260911-9f547cea-v1 |
| Accepted package (read-only) | .../runtime/packages/coding-agent |
| Built package name | @earendil-works/pi-coding-agent |
| Built CLI | dist/bundle/cli.js |
| User-facing command | prime-agent (NOT replaced by this port) |

## Local working-tree differences in the reference checkout (read-only)

Two uncommitted test changes exist in the reference checkout as inspected on
2026-09-11:

- packages/coding-agent/test/daemon-supervisor-ownership.test.ts
- packages/coding-agent/test/rlm-ledger.test.ts

They are NOT part of the pinned commit and are NOT copied into this clone. If a
test correction is needed here it is reproduced in this clone only, with
provenance recorded in docs/COMPATIBILITY.md.

## Compatibility references (read-only)

- C:/Users/openclawuser/.prime/agent/windows-patches/compatibility-manifest.json
- C:/Users/openclawuser/.prime/agent/windows-patches/PRIME-WINDOWS-COMPATIBILITY.md
- C:/Users/openclawuser/.prime/agent/windows-patches/optimus-main-20260911/activation-v3/CURRENT.json
- C:/Users/openclawuser/.prime/agent/windows-patches/optimus-main-20260911/activation-v3/COMPLETION.md
- C:/Users/openclawuser/.prime/agent/windows-patches/optimus-main-20260911/candidate-v3/fork-sync-guard.cjs
- C:/Users/openclawuser/.prime/agent/windows-patches/optimus-main-20260911/candidate-v3/built-inputs.json

## Acceptance / build evidence (read-only)

C:/Users/openclawuser/optimus-update-20260911 (HANDOFF.md, acceptance-revision.json,
kernel-constraints.txt, kernel.json).

## Definition of parity

Committed fork source PLUS the accepted Windows behaviour and configuration
contracts. Missing runtime/launcher behaviour is compared deliberately; historical
bundle patches are not stacked blindly.

## Python boundary

- Transport: newline-delimited JSON, protocol version 3, `python -m rlm.repl`.
- Interpreter selection: PRIME_AGENT_KERNEL_PYTHON, PRIME_AGENT_KERNEL_VENV.
- The port uses its OWN private venv built from the same pinned Python
  implementation, skill package code and dependency versions.
- The production venv is read-only reference only. No installs, edits, bytecode
  generation or tests run there.

## Isolation contract for test child processes

Every candidate or reference child process runs with explicit private agent
directory, session/artifact/memory/harness roots, TEMP/TMP inside this project,
private daemon pipe and kernel bridge, and its own kernel Python. The running
Prime host is never reconfigured, restarted or attached to.
