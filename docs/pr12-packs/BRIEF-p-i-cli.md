TASK: drive ONE pack of Rust compile errors to zero in an existing TypeScript->Rust port.

Repo: C:/Users/openclawuser/optimus-rust-port   (Rust, workspace crates pi-ai, pi-agent-core, pi-tui, pi-coding-agent)
TS reference (READ-ONLY, the ground truth for behaviour and signatures): C:/Users/openclawuser/optimus-main-20260911/packages
Worklist for your pack (read it FIRST): docs/pr12-packs/p-i-cli.md
Owned-file list: docs/pr12-packs/p-i-cli.files

HARD RULES
1. Edit ONLY the files in your .files list, and only the files in your worklist. Other packs are
   being edited concurrently by other workers. If you need a change in another file, message the
   lead (receiver_role='parent') and continue with something else - never edit outside your list.
2. Port, do not reinvent. Read the TypeScript source for every symbol you touch. Match names, field
   names, JSON field names, constants and behaviour 1:1. NEVER invent a function, method, type or
   field that does not exist in the TS.
3. NO stand-ins, NO stubs, NO `todo!()`, NO `unimplemented!()`, NO empty bodies, NO returning
   `Value::Null` where TS returns real data, NO cfg-gating to hide an error, NO duplicate/shadow
   types, NO conversion shims, NO deleting or weakening tests.
4. Prefer the canonical owner over a local duplicate. If your file declares a local copy of a type
   that already exists elsewhere in the crate (or in pi-tui/pi-ai), re-export or use the canonical
   one and fix the call sites in YOUR files. Check the TS imports to prove which one is canonical.
5. Align callers to the canonical API rather than changing the canonical API to match a caller.
6. Rust toolchain is PRIVATE. Always run cargo through this environment:
     cd /c/Users/openclawuser/optimus-rust-port
     CARGO_BUILD_JOBS=4 CARGO_INCREMENTAL=0 CARGO_TARGET_DIR=.port-env/target \
       cargo check -p pi-coding-agent --tests --message-format=json > .port-env/tmp/w-p-i-cli-runN.jsonl 2> .port-env/tmp/w-p-i-cli-runN.err
   (RUSTUP_HOME=C:\Users\openclawuser\optimus-rust-toolchain\rustup, CARGO_HOME=...\cargo if cargo is not found.)

MEASUREMENT GATE - a run is only usable if ALL of these hold. Each rule exists because someone was
already fooled by it:
  a) the run must be COMPLETE: await the command to exit and require the .err tail to contain
     "could not compile" or "Finished". A jsonl measured while still being written gives false zeros.
  b) pi-coding-agent must have emitted WARNINGS (>0). Zero warnings means rustc ABORTED before
     type-checking, so every count is meaningless. Discard the run.
  c) measure with --tests. Plain `cargo check` never type-checks #[cfg(test)] modules.
  d) count UNIQUE (file, line, message) sites; rustc emits each error once per target.
Quick per-file parse probe: `rustfmt --edition 2021 --emit stdout FILE >/dev/null` exit 0 = parses.

METHOD
- Work file by file. After each file or small group, re-run the check and confirm YOUR files went
  down. Fix causes, not symptoms: if one root cause produces 5 errors, fix the cause once.
- Read the TS for the same file (path mirrors: crates/pi-coding-agent/src/X  <->  packages/coding-agent/src/X,
  .rs -> .ts). Use it to check the real signature before you change a call site.
- If an error's fix truly belongs to another file/pack, note it and skip; report it as a blocker.

REPORT BACK with `await agent_message.send(message, receiver_role='parent')`:
- your pack name; errors before -> after; the gate evidence (jsonl path, warnings count, complete yes/no);
- files you changed; any blocker you skipped (file:line + what is needed);
- confirm: 0 todo!/unimplemented!, no files edited outside your list, no tests deleted.
Report only numbers you actually measured. If a run was invalid, say so and re-run.
