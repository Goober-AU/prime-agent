from __future__ import annotations

import asyncio
import importlib
import unittest
from pathlib import Path
from unittest.mock import AsyncMock, patch

rlm = importlib.import_module("rlm")


class CollectTest(unittest.TestCase):
    def test_handles_names_and_pending_results_use_only_the_collect_request(self):
        reply = {"results": [{"rlm_child_id": "child", "session_name": "builder",
                              "session_dir": "/tmp/child", "status": "running", "settled": False,
                              "answer_preview": "partial", "duration_ms": 5.0, "tool_use_count": 1.0}]}
        host = AsyncMock(return_value=reply)
        handle = rlm.RLMSpawnHandle("child", "builder", Path("/tmp/child"), "provider/model")
        with patch.object(rlm, "host_request", host):
            result = asyncio.run(rlm.rlm.collect([handle, " builder "], timeout_ms=12))
        host.assert_awaited_once_with("rlm.collect", {"targets": ["child", "builder"], "timeout_ms": 12})
        self.assertEqual(result[0].status, "running")
        self.assertFalse(result[0].settled)
        self.assertEqual(result[0].session_dir, Path("/tmp/child"))

    def test_all_children_and_terminal_failure_are_valid_snapshots(self):
        host = AsyncMock(return_value={"results": [{"rlm_child_id": "failed", "status": "error",
                                                   "settled": True, "error": "failed to finish"}]})
        with patch.object(rlm, "host_request", host):
            result = asyncio.run(rlm.collect())
        host.assert_awaited_once_with("rlm.collect", {"targets": [], "timeout_ms": 0})
        self.assertTrue(result[0].settled)
        self.assertEqual(result[0].error, "failed to finish")

    def test_bad_inputs_never_reach_the_host(self):
        host = AsyncMock()
        with patch.object(rlm, "host_request", host):
            for timeout in [True, -1, 0.1, 2_147_483_648]:
                with self.assertRaises(TypeError):
                    asyncio.run(rlm.collect(timeout_ms=timeout))
            for target in [False, 2, {}, [""]]:
                with self.assertRaises(TypeError):
                    asyncio.run(rlm.collect(target))
        host.assert_not_called()

    def test_invalid_result_flags_and_numbers_are_rejected(self):
        valid = {"rlm_child_id": "child", "status": "done", "settled": True}
        for field, value in [("settled", "yes"), ("status", []), ("duration_ms", True),
                             ("duration_ms", float("nan")), ("tool_use_count", -1)]:
            with self.assertRaises(RuntimeError):
                rlm._child_result_from_payload({**valid, field: value})


if __name__ == "__main__":
    unittest.main()
