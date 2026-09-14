# Evidence: private Python kernel environment and child-process isolation

Date: 2026-09-11

## Private Python venv (.port-env/python)

- Built with uv from the same pinned interpreter as production: CPython 3.11.15
  (`uv venv --python 3.11.15`), matching the production kernel venv's
  `pyvenv.cfg` home (`uv/python/cpython-3.11.15-windows-x86_64-none`).
- `prime-agent-runtime` installed editable from this clone
  (prime-agent-runtime/src/rlm), so the Python source bytes are the pinned ones.
- All 12 Python skill packages installed editable from
  packages/coding-agent/skills/{agent-message,agent-observe,attach-image,compact,
  edit,goal,linear,memory,notion,refine,rlm-heartbeat,websearch}.
- Package set verified against the production kernel venv by comparing
  `importlib.metadata.distributions()` names: **65 vs 65, zero differences**.
- No installs, edits, bytecode writes or tests were performed in the production
  venv. It was read only via `python -c` metadata queries.

## Kernel protocol probe (private process)

`python -m rlm.repl` started with the private venv and PYTHONPATH pointing at the
clone's prime-agent-runtime/src. Observed exchange:

```
{"event":"ready","protocol":3,"python":"3.11.15","snapshotFormats":["legacy","cas-v2"]}
{"event":"result","id":"c1","text":"42"}
{"event":"done","id":"c1","status":"ok"}
{"event":"done","id":"c2","status":"ok"}
```

Confirms protocol version 3, newline-delimited JSON, `execute`/`shutdown`
requests and `ready`/`result`/`done` events - the same contract the Rust port
must speak.

## Child-process isolation proof

`scripts/port-isolation.ps1` builds an explicit private environment map and
refuses to run if any resolved path still points at production state. Proof run
through that script, inside a kernel cell:

```
PRIME_AGENT_CODING_AGENT_DIR = C:\Users\openclawuser\optimus-rust-port\.port-env\iso\agent
TEMP                        = C:\Users\openclawuser\optimus-rust-port\.port-env\iso\tmp
```

Private roots created under `.port-env/iso/`: agent, sessions, artifacts, memory,
harness, cache, tmp, pipes, logs, kernel, home.

## Not yet done

- Reference (TypeScript) differential runs still need a private build of the
  reference checkout; no production process has been started or attached to.
- No real provider, gateway or Telegram endpoint has been contacted.
