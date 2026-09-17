from __future__ import annotations

import io
import os
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "src"))

import dill
from rlm import repl
from rlm.snapshot_serializer import dump_snapshot_value


class CustomList(list):
    reductions = 0

    def __reduce_ex__(self, protocol):
        type(self).reductions += 1
        return (type(self), (list(self),))


class SnapshotSpeedTests(unittest.TestCase):
    def serialize(self, value, cap=1 << 20):
        stream = io.BytesIO()
        dump_snapshot_value(dill, value, stream, repl._CappedWriter(stream, cap))
        return stream.getvalue()

    def test_builtin_graph_uses_native_serializer_and_keeps_aliases_cycles(self):
        shared = [None, True, 1, 1.5, "text", b"bytes"]
        cycle = []
        cycle.append(cycle)
        value = {"a": shared, "b": shared, "cycle": cycle, "set": {1, 2}, "frozen": frozenset({3}), "tuple": (4, 5)}
        with mock.patch.object(dill, "dump", side_effect=AssertionError("slow serializer used")):
            blob = self.serialize(value)
        restored = dill.loads(blob)
        self.assertIs(restored["a"], restored["b"])
        self.assertIs(restored["cycle"], restored["cycle"][0])
        self.assertEqual(restored["a"], shared)
        shared.append("changed")
        self.assertEqual(dill.loads(self.serialize(value))["a"][-1], "changed")

    def test_custom_reducer_runs_once_and_partial_buffer_is_reset(self):
        CustomList.reductions = 0
        value = ["prefix" * 20000, CustomList([1, 2])]
        restored = dill.loads(self.serialize(value))
        self.assertIsInstance(restored[1], CustomList)
        self.assertEqual(restored, value)
        self.assertEqual(CustomList.reductions, 1)

    def test_dynamic_closure_and_class_keep_dill_semantics(self):
        captured = [4]
        class LocalClass:
            def total(self):
                return sum(captured)
        value = {"object": LocalClass(), "function": lambda: sum(captured)}
        captured.append(5)
        restored = dill.loads(self.serialize(value))
        self.assertEqual(restored["object"].total(), 9)
        self.assertEqual(restored["function"](), 9)

    def test_native_and_fallback_paths_enforce_variable_cap(self):
        for value in (["x" * 10000], [CustomList([1]), "x" * 10000]):
            with self.assertRaises(repl._SnapshotSizeLimitExceeded):
                self.serialize(value, cap=100)

    def test_builtin_reducer_values_fall_back_without_changing_their_types(self):
        for value in (bytearray(b"mutable"), complex(1, 2), range(5)):
            restored = dill.loads(self.serialize(value))
            self.assertIs(type(restored), type(value))
            self.assertEqual(restored, value)

    def test_legacy_overflow_writes_only_final_payload_and_restores_latest(self):
        namespace = {f"v{i}": bytes([i]) * 4096 for i in range(24)}
        blobs = {name: dill.dumps(value) for name, value in namespace.items()}
        cap = len(dill.dumps(blobs)) - 1
        with tempfile.TemporaryDirectory() as root:
            path, manifest = os.path.join(root, "state.dill"), os.path.join(root, "state.json")
            result = repl._snapshot_state(namespace, path, manifest, cap, cap, False)
            self.assertNotIn("error", result)
            self.assertEqual(result["written_bytes"], os.path.getsize(path) + os.path.getsize(manifest))
            self.assertLessEqual(os.path.getsize(path), cap)
            self.assertTrue(result["skipped"])
            restored = {}
            self.assertNotIn("error", repl._restore_state(restored, path))
            self.assertEqual(restored, {name: namespace[name] for name in result["saved"]})
            self.assertFalse(list(Path(root).glob("*.tmp")))


if __name__ == "__main__":
    unittest.main()
