"""What the type-truth probe records of a type, whatever its engine."""

from __future__ import annotations

import datetime as dt
import importlib.metadata
import typing
from collections import defaultdict
from dataclasses import dataclass, field
from typing import Any


@dataclass
class Probe:
    """One type, and what each driver did with it."""

    # As its engine names it: ``int4[]``, ``bigint unsigned``.
    type: str
    annotation: Any
    # Driver -> the Python types it returned, as ``list[int]`` and ``int``.
    seen: dict[str, set[str]] = field(default_factory=dict)
    # Edge -> driver -> what happened: ``raises ...``, ``lossy ...``, ``MISMATCH ...``.
    notes: defaultdict[str, dict[str, str]] = field(
        default_factory=lambda: defaultdict(dict)
    )

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
    """What the probe of one engine observed: its part of ``docs/typemap.md``."""

    engine: str
    # Where, and with what: ``PostgreSQL 18, session TimeZone ..., with psycopg 3.3``.
    observed: str
    drivers: tuple[str, ...]
    probes: list[Probe]

    @property
    def mismatches(self) -> list[str]:
        return [f"{self.engine}: {m}" for p in self.probes for m in p.mismatches]


def release(version: str) -> str:
    """A version's major and minor, ``3.3`` of ``3.3.8``: what the document
    names, so that a patch release does not change it.
    """
    return ".".join(version.split(".")[:2])


def versions(*packages: str) -> str:
    """Each package and the release installed: ``psycopg 3.3, asyncpg 0.32``."""
    return ", ".join(
        f"{name} {release(importlib.metadata.version(name))}" for name in packages
    )


def raised(error: Exception) -> str:
    """The driver's error under SQLAlchemy's, by name."""
    return type(getattr(error, "orig", None) or error).__name__


def cut(text: str, width: int) -> str:
    """``text``, cut to ``width`` where it is longer."""
    return text if len(text) <= width else text[: width - 3] + "..."


def short(text: str | None) -> str:
    """What a database holds, as it says it, in brief."""
    return f"`{cut('NULL' if text is None else text, 32)}`"


def describe(value: object) -> str:
    """A value's type as an annotation would spell it, ``list[int]``, and an
    aware datetime's zone: ``datetime(UTC)``.
    """
    if isinstance(value, list):
        items = sorted({describe(v) for v in typing.cast("list[object]", value)})
        return f"list[{' | '.join(items)}]" if items else "list"
    if isinstance(value, dt.datetime) and value.tzinfo is not None:
        return f"datetime({value.tzinfo})"
    return type(value).__name__
