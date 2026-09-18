"""Pipeline status regressions runnable on POSIX and native Windows Git Bash."""

from __future__ import annotations

import asyncio
import os
import shutil
import unittest
from unittest import mock

from rlm import bash


class BashPipelineTest(unittest.IsolatedAsyncioTestCase):
    def setUp(self):
        shell = os.environ.get("PRIME_AGENT_BASH_SHELL")
        if not shell and os.name == "posix":
            shell = shutil.which("bash")
        if not shell:
            self.skipTest("Requires Bash; set PRIME_AGENT_BASH_SHELL on Windows")
        self.enterContext(mock.patch.dict(os.environ, {
            "PRIME_AGENT_BASH_SHELL": shell,
            "PRIME_AGENT_BASH_COMMAND_PREFIX": "",
            "PRIME_AGENT_INTERNAL_ORPHAN_PROCESS_JOURNAL": "",
            "PRIME_AGENT_KERNEL_OWNER_PID": "",
        }))

    async def run_command(self, command):
        handle = bash(command)
        try:
            result = await asyncio.wait_for(handle, timeout=10)
            await asyncio.wait_for(handle._wait_reaped(), timeout=10)
            self.assertEqual(handle.poll(), result)
            return result
        finally:
            if not handle._reaped:
                handle.kill(grace=0.1)
                await asyncio.wait_for(handle._wait_reaped(), timeout=10)

    async def test_successful_pipelines_preserve_output(self):
        result = await self.run_command("printf 'ok\\n' | cat | cat")
        self.assertEqual(result.exit_code, 0)
        self.assertEqual(result.output.strip(), "ok")

    async def test_pipeline_producer_failure_is_not_hidden(self):
        for command, status in (
            ("false | cat", 1),
            ("(exit 3) | cat", 3),
            ("(exit 7) | cat | cat", 7),
            ("false | (exit 3) | cat", 3),
        ):
            with self.subTest(command=command):
                result = await self.run_command(command)
                self.assertEqual(result.exit_code, status)

    async def test_explicit_141_is_never_changed_to_success(self):
        for command, status in (
            ("exit 141", 141),
            ("(exit 141)", 141),
            ("(exit 141) | cat", 141),
            ("printf '' | (exit 141)", 141),
            ("false | (exit 141)", 141),
            ("(exit 141) | (exit 3)", 3),
        ):
            with self.subTest(command=command):
                result = await self.run_command(command)
                self.assertEqual(result.exit_code, status)

    async def test_sigpipe_is_nonzero_unless_caller_explicitly_accepts_it(self):
        result = await self.run_command("yes | head -1")
        self.assertEqual(result.exit_code, 141)
        self.assertEqual(result.output.strip(), "y")

        result = await self.run_command(
            "yes | head -1; pipeline_status=$?; "
            'if [ "$pipeline_status" -ne 0 ] && [ "$pipeline_status" -ne 141 ]; '
            'then exit "$pipeline_status"; fi'
        )
        self.assertEqual(result.exit_code, 0)
        self.assertEqual(result.output.strip(), "y")

    async def test_commands_are_not_replayed_or_implicitly_errexit(self):
        result = await self.run_command("printf 'once\\n'; false | cat")
        self.assertEqual(result.exit_code, 1)
        self.assertEqual(result.output.strip(), "once")
        result = await self.run_command("false | cat; printf 'after\\n'")
        self.assertEqual(result.exit_code, 0)
        self.assertEqual(result.output.strip(), "after")

    async def test_prefix_and_user_shell_state_are_retained(self):
        with mock.patch.dict(os.environ, {
            "PRIME_AGENT_BASH_COMMAND_PREFIX": "PIPELINE_PREFIX=ready; printf 'prefix\\n'",
        }):
            result = await self.run_command('printf "%s\\n" "$PIPELINE_PREFIX"; false | cat')
        self.assertEqual(result.exit_code, 1)
        self.assertEqual(result.output.splitlines(), ["prefix", "ready"])

    async def test_background_handle_poll_keeps_pipeline_status(self):
        handle = bash("sleep 0.1; (exit 3) | cat")
        try:
            self.assertIsNone(handle.poll())
            await asyncio.wait_for(handle._wait_reaped(), timeout=10)
            self.assertEqual(handle.poll().exit_code, 3)
            self.assertEqual((await handle).exit_code, 3)
        finally:
            if not handle._reaped:
                handle.kill(grace=0.1)
                await asyncio.wait_for(handle._wait_reaped(), timeout=10)

    @unittest.skipUnless(os.name == "posix" and os.path.exists("/bin/dash"), "POSIX dash only")
    async def test_dash_fallback_parses_and_retains_native_status_semantics(self):
        with mock.patch.dict(os.environ, {"PRIME_AGENT_BASH_SHELL": "/bin/dash"}):
            for command, status in (("printf 'dash\\n'", 0), ("(exit 141)", 141), ("false | cat", 0)):
                with self.subTest(command=command):
                    result = await self.run_command(command)
                    self.assertEqual(result.exit_code, status)


if __name__ == "__main__":
    unittest.main()
