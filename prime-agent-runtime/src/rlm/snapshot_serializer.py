"""Fast builtin/date/decimal snapshots, retaining dill for custom objects."""

from __future__ import annotations

import copyreg
import datetime
import decimal
import pickle
from typing import Any


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


class _PrimitivePickler(pickle.Pickler):
    def __init__(self, writer: Any, dill: Any) -> None:
        super().__init__(writer, protocol=dill.settings["protocol"])
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


def dump_snapshot_value(dill: Any, value: Any, buffer: Any, writer: Any) -> None:
    """Serialize afresh on every save, including in-place mutations and cycles."""
    try:
        _PrimitivePickler(writer, dill).dump(value)
    except (_RequiresDill, pickle.PicklingError):
        buffer.seek(0)
        buffer.truncate()
        writer.written = 0
        dill.dump(value, writer)
