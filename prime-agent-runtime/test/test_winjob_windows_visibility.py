"""Real Windows console and containment checks with isolated, harmless children."""

from __future__ import annotations

import asyncio
import ctypes
import os
from pathlib import Path
import sys
import tempfile
import time
import unittest
from unittest import mock

from rlm import _winjob, bash


@unittest.skipUnless(os.name == "nt", "requires real Windows Job Objects")
class WinJobVisibilityTest(unittest.TestCase):
    def test_explicit_git_bash_works_with_an_unusable_path_and_space_directory(self):
        shell = Path("C:/Program Files/Git/bin/bash.exe")
        if not shell.is_file():
            self.skipTest("default Git Bash is not installed")
        with tempfile.TemporaryDirectory(prefix="optimus shell cwd ") as directory:
            original_cwd = os.getcwd()
            try:
                os.chdir(directory)
                with mock.patch.dict(os.environ, {"PRIME_AGENT_BASH_SHELL": str(shell), "PATH": directory}):
                    async def run_command():
                        return await bash("printf 'explicit-shell-ok\\n'; pwd")
                    result = asyncio.run(run_command())
                self.assertEqual(result.exit_code, 0, result.output)
                self.assertIn("explicit-shell-ok", result.output)
                self.assertIn(Path(directory).name, result.output)
            finally:
                os.chdir(original_cwd)

    def test_hidden_job_child_keeps_output_and_has_no_console(self):
        # An inherited existing console is not evidence of a newly flashed window.
        # This diagnostic checks only the headless worker-launch contract.
        kernel32 = ctypes.WinDLL("kernel32")
        kernel32.GetConsoleWindow.restype = ctypes.c_void_p
        if kernel32.GetConsoleWindow():
            self.skipTest("requires a headless parent; an inherited console is legitimate")
        with tempfile.TemporaryDirectory(prefix="optimus shell visibility ") as directory:
            job = _winjob.create_job()
            self.assertIsNotNone(job)
            process = None
            try:
                code = (
                    "import ctypes,os; "
                    "k=ctypes.WinDLL('kernel32'); k.GetConsoleWindow.restype=ctypes.c_void_p; "
                    "print(str(k.GetConsoleWindow() or 0)+':'+os.path.basename(os.getcwd()))"
                )
                process = _winjob.spawn_in_job(job, [sys.executable, "-c", code], directory, dict(os.environ))
                self.assertTrue(process.resume())
                self.assertEqual(process.wait(timeout=10), 0)
                self.assertEqual(
                    process.stdout.read().decode().strip(),
                    "0:" + Path(directory).name,
                    "background bash/tool jobs must not create a console window",
                )
            finally:
                _winjob.terminate(job)
                if process is not None:
                    process.wait(timeout=5)
                    process.stdout.close()
                    process.close()
                deadline = time.monotonic() + 5
                while _winjob.is_empty(job) is False and time.monotonic() < deadline:
                    time.sleep(0.01)
                empty = _winjob.is_empty(job)
                _winjob.close(job)
                self.assertTrue(empty, "the isolated job must have no surviving descendants")


if __name__ == "__main__":
    unittest.main()
