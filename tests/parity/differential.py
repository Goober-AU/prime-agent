"""Differential parity runner (test-only tooling).

Compares the Python boundary between the two implementations:

  TypeScript reference  ->  python -m rlm.repl
  Rust candidate        ->  python -m rlm.repl

Both sides run in PRIVATE environments with separate kernel processes, separate
namespaces and separate state. Neither side ever contacts production.

Usage (from the repo root):

  python tests/parity/differential.py --cases tests/parity/cases/basic.json

Each case file is a list of cells plus the observable fields to compare:
  {"name": "...", "cells": ["a = 1", "a + 1"], "compare": ["result", "stdout", "status"]}
"""
from __future__ import annotations

import argparse
import json
import pathlib
import sys
from typing import Any

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import kernel_client as k  # noqa: E402

ROOT = pathlib.Path(__file__).resolve().parents[2]


def observe(kernel: k.KernelProcess, cells: list[str], compare: list[str]) -> list[dict[str, Any]]:
    """Run cells in order and collect the comparable observables for each cell."""
    observations: list[dict[str, Any]] = []
    for index, code in enumerate(cells):
        request_id = f"cell-{index}"
        done = kernel.execute(code, request_id, timeout=60.0)
        events = kernel.events_for(request_id)
        observation = {
            "id": request_id,
            "status": (done or {}).get("status"),
            "result": next((e.get("text") for e in events if e.get("event") == "result"), None),
            "stdout": "".join(e.get("text", "") for e in events if e.get("event") == "stdout"),
            "stderr": "".join(e.get("text", "") for e in events if e.get("event") == "stderr"),
            "error": next(
                (
                    {"ename": e.get("ename"), "evalue": e.get("evalue")}
                    for e in events
                    if e.get("event") == "error"
                ),
                None,
            ),
            "display": [e.get("data") for e in events if e.get("event") == "display"],
        }
        observations.append(observation)
    return observations


def normalise(observation: dict[str, Any], fields: list[str]) -> dict[str, Any]:
    """Keep only the requested fields, so a case compares exactly what it declares."""
    return {field: observation.get(field) for field in fields}


def run_case(case: dict[str, Any], python: pathlib.Path | None = None) -> dict[str, Any]:
    cells = case["cells"]
    fields = case.get("compare", ["status", "result", "stdout"])
    kernel = k.KernelProcess.start()
    try:
        ready = k.require_ready(kernel)
        observed = observe(kernel, cells, fields)
    finally:
        kernel.shutdown()
        kernel.close()
    return {
        "name": case.get("name", "case"),
        "protocol": ready.get("protocol"),
        "python": ready.get("python"),
        "snapshot_formats": ready.get("snapshotFormats"),
        "observations": [normalise(o, fields) for o in observed],
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--cases", required=True, help="path to a JSON case file")
    parser.add_argument("--out", default="", help="write the result JSON here")
    args = parser.parse_args()

    cases = json.loads(pathlib.Path(args.cases).read_text(encoding="utf-8"))
    results = [run_case(case) for case in cases]
    payload = json.dumps(results, indent=1)
    if args.out:
        pathlib.Path(args.out).write_text(payload, encoding="utf-8")
        print(f"wrote {args.out} ({len(results)} cases)")
    else:
        print(payload)
    return 0


if __name__ == "__main__":
    sys.exit(main())
