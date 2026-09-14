"""Differential/parity harness scaffolding for the Rust port.

This module is test-only tooling. It never touches the production install: every
process it starts uses scripts/port-isolation.ps1's environment map and the
private venv at .port-env/python.

Protocol under test (prime-agent-runtime/src/rlm/repl.md, version 3):
  requests  -> {"type": "execute"|"interrupt"|"host_reply"|"snapshot"|"restore"
                          |"snapshot_export_legacy"|"list_names"|"shutdown", ...}
  events    <- {"event": "ready"|"stdout"|"stderr"|"result"|"display"
                          |"host_request"|"error"|"done", ...}
"""
from __future__ import annotations

import json
import os
import pathlib
import subprocess
import threading
import time
from dataclasses import dataclass, field
from typing import Any

# tests/parity/kernel_client.py -> repo root is three levels up
ROOT = pathlib.Path(__file__).resolve().parents[2]
PRIVATE_PYTHON = ROOT / ".port-env" / "python" / "Scripts" / "python.exe"
PRIVATE_ISO = ROOT / ".port-env" / "iso"
RUNTIME_SRC = ROOT / "prime-agent-runtime" / "src"

PROTOCOL_VERSION = 3


def private_env() -> dict[str, str]:
    """Explicit private environment for a kernel child process.

    Mirrors scripts/port-isolation.ps1. Any value that resolves inside the
    production agent directory is a hard error, so a mistake fails loudly
    instead of silently reading or writing production state.
    """
    env = dict(os.environ)
    env["PRIME_AGENT_CODING_AGENT_DIR"] = str(PRIVATE_ISO / "agent")
    env["PI_CODING_AGENT_DIR"] = str(PRIVATE_ISO / "agent")
    env["PRIME_AGENT_SESSION_DIR"] = str(PRIVATE_ISO / "sessions")
    env["PRIME_AGENT_CODING_AGENT_SESSION_DIR"] = str(PRIVATE_ISO / "sessions")
    env["PRIME_AGENT_KERNEL_PYTHON"] = str(PRIVATE_PYTHON)
    env["PRIME_AGENT_KERNEL_VENV"] = str(PRIVATE_ISO / "kernel")
    env["PYTHONPATH"] = str(RUNTIME_SRC)
    env["TEMP"] = str(PRIVATE_ISO / "tmp")
    env["TMP"] = str(PRIVATE_ISO / "tmp")
    env["HOME"] = str(PRIVATE_ISO / "home")
    env["USERPROFILE"] = str(PRIVATE_ISO / "home")
    env["PRIME_AGENT_TELEMETRY"] = "0"
    env.pop("NODE_OPTIONS", None)
    for key in ("PRIME_AGENT_CODING_AGENT_DIR", "PRIME_AGENT_SESSION_DIR", "TEMP", "TMP", "HOME"):
        value = env.get(key, "")
        if value and ".prime" in value.replace("\\", "/").lower():
            raise RuntimeError(f"{key} points at production state: {value}")
    return env


@dataclass
class KernelProcess:
    """A private `python -m rlm.repl` process speaking newline-delimited JSON."""

    process: subprocess.Popen
    events: list[dict[str, Any]] = field(default_factory=list)
    _lock: threading.Lock = field(default_factory=threading.Lock)
    _reader: threading.Thread | None = None

    @classmethod
    def start(cls) -> "KernelProcess":
        for name in ("agent", "sessions", "tmp", "home", "kernel"):
            (PRIVATE_ISO / name).mkdir(parents=True, exist_ok=True)
        process = subprocess.Popen(
            [str(PRIVATE_PYTHON), "-m", "rlm.repl"],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env=private_env(),
            cwd=str(PRIVATE_ISO / "tmp"),
            text=True,
            encoding="utf-8",
            bufsize=1,
        )
        kernel = cls(process=process)
        kernel._reader = threading.Thread(target=kernel._pump, daemon=True)
        kernel._reader.start()
        return kernel

    def _pump(self) -> None:
        assert self.process.stdout is not None
        for line in self.process.stdout:
            line = line.strip()
            if not line:
                continue
            try:
                event = json.loads(line)
            except json.JSONDecodeError:
                continue
            with self._lock:
                self.events.append(event)

    def send(self, request: dict[str, Any]) -> None:
        assert self.process.stdin is not None
        self.process.stdin.write(json.dumps(request) + "\n")
        self.process.stdin.flush()

    def wait_for(self, predicate, timeout: float = 30.0) -> dict[str, Any] | None:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            with self._lock:
                for event in self.events:
                    if predicate(event):
                        return event
            time.sleep(0.02)
        return None

    def wait_for_done(self, request_id: str, timeout: float = 30.0) -> dict[str, Any] | None:
        return self.wait_for(
            lambda e: e.get("event") == "done" and e.get("id") == request_id, timeout
        )

    def events_for(self, request_id: str) -> list[dict[str, Any]]:
        with self._lock:
            return [e for e in self.events if e.get("id") == request_id]

    def execute(self, code: str, request_id: str, timeout: float = 30.0) -> dict[str, Any] | None:
        self.send({"type": "execute", "id": request_id, "code": code})
        return self.wait_for_done(request_id, timeout)

    def shutdown(self) -> None:
        try:
            self.send({"type": "shutdown", "id": "shutdown"})
            self.process.wait(timeout=10)
        except Exception:
            self.process.kill()

    def close(self) -> None:
        try:
            self.process.kill()
        except Exception:
            pass
        for stream in (self.process.stdin, self.process.stdout, self.process.stderr):
            try:
                if stream:
                    stream.close()
            except Exception:
                pass


def require_ready(kernel: KernelProcess, timeout: float = 30.0) -> dict[str, Any]:
    ready = kernel.wait_for(lambda e: e.get("event") == "ready", timeout)
    if ready is None:
        raise RuntimeError("kernel did not emit a ready event")
    if ready.get("protocol") != PROTOCOL_VERSION:
        raise RuntimeError(f"unexpected protocol version: {ready.get('protocol')}")
    return ready
