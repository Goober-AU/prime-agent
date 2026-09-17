"""Fast primitive snapshots, retaining dill for all non-builtin objects."""

from __future__ import annotations

import pickle
from typing import Any


class _RequiresDill(Exception):
    pass


class _PrimitivePickler(pickle.Pickler):
    def reducer_override(self, value: Any) -> Any:
        # The C pickler bypasses this hook only for exact builtin primitives and
        # containers. Never invoke custom reducers or encode globals by reference:
        # dill must retain its closure, interactive-class and module semantics.
        raise _RequiresDill()


def dump_snapshot_value(dill: Any, value: Any, buffer: Any, writer: Any) -> None:
    """Serialize afresh on every save, including in-place mutations and cycles."""
    try:
        _PrimitivePickler(writer, protocol=dill.settings["protocol"]).dump(value)
    except _RequiresDill:
        buffer.seek(0)
        buffer.truncate()
        writer.written = 0
        dill.dump(value, writer)
