# Running the installed Rust application

The installed command is **`optimus-rust`**. Run it from the project you want to work on:

```bash
cd /path/to/your/project
optimus-rust
```

Use `optimus-rust --help` for command-line options. In the TUI, `/model` opens the searchable picker. Type a model ID, name, or provider; use Up/Down to navigate, Enter to select, and Escape to cancel. `/model <search>` opens a prefilled search, or switches directly when the reference is exact and unambiguous. Alt+S toggles all/scoped models when a model scope is configured; the shortcut follows the `app.model.toggleScope` keybinding. Current and recent models appear first within signed-in providers. Selecting a built-in provider that needs credentials opens its login flow.

This fixes the Rust model picker. The TypeScript configuration menu's combined Providers/Models/MCP tabs are not part of this change; Rust provider login remains available through `/login` and the model picker. Other Rust parity gaps remain listed in [RUST_MAIN_READINESS.md](RUST_MAIN_READINESS.md).

## Linux installation layout

`scripts/optimus-rust` is the maintained launcher for the local installation:

```text
~/.local/bin/optimus-rust
~/.local/share/optimus-rust/
  current -> releases/<commit>
  releases/<commit>/
    COMMIT
    bin/optimus-rust
    package.json
    packages/coding-agent/
    prime-agent-runtime/
    kernel-venv/
~/.config/optimus-rust/
```

The release contains the built executable and the matching Python runtime, skills, and package resources. The launcher sets their locations, uses a separate Rust profile, and isolates daemon sockets from the TypeScript installation. Each release has its own Python environment so an upgrade can be prepared while an older session is running. The launcher preserves the caller's working directory and forwards every argument. It does not copy credentials or sessions automatically.

Once the release bundle and `current` link have been prepared, install or refresh the launcher with:

```bash
mkdir -p "$HOME/.local/bin"
install -m 755 scripts/optimus-rust "$HOME/.local/bin/optimus-rust"
optimus-rust --version
```

`~/.local/bin` must be on your shell's PATH. A source checkout alone does not prepare this release bundle; use the Cargo instructions in the [README](../README.md#get-started) to run from source. Copying only the executable is insufficient for Python tools and bundled skills.

The launcher respects `PRIME_AGENT_CODING_AGENT_DIR` and `PRIME_AGENT_KERNEL_VENV` overrides. `OPTIMUS_RUST_TMPDIR` overrides the socket directory; keep it short enough for Unix socket limits. The default is `$XDG_RUNTIME_DIR/optimus-rust`, falling back to `~/.cache/optimus-rust`. It is separate from Prime's temporary socket directory.

Retain previous releases during upgrades. An already-running client keeps its loaded executable; relaunch `optimus-rust` to use an updated client. Do not replace a Python environment while it is executing tools.
