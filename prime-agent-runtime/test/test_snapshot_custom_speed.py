from __future__ import annotations

import copyreg
import gc
import io
import os
import pickle
import random
import sys
import tempfile
import types
import unittest
import weakref
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "src"))

import dill
from rlm import repl, snapshot_serializer
from rlm.bash import BashHandle


class Box:
    def __init__(self, values):
        self.values = values


class CountedList(list):
    reductions = 0

    def __reduce_ex__(self, protocol):
        type(self).reductions += 1
        return type(self), (list(self),)


class SnapshotCustomSpeedTests(unittest.TestCase):
    def setUp(self):
        settings = dill.settings.copy()
        self.addCleanup(dill.settings.update, settings)
        dill.settings["recurse"] = True

    def serialize(self, value, cap=32 << 20):
        output = io.BytesIO()
        snapshot_serializer.dump_snapshot_value(dill, value, output, repl._CappedWriter(output, cap))
        return output.getvalue()

    def test_large_captured_graph_uses_native_fragment_and_restores_latest_mutation(self):
        namespace = {"rows": [{"id": i, "xyz": [i, 0.5, 1]} for i in range(2000)]}
        exec("def lookup(i):\n return rows[i]", namespace)
        with mock.patch.object(snapshot_serializer, "_value_fragment", wraps=snapshot_serializer._value_fragment) as fragment:
            restored = dill.loads(self.serialize(namespace["lookup"]))
        self.assertTrue(any(call.args[0].getbuffer().nbytes > 10000 for call in fragment.call_args_list))
        self.assertEqual(restored(1200), namespace["rows"][1200])
        namespace["rows"][1200]["xyz"].append("changed")
        self.assertEqual(dill.loads(self.serialize(namespace["lookup"]))(1200)["xyz"][-1], "changed")

    def test_aliases_and_cycles_across_custom_and_native_boundaries(self):
        for protocol in (3, 4, 5):
            with self.subTest(protocol=protocol):
                dill.settings["protocol"] = protocol
                shared = ["same"]
                rows = [shared] * 200
                box = Box(rows)
                rows.append(box)
                cycle = [0] * 200
                cycle.append(cycle)
                value = {"custom_first": box, "rows_again": rows, "shared_again": shared, "cycle": cycle}
                restored = dill.loads(self.serialize(value))
                self.assertIs(restored["custom_first"].values, restored["rows_again"])
                self.assertIs(restored["rows_again"][0], restored["shared_again"])
                self.assertIs(restored["rows_again"][-1], restored["custom_first"])
                self.assertIs(restored["cycle"][-1], restored["cycle"])

    def test_mixed_functions_share_the_same_captured_graph_with_sibling_values(self):
        captured = [{"n": i} for i in range(200)]
        def first():
            return captured
        def second():
            return captured
        restored = dill.loads(self.serialize({"first": first, "captured": captured, "second": second}))
        self.assertIs(restored["first"](), restored["captured"])
        self.assertIs(restored["second"](), restored["captured"])

    def test_native_fragment_framing_with_large_strings_bytes_and_opcode_payloads(self):
        for protocol in (3, 4, 5):
            for large in ("text" * 50000, b"\x95\x80\x04." * 50000):
                with self.subTest(protocol=protocol, kind=type(large)):
                    dill.settings["protocol"] = protocol
                    values = [large] + list(range(50000)) + [large]
                    restored = dill.loads(self.serialize(Box(values)))
                    self.assertEqual(restored.values, values)
                    self.assertIs(restored.values[0], restored.values[-1])

    def test_multiple_native_fragments_share_memo_and_follow_late_custom_values(self):
        shared = ["alias"]
        a = [shared] * 200
        b = [{"i": i} for i in range(200)] + [shared]
        c = tuple([shared] * 200)
        restored = dill.loads(self.serialize(Box([a, b, c, Box(shared)])))
        a2, b2, c2, box2 = restored.values
        self.assertIs(a2[0], b2[-1])
        self.assertIs(a2[0], c2[0])
        self.assertIs(a2[0], box2.values)

    def test_custom_reducers_run_once_even_after_failed_large_native_probe(self):
        CountedList.reductions = 0
        values = list(range(500)) + [CountedList([1])]
        restored = dill.loads(self.serialize(Box(values)))
        self.assertEqual(restored.values, values)
        self.assertIsInstance(restored.values[-1], CountedList)
        self.assertEqual(CountedList.reductions, 1)

    def test_mixed_graph_is_not_repeatedly_probed_at_nested_levels(self):
        original_dump = snapshot_serializer._SubgraphPickler.dump
        probes = []
        def counted_dump(pickler, value):
            probes.append(type(value))
            return original_dump(pickler, value)
        value = Box([Box(list(range(200)))] + [0] * 200)
        with mock.patch.object(snapshot_serializer._SubgraphPickler, "dump", counted_dump):
            restored = dill.loads(self.serialize(value))
        self.assertEqual(restored.values[0].values, list(range(200)))
        self.assertEqual(probes, [list])

    def test_registered_primitive_dispatch_is_not_bypassed(self):
        def save_list(pickler, value):
            pickler.save_reduce(tuple, (tuple(value),), obj=value)
        with mock.patch.dict(dill.Pickler.dispatch, {list: save_list}):
            restored = dill.loads(self.serialize(Box(list(range(200)))))
            primitive = dill.loads(self.serialize(list(range(200))))
        self.assertIsInstance(restored.values, tuple)
        self.assertIsInstance(primitive, tuple)

    def test_custom_reducer_changing_dispatch_mid_dump_is_respected(self):
        def save_list(pickler, value):
            pickler.save_reduce(tuple, (tuple(value),), obj=value)
        class Mutator:
            def __reduce_ex__(self, protocol):
                dill.Pickler.dispatch[list] = save_list
                return Box, (list(range(200)),)
        with mock.patch.dict(dill.Pickler.dispatch, {}):
            baseline = dill.loads(dill.dumps(Mutator()))
        with mock.patch.dict(dill.Pickler.dispatch, {}):
            restored = dill.loads(self.serialize(Mutator()))
        self.assertIsInstance(baseline.values, tuple)
        self.assertEqual(restored.values, baseline.values)

    def test_custom_persistent_references_are_not_bypassed(self):
        rows = list(range(200))
        def persistent_id(pickler, value):
            return "SYNTHETIC-ROWS" if value is rows else None
        class Reader(dill.Unpickler):
            def persistent_load(self, reference):
                self.assert_reference(reference)
                return ("persistent-reference",)
            def assert_reference(self, reference):
                if reference != "SYNTHETIC-ROWS":
                    raise AssertionError(reference)
        with mock.patch.object(dill.Pickler, "persistent_id", persistent_id):
            baseline = Reader(io.BytesIO(dill.dumps(Box(rows)))).load()
            restored = Reader(io.BytesIO(self.serialize(Box(rows)))).load()
            direct = Reader(io.BytesIO(self.serialize(rows))).load()
        self.assertEqual(restored.values, baseline.values)
        self.assertEqual(direct, ("persistent-reference",))

    def test_custom_reducer_override_is_not_bypassed(self):
        rows = list(range(200))
        def reducer_override(pickler, value):
            return (tuple, (tuple(value),)) if value is rows else NotImplemented
        with mock.patch.object(dill.Pickler, "reducer_override", reducer_override, create=True):
            baseline = dill.loads(dill.dumps(Box(rows)))
            restored = dill.loads(self.serialize(Box(rows)))
            direct = dill.loads(self.serialize(rows))
        self.assertIsInstance(baseline.values, tuple)
        self.assertEqual(restored.values, baseline.values)
        self.assertEqual(direct, tuple(rows))

    def test_registered_custom_reducer_and_dynamic_class_behaviour_survive(self):
        class Dynamic:
            def __init__(self, values):
                self.values = values
            def total(self):
                return sum(self.values)
        restored = dill.loads(self.serialize(Dynamic(list(range(200)))))
        self.assertEqual(restored.total(), sum(range(200)))
        with mock.patch.dict(copyreg.dispatch_table, {Box: lambda value: (Box, (["reduced"],))}):
            restored = dill.loads(self.serialize(Box(list(range(200)))))
        self.assertEqual(restored.values, ["reduced"])

    def test_nested_module_namespace_keeps_dill_reference_semantics(self):
        module = types.ModuleType("snapshot_test_importable_module")
        module.value = ["live"]
        with mock.patch.dict(sys.modules, {module.__name__: module}):
            value = Box([0] * 200 + [module.__dict__])
            baseline = dill.loads(dill.dumps(value))
            restored = dill.loads(self.serialize(value))
            self.assertIs(baseline.values[-1], module.__dict__)
            self.assertIs(restored.values[-1], module.__dict__)

    def test_probe_boundary_validation_rejects_unknown_layout(self):
        output = io.BytesIO(b"\x80\x04\x95" + (999).to_bytes(8, "little") + b"N.")
        writer = snapshot_serializer._ProbeWriter(output, 1000)
        self.assertIsNone(snapshot_serializer._value_fragment(output, writer, 4))
        value = Box(list(range(200)))
        with mock.patch.object(snapshot_serializer, "_value_fragment", return_value=None):
            self.assertEqual(dill.loads(self.serialize(value)).values, value.values)

    def test_legacy_protocols_remain_supported(self):
        for protocol in (0, 1, 2):
            dill.settings["protocol"] = protocol
            values = list(range(200))
            self.assertEqual(dill.loads(self.serialize(Box(values))).values, values)

    def test_large_graph_caps_and_live_bash_handles_are_not_weakened(self):
        with self.assertRaises(repl._SnapshotSizeLimitExceeded):
            self.serialize(Box(list(range(10000))), cap=1000)
        handle = object.__new__(BashHandle)
        for snapshot_format in ("legacy", "cas-v2"):
            with tempfile.TemporaryDirectory() as root:
                result = repl._snapshot_state(
                    {"direct": handle, "nested": Box([0] * 200 + [handle]), "safe": Box(list(range(200)))},
                    os.path.join(root, "state.dill"), os.path.join(root, "state.json"),
                    1 << 20, 1 << 20, False, snapshot_format=snapshot_format,
                )
                self.assertNotIn("error", result)
                self.assertEqual(result["saved"], ["safe"])
                skipped = {item["name"]: item["reason"] for item in result["skipped"]}
                self.assertIn("runtime-owned process handle", skipped["direct"])
                self.assertIn("runtime-owned process handle", skipped["nested"])

    def test_random_builtin_subgraphs_inside_custom_objects_round_trip(self):
        rng = random.Random(20260918)
        for _ in range(25):
            pool = [{"n": rng.randrange(100000), "bytes": rng.randbytes(12)} for _ in range(200)]
            rows = [rng.choice(pool) for _ in range(1000)]
            restored = dill.loads(self.serialize(Box(rows)))
            self.assertEqual(restored.values, rows)
            for original in pool:
                indexes = [i for i, item in enumerate(rows) if item is original]
                if indexes:
                    self.assertTrue(all(restored.values[index] is restored.values[indexes[0]] for index in indexes))

    def test_serialization_metrics_are_numeric_and_account_for_final_retained_names(self):
        metrics = snapshot_serializer.SnapshotSerializationMetrics()
        metrics.record("private_document_name", 120_000_000)
        metrics.record("secret_variable", 150_000_000)
        metrics.record("retained", 10_000_000)
        summary = metrics.summarize(["private_document_name", "retained"])
        self.assertEqual(summary, {
            "serialization_max_variable_ms": 150,
            "serialization_slow_variables": 2,
            "serialization_saved_ms": 130,
            "serialization_skipped_ms": 150,
        })
        self.assertNotIn("private_document_name", repr(summary))
        self.assertNotIn("secret_variable", repr(summary))
        for snapshot_format in ("legacy", "cas-v2"):
            with tempfile.TemporaryDirectory() as root:
                result = repl._snapshot_state(
                    {"safe": Box(list(range(200))), "over_cap": b"x" * 100000},
                    os.path.join(root, "state.dill"), os.path.join(root, "state.json"),
                    1 << 20, 10000, False, snapshot_format=snapshot_format,
                )
                self.assertNotIn("error", result)
                for key in summary:
                    self.assertGreaterEqual(result["metrics"][key], 0)
                self.assertIn("over_cap", [item["name"] for item in result["skipped"]])
                accounted = result["metrics"]["serialization_saved_ms"] + result["metrics"]["serialization_skipped_ms"]
                self.assertLessEqual(accounted, result["metrics"]["serialization_wall_ms"])

    def test_serializer_wrapper_does_not_retain_graph_until_cyclic_gc(self):
        enabled = gc.isenabled()
        gc.disable()
        try:
            value = Box(list(range(200)))
            reference = weakref.ref(value)
            self.serialize(value)
            del value
            self.assertIsNone(reference(), "completed pickler must release its snapshot memo immediately")
        finally:
            if enabled:
                gc.enable()


if __name__ == "__main__":
    unittest.main()
