from __future__ import annotations

import datetime as dt
import importlib.metadata
import typing
from collections import defaultdict
from dataclasses import dataclass, field
from typing import Any


@dataclass
class Probe:
    type: str  # As its engine names it: ``int4[]``, ``bigint unsigned``
    annotation: Any
    # The Python types each driver returned, spelt as ``list[int]`` and ``int``.
    seen: dict[str, set[str]] = field(default_factory=dict)
    # What happened to each edge on each driver: ``raises ...``, ``lossy ...`` or ``MISMATCH ...``.
    notes: defaultdict[str, dict[str, str]] = field(default_factory=lambda: defaultdict(dict))

    @property
    def mismatches(self) -> list[str]:
        return [
            f"{self.type} {edge} on {driver}: {note}"
            for edge, by_driver in self.notes.items()
            for driver, note in by_driver.items()
            if note.startswith("MISMATCH")
        ]


@dataclass(frozen=True)
class Section:
    engine: str
    observed: str  # Where and with what: ``PostgreSQL 18, session TimeZone ..., with psycopg 3.3``
    drivers: tuple[str, ...]
    probes: list[Probe]

    @property
    def mismatches(self) -> list[str]:
        return [f"{self.engine}: {mismatch}" for probe in self.probes for mismatch in probe.mismatches]


def release(version: str) -> str:
    # The document names only the major and minor (``3.3`` of ``3.3.8``), so that a patch release does not change it.
    return ".".join(version.split(".")[:2])


def versions(*packages: str) -> str:
    return ", ".join(f"{name} {release(importlib.metadata.version(name))}" for name in packages)


def raised(error: Exception) -> str:
    # SQLAlchemy wraps the driver's error, and the driver's is the one worth naming.
    return type(getattr(error, "orig", None) or error).__name__


def cut(text: str, width: int) -> str:
    return text if len(text) <= width else text[: width - 3] + "..."


def short(text: str | None) -> str:
    return f"`{cut('NULL' if text is None else text, 32)}`"


def describe(value: object) -> str:
    """Spell a value's type as an annotation would (``list[int]``), plus an aware datetime's zone: ``datetime(UTC)``."""
    if isinstance(value, list):
        items = sorted({describe(item) for item in typing.cast("list[object]", value)})
        return f"list[{' | '.join(items)}]" if len(items) > 0 else "list"
    if isinstance(value, dt.datetime) and value.tzinfo is not None:
        return f"datetime({value.tzinfo})"
    return type(value).__name__
