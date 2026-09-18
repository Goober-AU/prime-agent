"""Native builtin subgraph snapshots, retaining dill's custom-object semantics."""

from __future__ import annotations

import copyreg
import datetime
import decimal
import io
import pickle
from typing import Any


class SnapshotSerializationMetrics:
    """Bounded per-save accounting; variable names never leave this object."""

    def __init__(self) -> None:
        self._durations_ns: dict[str, int] = {}

    def record(self, name: str, elapsed_ns: int) -> None:
        self._durations_ns[name] = max(0, elapsed_ns)

    def summarize(self, saved: Any) -> dict[str, float | int]:
        saved_names = set(saved)
        saved_ns = sum(elapsed for name, elapsed in self._durations_ns.items() if name in saved_names)
        total_ns = sum(self._durations_ns.values())
        return {
            "serialization_max_variable_ms": max(self._durations_ns.values(), default=0) / 1_000_000,
            "serialization_slow_variables": sum(elapsed >= 100_000_000 for elapsed in self._durations_ns.values()),
            "serialization_saved_ms": saved_ns / 1_000_000,
            "serialization_skipped_ms": (total_ns - saved_ns) / 1_000_000,
        }


class _RequiresDill(Exception):
    pass


_NATIVE_VALUE_TYPES = (
    datetime.date,
    datetime.datetime,
    datetime.time,
    datetime.timedelta,
    datetime.timezone,
    decimal.Decimal,
)


def _native_dispatch_compatible(dill: Any) -> bool:
    if dill.Pickler.persistent_id is not pickle._Pickler.persistent_id:
        return False
    if getattr(dill.Pickler, "reducer_override", None) is not None:
        return False
    expected = {
        type(None): pickle._Pickler.save_none,
        bool: pickle._Pickler.save_bool,
        int: pickle._Pickler.save_long,
        float: pickle._Pickler.save_float,
        bytes: pickle._Pickler.save_bytes,
        str: pickle._Pickler.save_str,
        list: pickle._Pickler.save_list,
        dict: dill._dill.save_module_dict,
        tuple: pickle._Pickler.save_tuple,
        set: pickle._Pickler.save_set,
        frozenset: pickle._Pickler.save_frozenset,
    }
    return all(dill.Pickler.dispatch.get(kind) is handler for kind, handler in expected.items())


class _PrimitivePickler(pickle.Pickler):
    def __init__(self, writer: Any, dill: Any, *, protocol: int | None = None) -> None:
        super().__init__(writer, protocol=dill.settings["protocol"] if protocol is None else protocol)
        self._dill = dill

    def reducer_override(self, value: Any) -> Any:
        # The C pickler bypasses this hook only for exact builtin primitives and
        # containers. Exact immutable standard-library values have the same
        # importable reducers under dill. Subclasses, custom tzinfo, closures and
        # overridden reducer registrations still fall back before invoking them.
        for trusted in _NATIVE_VALUE_TYPES:
            if type(value) is trusted or value is trusted:
                if self._dill.Pickler.dispatch.get(trusted) is not None or trusted in copyreg.dispatch_table:
                    raise _RequiresDill()
                return NotImplemented
        raise _RequiresDill()


class _ProbeLimitExceeded(Exception):
    pass


class _SubgraphPickler(_PrimitivePickler):
    def persistent_id(self, value: Any) -> None:
        # Unlike ordinary dictionaries, dill can serialize a module namespace
        # by reference. C pickle bypasses reducer_override for exact dicts.
        if type(value) is dict and "__name__" in value:
            raise _RequiresDill()
        return None


class _ProbeWriter:
    def __init__(self, buffer: io.BytesIO, limit: int) -> None:
        self.buffer = buffer
        self.limit = limit
        self.last_write_start = 0

    def write(self, data: bytes) -> int:
        if self.buffer.tell() + len(data) > self.limit:
            raise _ProbeLimitExceeded()
        self.last_write_start = self.buffer.tell()
        return self.buffer.write(data)


def _value_fragment(buffer: io.BytesIO, writer: _ProbeWriter, protocol: int) -> bytes | None:
    """Remove the native stream envelope without interpreting value opcodes."""
    data = buffer.getvalue()
    if data[:2] != bytes((pickle.PROTO[0], protocol)) or not data.endswith(pickle.STOP):
        return None
    if protocol >= 4:
        # CPython emits the final frame as one write (possibly after PROTO).
        # Validate its boundary rather than searching arbitrary user bytes for
        # FRAME/STOP. Unexpected writer layouts simply keep the dill path.
        start = max(2, writer.last_write_start)
        if data[start:start + 1] == pickle.FRAME:
            size = int.from_bytes(data[start + 1:start + 9], "little")
            if size != len(data) - start - 9 or size < 1:
                return None
            data = data[:start + 1] + (size - 1).to_bytes(8, "little") + data[start + 9:]
        elif len(data) - start >= 4:
            return None
    return data[2:-1]


def _dump_with_dill(dill: Any, value: Any, writer: Any) -> None:
    """Keep dill's graph/reducer semantics, accelerating only builtin subgraphs."""
    pickler = dill.Pickler(writer, protocol=dill.settings["protocol"])
    original_save = pickler.save
    # A custom dispatch table may change even primitive values inside a graph.
    can_accelerate = _native_dispatch_compatible(dill) and pickler.proto >= 3
    if not can_accelerate:
        pickler.dump(value)
        return

    def save(obj: Any, save_persistent_id: bool = True) -> None:
        nonlocal can_accelerate
        if not can_accelerate:
            original_save(obj, save_persistent_id=save_persistent_id)
            return
        kind = type(obj)
        if (
            can_accelerate
            and kind in (list, dict, tuple)
            and len(obj) >= 128
            and id(obj) not in pickler.memo
            and (kind is not dict or "__name__" not in obj)
            and _native_dispatch_compatible(dill)
        ):
            buffer = io.BytesIO()
            probe_writer = _ProbeWriter(buffer, max(0, getattr(writer, "_limit", 16 << 20)))
            native = _SubgraphPickler(
                probe_writer, dill, protocol=pickler.proto
            )
            native.memo = pickler.memo
            try:
                native.dump(obj)
            except (_RequiresDill, pickle.PicklingError, _ProbeLimitExceeded):
                # Do not repeatedly probe a mixed graph at every nested level.
                # Revert this value's remaining traversal to unwrapped dill.
                can_accelerate = False
                pickler.save = original_save
            else:
                fragment = _value_fragment(buffer, probe_writer, pickler.proto)
                if fragment is not None:
                    # Never nest FRAME opcodes. Both serializers share one memo,
                    # including aliases/cycles crossing into the custom graph.
                    if pickler.proto >= 4:
                        pickler.framer.end_framing()
                    pickler._file_write(fragment)
                    if pickler.proto >= 4:
                        pickler.framer.start_framing()
                    pickler.memo = native.memo.copy()
                    return
                can_accelerate = False
                pickler.save = original_save
        original_save(obj, save_persistent_id=save_persistent_id)

    pickler.save = save
    try:
        pickler.dump(value)
    finally:
        # The wrapper closes over this pickler and its memo. Do not retain the
        # entire snapshot graph until cyclic GC happens to run.
        del pickler.save


def dump_snapshot_value(dill: Any, value: Any, buffer: Any, writer: Any) -> None:
    """Serialize afresh on every save, including in-place mutations and cycles."""
    try:
        if not _native_dispatch_compatible(dill):
            raise _RequiresDill()
        _PrimitivePickler(writer, dill).dump(value)
    except (_RequiresDill, pickle.PicklingError):
        buffer.seek(0)
        buffer.truncate()
        writer.written = 0
        _dump_with_dill(dill, value, writer)
