# Running the installed Rust application

The installed command is **`optimus-agent`**. Run it from the project you want to work on:

```bash
cd /path/to/your/project
optimus-agent
```

Use `optimus-agent --help` for command-line options. In the TUI, `/model` opens the searchable picker. Type a model ID, name, or provider; use Up/Down to navigate, Enter to select, and Escape to cancel. `/model <search>` opens a prefilled search, or switches directly when the reference is exact and unambiguous. Alt+S toggles all/scoped models when a model scope is configured; the shortcut follows the `app.model.toggleScope` keybinding. Current and recent models appear first within signed-in providers. Selecting a built-in provider that needs credentials opens its login flow.

This fixes the Rust model picker. The TypeScript configuration menu's combined Providers/Models/MCP tabs are not part of this change; Rust provider login remains available through `/login` and the model picker. Other Rust parity gaps remain listed in [RUST_MAIN_READINESS.md](RUST_MAIN_READINESS.md).

## Linux installation layout

`scripts/optimus-agent` is the maintained launcher for the local installation:

```text
~/.local/bin/optimus-agent
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
install -m 755 scripts/optimus-agent "$HOME/.local/bin/optimus-agent.new"
mv -f "$HOME/.local/bin/optimus-agent.new" "$HOME/.local/bin/optimus-agent"
optimus-agent --version
```

`~/.local/bin` must be on your shell's PATH. A source checkout alone does not prepare this release bundle; use the Cargo instructions in the [README](../README.md#get-started) to run from source. Copying only the executable is insufficient for Python tools and bundled skills.

The temporary launcher replaces an older `optimus-agent -> optimus-rust` symlink without following it. After confirming the new command works, remove the old managed `~/.local/bin/optimus-rust` launcher and run `hash -r` in existing Bash shells. Only `optimus-agent` is installed on PATH. Cargo's internal executable and Linux process name remain `optimus-rust`; the existing release and configuration directories keep their names.

The launcher respects `PRIME_AGENT_CODING_AGENT_DIR` and `PRIME_AGENT_KERNEL_VENV` overrides. `OPTIMUS_RUST_TMPDIR` overrides the socket directory; keep it short enough for Unix socket limits. The default is `$XDG_RUNTIME_DIR/optimus-rust/<release>`, falling back to `~/.cache/optimus-rust/<release>`. It is separate from Prime's temporary socket directory.

Closing the TUI disconnects that client; the supervisor and resident sessions can continue in the background. Client-owned temporary sessions have their own disconnect cleanup. Changing the launcher does not restart existing processes.

To reuse an existing Prime OAuth login, share its `auth.json` through a symlink after backing up the Rust credential file. Independent copies of a rotating refresh token can become stale. Rust resolves the credential path before locking, so it coordinates refreshes with Prime's `proper-lockfile` lock on the original file. Settings, sessions, and daemon state can remain in the separate Rust profile. Only share credentials between trusted local profiles.

To use an existing Codex CLI login instead, configure `providers.openai-codex.apiKey` in the Rust profile's `models.json` with a command that reads the current access token:

```json
"apiKey": "!python3 -c 'import json; from pathlib import Path; print(json.loads((Path.home()/\".codex/auth.json\").read_text())[\"tokens\"][\"access_token\"])'"
```

Back up and remove any `openai-codex` entry in the Rust profile's `auth.json` first, because stored credentials take precedence over `models.json`. Keep the existing model definitions. This command is resolved for each request; Codex remains responsible for its login and token refresh. Use the actual Codex auth path if `CODEX_HOME` is customized. Do not symlink the whole Codex auth file: its format differs from Prime's.

Retain previous releases during upgrades. An already-running client keeps its loaded executable; relaunch `optimus-agent` to use an updated client. Do not replace a Python environment while it is executing tools.
