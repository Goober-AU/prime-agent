"""Portable regression probes for Windows snapshot and interrupt failures."""
from __future__ import annotations

import os
import tempfile
import time
import unittest

from test_repl import ReplProcess, one, stream_text


class WindowsStabilityTest(unittest.TestCase):
    def setUp(self):
        self.repl = ReplProcess()
        self.addCleanup(self.repl.close)
        self.repl.ready()

    def test_100_sequential_reads_and_prints(self):
        with tempfile.TemporaryDirectory() as tmp:
            for i in range(100):
                events = self.repl.execute(str(i), f"from pathlib import Path\nprint(Path({tmp!r}).is_dir(), {i})")
                self.assertEqual(one(events, "done")["status"], "ok")
                self.assertEqual(stream_text(events, "stdout"), f"True {i}\n")
        self.assertEqual(self.repl.shutdown(), 0)

    def test_direct_and_nested_live_handles_are_skipped_but_results_restore(self):
        setup = "\n".join([
            "from rlm.bash import BashHandle, BashResult",
            # No process is needed: reject the type before inspecting its internals.
            "handle = object.__new__(BashHandle)",
            "nested = {'handles': [handle]}",
            "finished = BashResult(output='ok', exit_code=0, duration=0.01)",
            "kept = 42",
        ])
        events = self.repl.execute("setup", setup)
        self.assertEqual(one(events, "done")["status"], "ok", events)
        with tempfile.TemporaryDirectory() as tmp:
            path = os.path.join(tmp, "state.dill")
            manifest = os.path.join(tmp, "state.json")
            self.repl.send({"type": "snapshot", "id": "snapshot", "path": path, "manifest_path": manifest})
            done = one(self.repl.until_done("snapshot"), "done")
            self.assertEqual(done["status"], "ok", done)
            self.assertEqual({item["name"] for item in done["skipped"]}, {"handle", "nested"})
            self.assertIn("kept", done["saved"])
            self.assertIn("finished", done["saved"])
            self.assertEqual(one(self.repl.execute("immediate", "print(kept)"), "done")["status"], "ok")
            fresh = ReplProcess()
            self.addCleanup(fresh.close)
            fresh.ready()
            fresh.send({"type": "restore", "id": "restore", "path": path})
            self.assertEqual(one(fresh.until_done("restore"), "done")["status"], "ok")
            check = fresh.execute("check", "print(kept, finished.output)")
            self.assertEqual(stream_text(check, "stdout"), "42 ok\n")
            self.assertEqual(fresh.shutdown(), 0)
        self.assertEqual(self.repl.shutdown(), 0)

    def test_fallback_interrupt_breaks_synchronous_snapshot_and_reuses_kernel(self):
        events = self.repl.execute("setup", "\n".join([
            "import signal",
            "if hasattr(signal, 'pthread_kill'): del signal.pthread_kill",
            "class Blocking:",
            "    def __reduce_ex__(self, protocol):",
            "        while True: pass",
            "blocked = Blocking()",
        ]))
        self.assertEqual(one(events, "done")["status"], "ok")
        with tempfile.TemporaryDirectory() as tmp:
            self.repl.send({"type": "snapshot", "id": "snapshot", "path": os.path.join(tmp, "state.dill"), "manifest_path": os.path.join(tmp, "state.json")})
            time.sleep(0.2)
            self.repl.send({"type": "interrupt", "id": "snapshot"})
            events = self.repl.until_done("snapshot")
            self.assertEqual(one(events, "done")["status"], "error", events)
            self.assertEqual(os.listdir(tmp), [])
        events = self.repl.execute("reuse", "del blocked\nprint('alive')")
        self.assertEqual(one(events, "done")["status"], "ok", events)
        self.assertEqual(stream_text(events, "stdout"), "alive\n")
        self.assertEqual(self.repl.shutdown(), 0)


if __name__ == "__main__":
    unittest.main()
