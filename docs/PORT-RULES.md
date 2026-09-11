# PORT RULES - read before writing any Rust in this repo

This repository is a behavioural port of the first-party TypeScript application
layer (packages/agent, packages/ai, packages/coding-agent, packages/tui) to Rust.
Read docs/BASELINE.md for the pinned reference.

## The one rule that matters

PORT IT. DO NOT REIMAGINE IT.

Choose the boring 1:1 implementation over the elegant redesign every time.
Idiomatic Rust is SECONDARY to observable compatibility.

## Hard boundaries

- Python stays Python. Never translate, rewrite or "improve" anything under
  prime-agent-runtime/ or any *.py file. Rust talks to the existing
  `python -m rlm.repl` JSON-lines protocol (protocol version 3).
- Never modify anything under packages/**. The TypeScript is the read-only
  reference and must stay runnable for differential tests.
- Never touch production paths: C:/Users/openclawuser/.prime/**,
  C:/Users/openclawuser/optimus-main-20260911/**, the production venv, the
  production named pipe, or any running Prime process.
- Do not install global tooling, do not edit PATH or the registry.
- Do not `git commit`, `git push`, `git checkout`, `git stash` or `git reset`.
  The lead commits. Your job is files + verification evidence.

## Rust toolchain (private, no PATH changes needed)

Every shell needs the private toolchain. Prefix commands with the helper:

    cd /c/Users/openclawuser/optimus-rust-port
    MSYS_NO_PATHCONV=1 cmd /c "call scripts\rust-env.cmd && cargo check -p pi-ai"

or set the environment yourself:

    RUSTUP_HOME=C:\Users\openclawuser\optimus-rust-toolchain\rustup
    CARGO_HOME=C:\Users\openclawuser\optimus-rust-toolchain\cargo
    PATH=%CARGO_HOME%\bin;%PATH%

Keep builds bounded. Set `CARGO_BUILD_JOBS=4` so parallel workers do not thrash
the machine. A shared target directory is intentional; "Blocking waiting for
file lock on build directory" is normal and resolves by itself.

**BUILD IT ALL AT ONCE. TEST AT THE END.** Hish's explicit rule for this port:
do not test at every step or after every file. Write the whole slice first, then
run ONE `cargo check -p <crate>` (and one `cargo test -p <crate>`) at the end of
the slice. Do not loop file-by-file through compile/fix cycles. Do not stop after
one file. Finish the slice, then check once, fix the real errors, and report.

## 1:1 mapping rules

- File mapping: `packages/<pkg>/src/<a>/<b>.ts` maps to
  `crates/<crate>/src/<a>/<b>.rs` (kebab-case becomes snake_case).
  `index.ts` maps to `mod.rs` in the same directory. The authoritative table is
  `evidence/migration-map.json`.
- Function mapping: every significant TypeScript function gets an identifiable
  Rust function. `executePythonCommand` -> `execute_python_command`.
- Type mapping:
  - `string` -> `String` / `&str`
  - `number` -> `f64` unless the code proves integer semantics, then `i64`/`u64`/`usize`
  - `boolean` -> `bool`
  - `null` -> `Option<T>` only when `null` is meaningful; `undefined` (missing
    key) must stay distinguishable from `null` when it is observable - use
    `Option<Option<T>>` or an explicit enum, and say why in a comment
  - `Promise<T>` -> `async fn -> T` / `Future<Output = T>`
  - `Array<T>` -> `Vec<T>`
  - `Map<K,V>` -> `indexmap::IndexMap` when iteration order is observable,
    `std::collections::HashMap` only when order cannot be observed
  - `Buffer` -> `Vec<u8>` / `bytes::Bytes`
  - `interface` -> `struct` (+ `trait` where polymorphism is real)
  - union of literals -> `enum` with `#[serde(rename_all = ...)]`
  - `any`/`unknown` -> `serde_json::Value`
- Preserve exactly: JSON field names (serde rename), insertion order, absent vs
  null, numeric semantics, event ordering, cancellation points, error messages,
  exit codes, environment variable names, CLI flags, file formats, and defaults.
- Keep constants, magic numbers and string literals identical to the TypeScript.
- Comments: keep the TypeScript comments where they explain a non-obvious
  decision. Do not write new essays.

## Concurrency rules

- `tokio` is the runtime. Preserve the ordering the TypeScript guarantees.
- JS `EventEmitter`/callback streams -> `tokio::sync::broadcast`/`mpsc` with the
  same emission order. `AbortSignal` -> `tokio_util::sync::CancellationToken`
  (or an equivalent that preserves abort semantics).
- Never introduce a new concurrency structure that changes ordering, buffering,
  or error propagation.

## Definition of done for one slice (not one file)

0. BUILD IT ALL AT ONCE. Do not run cargo between files. Do not test at every
   step. Write every file in the slice, then run exactly one `cargo check` and
   one `cargo test` at the end.
1. The Rust file exists at the mapped path.
2. Every significant TypeScript function has an identifiable Rust counterpart.
3. `cargo check -p <crate>` passes with no errors (warnings are acceptable while
   the port is incomplete, but note them). Run this ONCE, at the end of the slice.
4. Tests that exist for the TypeScript file have a Rust equivalent where the
   behaviour is testable without real network or real credentials.
5. You write a short status entry to `evidence/status/<slice>.json`:
   `{"slice": ..., "files": [{"src": ..., "rust": ..., "status": "ported|partial|blocked",
   "functions_mapped": n, "notes": "..."}], "build": "cargo check -p X: ok|failed",
   "blocked_on": "..."}`
6. Report honestly. "Compiles" is not "ported". If you could not port something,
   say exactly what and why.

## Scratch files

Write scratch/temporary files only under `.port-env/tmp/`. Never write temporary
or generated data into `evidence/` - that directory holds final evidence only.
Generated Rust data tables belong in the crate (`crates/<crate>/src/...`).

## Fake providers only

Never call a real provider, never use real API keys, never read
C:/Users/openclawuser/.prime/auth.json. Use synthetic fixtures and the faux
provider that exists in packages/ai/src/providers/faux.ts.
