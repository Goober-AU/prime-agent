# Python tool wait after interruption

The latest session stopped recording output after a Python call at 02:24:08 UTC on 2026-09-15. Its preceding tool was interrupted, and the user then asked for direct file searches. The pending call only enumerated and read local headers. Read-only daemon state still reported `isStreaming: true`, `isRunningTools: true`, and a ready worker. The Python process remained alive with no descendant shell/search process. This distinguishes the incident from the earlier supervisor mutex deadlock.

The Rust agent loop raced the tool's execution future directly against cancellation. When cancellation won, Tokio dropped the unfinished execution future. For Python, that discarded the continuation that releases its serialized kernel queue slot in `execute_queued`. The interrupt callback can finish the Python cell, but cannot resume the dropped Rust continuation. Subsequent calls wait on the unreleased queue tail.

The TypeScript reference keeps the original tool promise running after its abort race rejects: `packages/agent/src/agent-loop.ts:192-236` and `:1161-1180`. Its kernel queue releases the slot in `finally`, `packages/coding-agent/src/core/kernel/repl-manager.ts:1010-1018`.

The Rust loop now owns each tool execution in a Tokio task and races its join handle against cancellation. Cancellation still reaches the tool and returns promptly to the agent loop. Dropping the join handle leaves the tool's interrupt and queue-release cleanup running. Late updates remain suppressed by the existing cancellation/accepting-updates guards; the detached result does not emit another tool result or continue the model loop. Normal results and failures still pass through the same result handling, and task failures become ordinary tool errors.

No daemon protocol, Python source, TypeScript source, or terminal rendering changes are involved. The shared agent-loop fix applies to every native platform; compilation and installation checks are performed on Linux only. No tests or model/provider calls are run, per user instruction. The causal path is verified in source; no debugger backtrace or live interruption regression was obtained.

An already-stranded worker cannot load the new executable or reconstruct its dropped continuation. The installation uses a new release-specific daemon socket. Relaunch `optimus-agent` and use `/resume` to reopen the saved conversation on the updated worker. Existing processes and session files are preserved; the in-memory Python namespace is restored only to the extent supported by its saved snapshot.
