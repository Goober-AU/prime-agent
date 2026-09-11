"""Smoke test for the private kernel client (test-only tooling)."""
from __future__ import annotations

import sys
import pathlib

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import kernel_client as k  # noqa: E402


def test_kernel_speaks_protocol_3_and_keeps_state() -> None:
    kernel = k.KernelProcess.start()
    try:
        ready = k.require_ready(kernel)
        assert ready["protocol"] == k.PROTOCOL_VERSION

        assert kernel.execute("v = 40 + 2\nv", "c1")["status"] == "ok"
        results = [e for e in kernel.events_for("c1") if e["event"] == "result"]
        assert results and results[0]["text"] == "42"

        # state persists across cells
        assert kernel.execute("v += 1", "c2")["status"] == "ok"
        kernel.execute("print(v)", "c3")
        stdout = "".join(e["text"] for e in kernel.events_for("c3") if e["event"] == "stdout")
        assert stdout.strip() == "43"
    finally:
        kernel.shutdown()
        kernel.close()


def test_private_env_never_points_at_production() -> None:
    env = k.private_env()
    for key in ("PRIME_AGENT_CODING_AGENT_DIR", "TEMP", "HOME", "USERPROFILE"):
        assert ".prime" not in env[key].replace("\\", "/").lower(), key
    assert env["PRIME_AGENT_KERNEL_PYTHON"].startswith(str(k.ROOT))
