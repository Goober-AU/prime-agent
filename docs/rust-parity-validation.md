# Rust parity repair validation

This change integrates the reviewed Rust parity repairs and the preceding Windows
standalone repairs. It does not replace the TypeScript application, migrate a live
profile, change credentials, or activate an installation as part of a build.

## Protected behavior

- Existing session files and JSON-backed memory remain authoritative.
- Compaction keeps the 250,000-token trigger and the configured full model window.
- Native RLM children run inside the Rust daemon/Python integration, not through
  Codex CLI or another installed agent.
- Existing provider routes and reasoning choices remain available.
- Telegram text is steering input; unpaired senders cannot submit prompts.
- Background helpers are distinguished from active model work in status displays.
- Windows saved-chat scans cache unchanged files using volume/file-index identity;
  real atomic replacements invalidate that cache even at equal size and mtime.

## Validation setup

Use a disposable profile, private daemon socket and separate build target directory.
Never run these acceptance suites against a real user's profile. Run serially:
several suites intentionally exercise process-global environment variables.

Set `PRIME_AGENT_CODING_AGENT_DIR`, session/artifact directories, memory/harness
directories and the supervisor registry to paths inside that disposable profile.
Set `PI_PACKAGE_DIR` and `PI_CODING_AGENT_MODULE_DIR` to this checkout's
`packages/coding-agent`. A real-kernel smoke test needs an isolated Python venv with
the bundled Python skills installed. Provider tests use fake transports or a
loopback scripted model; they require no paid provider credentials.

The Telegram and memory suites require explicitly selected scratch directories:
`PARITY_TELEGRAM_STATE_ROOT` and `PARITY_MEMORY_STATE_ROOT`. Their fixtures clean
their own named case directories: never point these variables at valuable data.
The transcript suites accept `OPTIMUS_PARITY_STATE_ROOT` and the daemon suites
accept `PARITY_DAEMON_STATE_ROOT`; their default is a private temporary directory.

On Windows, compile the bootstrap stand-ins with
`crates/pi-coding-agent/tests/bootstrap_support/build-fixtures.cmd <scratch-tools>`
from an x64 Visual Studio developer shell, then set
`PARITY_BOOTSTRAP_TOOLS_DIR=<scratch-tools>`. These are tiny fake executables, not a
downloaded Python toolchain. The bootstrap/process-cleanup suites are Windows-only.

## Focused commands

All Cargo commands use `--locked -j 2` and test arguments `-- --test-threads=1`.

- `cargo test -p pi-coding-agent --test bootstrap_t01 --test bootstrap_t02`
- `cargo test -p pi-coding-agent --test compaction_parity_suite`
- `cargo test -p pi-coding-agent --test memory_scope_parity`
- `cargo test -p pi-coding-agent --test telegram_parity --test telegram_steering`
- `cargo test -p pi-coding-agent --lib telegram`
- `cargo test -p pi-coding-agent --test transcript_t06 --test transcript_t07`
- `cargo test -p pi-coding-agent --lib parity_tests`
- `cargo test -p pi-ai --lib t15_`
- `cargo test -p pi-coding-agent --lib t17_`
- `cargo test -p pi-agent-core --test rlm_t10`
- `cargo test -p pi-coding-agent --lib rlm_session_t10_tests`
- `cargo test -p pi-coding-agent --lib agent_observe_parity_tests`
- `cargo test -p pi-coding-agent --test rlm-daemon_t10`
- `cargo test -p pi-coding-agent --lib heartbeat_id_space_tests`
- `cargo test -p pi-coding-agent --lib t11_`
- `cargo test -p pi-coding-agent --lib t14_`
- `cargo test -p pi-coding-agent --lib g2_11`
- `cargo test -p pi-coding-agent --lib core::tools::ipython::tests`
- `cargo test -p pi-coding-agent --test sessionkernel_t04_process_cleanup`
- `cargo test -p pi-coding-agent --test standalone_compatibility --test windows_standalone_input`
- `cargo test -p pi-coding-agent --lib command_envelopes_preserve_public_request_and_client_identity`

Kernel ownership also has focused filters `session_and_tool_share_one_provisioner`,
`kernel_settings_and_environment_propagate`, `reload_waits_for_old_snapshot_writer`,
`resume_and_compact_kernel_notices`, `acp_mcp_reaches_owned_kernel`, and
`prewarm_dispose_and_export_contract` in the `pi-coding-agent` library tests.

Run `npm run check` separately for the retained TypeScript tree and installer checks.
On Windows its shell checks need Git's `usr/bin` and `bin` on PATH.

## Review boundaries and remaining work

Passing these focused checks is not a claim that every item in the static audit is
fixed or that the complete repository test suite passes. Existing Rust compiler
warnings remain outside this repair's scope. Real-provider/network latency and
live Telegram delivery are not measured by fake-transport tests.

A held-directory bootstrap cleanup test timed out once while terminal acceptance
was running concurrently. Its unchanged isolated rerun and full serial bootstrap
suite passed. The load-sensitive timeout remains recorded, not claimed fixed.
The transcript replacement oracle was corrected to use a real atomic rename;
rewriting the same file while preserving size and mtime is not that operation.
A separate deterministic Windows test denies data reads to prove unchanged
catalog entries reuse their cache rather than repeatedly scanning full history.

The cross-worker messaging gap recorded as C-03/D-12 remains for follow-up. Other
unassigned or unproven audit cases require their own evidence; the extension abort
seam (H-13) and subagent-summary focus binding are not newly implemented here.
The runtime performance-recorder adapter is also not claimed complete merely
because `/monitor` and recorder unit tests pass.

Daemon protocol changes repair existing command/envelope behavior rather than
introduce a required startup command: `get_session_tree` remains version-gated;
the negotiated envelope, not an untrusted body field, determines compatibility.
Requested session directories select their corresponding spawn ledger.

Local installation validation must additionally exercise the built executable:
saved-chat enumeration/open/save, real Python and native RLM children, terminal
menus and cursor input, isolated shutdown, and independent process ancestry.
Keep private profiles, transcripts, tokens, runtime receipts and machine-specific
launchers out of the public PR.
