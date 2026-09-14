# Rust integration into main — 14 September 2026

PR #15 is the cumulative Rust port, based on TypeScript reference
`9f547ceaba079dee45ee415a430d57fb86fa940d`. Its head at the start of this integration
was `df5137353346a2d91f8b354d012f3979ce09471c` from
`Goober-AU/prime-agent:fix/rust-cli-runtime`.

## Branch coverage

All of these checkpoints are ancestors of PR #15; none needs a separate merge:

| Branch/checkpoint | Commit |
| --- | --- |
| `rust-port/current-windows-20260911` | `855646113` |
| Telemus `fix/rust-port-completion` / PR #12 | `e322de90e` |
| Goober-AU `fix/rust-port-completion` / PR #13 | `f34aafb5f` |
| Telemus `fix/rust-cli-runtime` / PR #14 | `1819c65ed` |

Unrelated inherited upstream branches are outside this port integration.

## Integration fixes

- Cancelled stalled OpenAI-compatible response-body reads immediately and close the
  HTTP connection while preserving the partial response, matching the SDK signal.
  A loopback regression and a real terminal Escape probe cover this path.
- Serialized OAuth registry tests across the CLI, provider registry, and MCP
  catalog; restored the registry when each test finishes.
- Serialized cleanup tests and unregistered their callbacks on scope exit,
  including assertion failures.
- Corrected cron timezone conversion to interpret Unix timestamps as milliseconds.
  The existing local-clock regression now checks both March and July.
- Corrected a Windows-only drive-prefix assumption in a Linux path test and made
  the trace-persistence test follow the configured private agent directory.
- Moved the CLI test's private fixtures to the system temporary directory so a
  deep checkout does not exceed the Unix worker-socket path limit.
- Aligned npm workspace dependency ranges and the lockfile with version `0.1.0`.
  External dependency versions and TypeScript implementation source are unchanged.
- Excluded private Rust build/test scratch from Biome, including test-created
  dangling symlinks that previously caused the required npm check to fail.
- Restored the TypeScript release packer and `release:pack` command removed in the
  port branch; existing packaging tests, documentation and release tooling still
  reference them. The port does not replace TypeScript distribution.

## Validation

Final results are recorded in `evidence/status/rust-main-readiness.json`.

| Check | Result |
| --- | --- |
| Rust workspace check and test build, locked | Passed; 676 existing warning diagnostics |
| Rust tests (four threads) | 3,088 passed, zero failed/ignored |
| AI parallel stress (eight threads) | 20 runs × 668 tests passed |
| Cron timezone checks | Melbourne, New York and UTC passed |
| Required npm check, npm 11.10.0 | Passed without diagnostics |
| TypeScript release packer and bundled skills | 25 tests passed |
| CLI/TUI and real Python runtime | Passed |

Tests use synthetic providers or a loopback HTTP fixture, private agent/session
roots, and external-network isolation. The Python smoke uses the real default
Rust IPython factory and the unchanged `prime-agent-runtime` implementation.

The Linux build uses Rust/Cargo 1.95.0, `--locked`, disabled incremental compilation,
and debug information disabled for development/test profiles. The workspace's
historical `rust-version = "1.98"` metadata is not inherited by member crates; this
integration does not claim a verified minimum Rust version or silently change it.
The workstation required one Cargo job and temporary storage for dependency build
artifacts because its disk was nearly full.

Runtime smoke coverage includes text and JSON print responses through the actual
supervisor/worker, an 80×24 terminal, a typed prompt and rendered HTTP response,
Escape interruption of a stalled provider response, roster/conversation navigation,
quit and terminal restoration. Python coverage includes initial execution, Unicode output, persistent variables, a local host
request/reply, namespace inspection, and shutdown.

## Remaining scope

This merges a usable development port; it does not replace a running installation,
change the TypeScript launcher, or assert full provider/platform parity.
`REVIEW-NOTE-freeze.md` remains the historical audit inventory. Its two known
registry/cleanup test races are addressed here; its other audit and capability
limitations remain, including JS/TS extension execution, advanced adapter gaps,
AWS credential-chain limits, markdown rendering differences, and unaudited
telemetry/refinement paths. See also `docs/COMPATIBILITY.md`.

Windows-specific source and prior branch evidence are retained. This integration
is validated on Linux; it does not add a fresh Windows or macOS execution claim.
Unix socket directories must stay short enough for the platform's path limit.
The Rust port has no separate 3D or Web dashboard surface in this repository;
these fixes affect its shared runtime, CLI/TUI and development metadata.
