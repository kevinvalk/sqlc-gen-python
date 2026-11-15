"""The values one example of the runtime sweep has seen (see ``autoprop``).

A lookup only finds a row when its argument is a value the database holds, and a row with a foreign key is only accepted
when it references one. So each example pools what it seeded and what its calls returned, and we pick later arguments
and foreign keys from that pool.
"""

from __future__ import annotations

import dataclasses
import typing
from collections import defaultdict
from typing import Any

import sqlalchemy
from hypothesis import strategies as st

from suite.deep import failure

# An index into what a ``Pool`` holds, taken modulo its length. What Hypothesis draws must NOT depend on the database:
# it records each example's draws, and calls an example flaky (``FlakyStrategyDefinition``) when the same draws meet
# another strategy, as ``sampled_from`` a pool that grew would be.
PICK = st.integers(0, 15)


class Pool:
    LIMIT = 16

    def __init__(self, tables: dict[Any, str]) -> None:
        self.tables = tables  # The table each model reads, so a returned model pools its values under its table too
        self.rows: defaultdict[str, list[dict[str, object]]] = defaultdict(list)
        self.values: defaultdict[object, list[object]] = defaultdict(list)

    def add(self, key: object, value: object) -> None:
        if value is not None and len(self.values[key]) < self.LIMIT:
            self.values[key].append(value)

    def harvest(self, value: object) -> None:
        """Pool a result: its fields by name, a model's also by its table's, and every value by type."""
        if dataclasses.is_dataclass(value) and not isinstance(value, type):
            table = self.tables.get(type(value), "")
            for field in dataclasses.fields(value):
                item = getattr(value, field.name)
                self.add(field.name.rstrip("_"), item)
                if table != "":
                    self.add(f"{table}.{field.name.rstrip('_')}", item)
                self.harvest(item)
        elif isinstance(value, list):
            for item in typing.cast("list[object]", value):
                self.harvest(item)
        elif value is not None and not isinstance(value, sqlalchemy.CursorResult):
            self.add(type(value), value)

    def fitting(self, keys: tuple[str, ...], hint: Any) -> list[object]:
        """Pooled values that fit ``hint``, under the first of ``keys`` that has any (the most precise first), or else
        of ``hint``'s type.
        """
        found = next((self.values[key] for key in keys if len(self.values[key]) > 0), [])
        if len(found) <= 0 and isinstance(hint, type):
            found = self.values[hint]
        return [value for value in found if failure(value, hint) is None]
