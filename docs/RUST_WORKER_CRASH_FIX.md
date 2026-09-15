# Rust worker crash and installed Python runtime

The reported worker `181a217a9cac` aborted with a Tokio worker-thread stack
overflow. The saved session separately records an `ipython` bootstrap failure:
startup attempted to install `prime-agent-runtime` from the registry instead of
using the bundled source. The unsuccessful bootstrap recreated the release's
Python environment, leaving only its seed packages. The model's assessment was
therefore not based on a successful repository inspection.

Changes:

- Allocate 16 MiB stacks for the four native Tokio runtime workers. The installed
  development binary has large synchronous poll frames (roughly 228 KiB for
  extension event dispatch and 112 KiB for session event processing), which nest
  with other futures. The prior runtime used Tokio's 2 MiB default. This increases
  the stack budget without changing event order or daemon behavior. No crash dump
  was available, so the exact overflowing call chain remains unverified.
- Resolve the Python source in the native release layout, both relative to
  `packages/coding-agent` and the executable's `bin` directory. The existing
  TypeScript distribution candidate remains first.
- The installed launcher selects its bundled interpreter using
  `PRIME_AGENT_KERNEL_PYTHON`. The existing runtime readiness checks still run,
  but auto-bootstrap cannot delete this prepared environment merely because it
  has no bootstrap-version marker. Explicit Python or venv overrides retain
  their existing meaning.
- Commit the existing per-release daemon socket isolation in the launcher.
- Clear activity flags and loaders on terminal connection closure and the
  supervisor's confirmed worker-exit error from a session/revision-guarded
  state refresh. Late command failures cannot clear a newer session's activity.
  Transient socket loss alone does not falsely complete a running turn. Failed
  prompts are not replayed.

The previous stale-Waiting fix from PR #21 is included through the main branch.
Renamed the repository source launcher to `optimus-agent.sh`, including its
launcher-path export and documentation references. Its TypeScript execution
behavior is unchanged. Documentation under `packages/` is updated solely for the
user-requested rename; the TypeScript implementation remains unchanged.

No daemon protocol, Python source, provider implementation, or user configuration
is changed. These changes apply to the native runtime and terminal UI; the shell
launcher change applies to Linux installations.

Validation is recorded in `evidence/status/rust-worker-crash.json`. Tests remain
deferred under the user's no-tests instruction. Compilation and installation
readiness checks do not establish that the original live turn can now complete;
the next user run must confirm that behavior.
