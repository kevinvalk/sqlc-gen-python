"""The type-truth probe: each mapped PostgreSQL type, at its edges, per driver.

Every type in the generator's map (``TYPES`` in
``src/engine/postgresql/types.rs``) has a column in ``tests/schemas/src/pgtypes``,
and an array column unless arrays of
it are an error (a Rust spec holds this), and that schema has no column of a
type outside the map, or it would not generate. So its columns are the map:
each type, at each array depth a column has, is probed with the annotation
the generator wrote for it. Then each of ``EDGES`` (NULL, the extremes, NaN and
infinities, empty and unicode values) is selected through psycopg, psycopg
async and asyncpg, alone and inside arrays as deep as a column has, and:

- the value must inhabit the annotation exactly (``deep.check``), or the
  driver must raise: a value outside the annotation is a **mismatch**, and
  fails ``cargo xtask verify``. JSON, annotated ``object``, which every value
  inhabits, must also be the decoded document;
- the value is bound back through the same driver, and PostgreSQL must read it
  as the value it sent; when not, the edge is **lossy** (``'1 mon'`` comes back
  as 30 days), a property of the driver worth knowing, not a wrong annotation.

The result is ``docs/typemap.md``.
"""

from __future__ import annotations

import asyncio
import datetime as dt
import importlib
import importlib.metadata
import json
import re
import sys
import typing
from collections import defaultdict
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

import sqlalchemy
from sqlalchemy.ext.asyncio import create_async_engine

import postgres
from pgvalues import Pg, json_type, tables
from postgres import aw, database
from suite.deep import failure, strip_none

ROOT = Path(__file__).resolve().parents[2]
SCHEMA = ROOT / "tests/schemas/src/pgtypes/schema.sql"
# The generated models of that schema, which the probe reads every type through.
PGTYPES = "pgtypes_sqlalchemy"
DRIVERS = ("psycopg", "psycopg_async", "asyncpg")
# The session's TimeZone. Not UTC, which would hide what a driver does with it.
TIMEZONE = "Europe/Amsterdam"

# SQL expressions at the edges of each type's domain.
EDGES: dict[str, tuple[str, ...]] = {
    "bool": ("true", "false"),
    "int2": ("-32768", "32767"),
    "int4": ("-2147483648", "2147483647"),
    "int8": ("-9223372036854775808", "9223372036854775807"),
    "oid": ("0", "4294967295"),
    "float4": ("'NaN'", "'Infinity'", "'-Infinity'", "'-0'", "'3.4028235e38'", "'1e-45'"),
    "float8": ("'NaN'", "'Infinity'", "'-Infinity'", "'-0'", "'1.7976931348623157e308'",
               "'5e-324'"),
    "numeric": ("'NaN'", "'Infinity'", "'-Infinity'", "'2.500'", "'-0.000000000000000000001'",
                "'123456789012345678901234567890.123456789'"),
    "money": ("'-92233720368547758.08'", "'92233720368547758.07'", "'0'"),
    "text": ("''", "'zażółć gęślą jaźń 🧾'", "E'tab\\t, newline\\n, quote '''",
             "repeat('x', 100000)"),
    "varchar": ("''", "'ünïcødé'"),
    "bpchar": ("''", "'ab  '"),
    "name": ("''", "repeat('é', 40)"),
    "citext": ("''", "'ÄÖü'"),
    "macaddr": ("'08:00:2b:01:02:03'", "'ff:ff:ff:ff:ff:ff'"),
    "macaddr8": ("'08:00:2b:01:02:03:04:05'",),
    "xml": ("''", "'<a>ü</a>'", "'text only'"),
    "tsvector": ("''", "'a fat cat'", "$$'quoted words':1A$$"),
    "bytea": ("''", r"'\x00ff'", ("(SELECT string_agg(set_byte('\\x00'::bytea, 0, i), "
              "''::bytea ORDER BY i) FROM generate_series(0, 255) i)")),
    "date": ("'0001-01-01'", "'9999-12-31'", "'2000-02-29'", "'infinity'", "'-infinity'",
             "'4713-01-01 BC'"),
    "time": ("'00:00'", "'23:59:59.999999'", "'24:00:00'"),
    "timetz": ("'00:00+15:59'", "'23:59:59.999999-15:59'", "'12:00:00+05:30:15'",
               "'24:00:00+00'"),
    "timestamp": ("'0001-01-01 00:00'", "'9999-12-31 23:59:59.999999'", "'infinity'",
                  "'-infinity'"),
    "timestamptz": ("'0001-01-01 00:00+00'", "'9999-12-31 23:59:59.999999+00'",
                    "'2026-03-29 02:30 Europe/Amsterdam'", "'infinity'", "'-infinity'"),
    "interval": ("'0'", "'1 microsecond'", "'1 mon'", "'1 year 2 mons 3 days 04:05:06.789'",
                 "'-178000000 years'"),
    "uuid": ("'00000000-0000-0000-0000-000000000000'",
             "'ffffffff-ffff-ffff-ffff-ffffffffffff'"),
    "json": ("'null'", "'[]'", "'\"ü\"'", "'{\"a\": [1, 2.5, true, null]}'",
             "'123456789012345678901234567890'", "'1e400'"),
    "jsonb": ("'null'", "'[]'", "'\"ü\"'", "'{\"a\": [1, 2.5, true, null]}'",
              "'123456789012345678901234567890'", "'1e400'"),
    "inet": ("'0.0.0.0'", "'255.255.255.255'", "'::'", "'::ffff:1.2.3.4'", "'10.0.0.1/8'",
             "'2001:db8::1/64'", "'192.168.0.0/24'"),
    "cidr": ("'0.0.0.0/0'", "'::/0'", "'10.0.0.0/8'", "'192.168.1.1/32'", "'2001:db8::/32'"),
}  # fmt: skip


@dataclass
class Probe:
    """One type, alone or as an array, and what each driver did with it."""

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


def probe(server: sqlalchemy.URL, package: str) -> list[Probe]:
    """Probe every type of a column of ``package``, the generated pgtypes
    models, at every array depth a column has. Enums are the runtime suite's.
    """
    models = importlib.import_module(f"{package}.models")
    with database(server, SCHEMA.read_text()) as url:
        engine = sqlalchemy.create_engine(url.set(drivername="postgresql+psycopg"))
        with engine.connect() as conn:
            # For every connection after this one.
            conn.exec_driver_sql(
                f"ALTER DATABASE {url.database} SET timezone = '{TIMEZONE}'"
            )
            conn.commit()
            annotations = {
                (column.pg.name, column.pg.dims): strip_none(hint)[0]
                for t in tables(conn, models)
                if t.model is not None
                for column, hint in zip(
                    t.columns, typing.get_type_hints(t.model).values(), strict=True
                )
                if not column.pg.labels
            }
        engine.dispose()
        probes = [
            Probe(name + "[]" * dims, annotation)
            for (name, dims), annotation in sorted(annotations.items())
        ]
        for driver in DRIVERS:
            asyncio.run(_drive(url.set(drivername=f"postgresql+{driver}"), probes))
        return probes


async def _drive(url: sqlalchemy.URL, probes: list[Probe]) -> None:
    is_async = url.drivername != "postgresql+psycopg"
    engine: Any = (
        create_async_engine(url) if is_async else sqlalchemy.create_engine(url)
    )
    driver = url.drivername.removeprefix("postgresql+")
    try:
        conn = await aw(engine.connect())
        for p in probes:
            p.seen[driver] = set()
            if p.type.split("[")[0] not in EDGES:
                p.notes["-"][driver] = "MISMATCH: no edge values to probe"
            for edge, sql in expressions(p.type):
                if note := await _edge(conn, p, driver, edge, sql):
                    p.notes[edge][driver] = note
        await aw(conn.close())
    finally:
        await aw(engine.dispose())


def expressions(pg_type: str) -> list[tuple[str, str]]:
    """Each edge of a type, and the SQL that selects it: alone, or in an
    array as deep as the type's (NULL and empty arrays too, but never a NULL
    item: see "Known limitations" in docs/python-output.md).
    """
    base = pg_type.split("[", maxsplit=1)[0]
    dims = pg_type.count("[]")

    def nested(sql: str) -> str:
        for _ in range(dims):
            sql = f"ARRAY[{sql}]"
        return sql

    # A `\:` keeps SQLAlchemy from reading a colon in a literal as a bind.
    edges = [
        (edge, nested(f"({edge.replace(':', chr(92) + ':')})::{base}"))
        for edge in EDGES.get(base, ())
    ]
    empty = [("{}", f"'{{}}'::{pg_type}")] if dims else []
    return [("NULL", f"NULL::{pg_type}"), *empty, *edges]


async def _edge(conn: Any, p: Probe, driver: str, edge: str, sql: str) -> str:  # noqa: ANN401
    """Select one edge, check it, bind it back; say what was notable."""
    # JSON is also selected as the text of the document it holds.
    is_json = p.type.split("[")[0] in {"json", "jsonb"}
    columns = f"{sql}, to_json({sql})::text" if is_json else sql
    transaction = await aw(conn.begin())
    try:
        try:
            result = await aw(conn.execute(sqlalchemy.text(f"SELECT {columns}")))
            value, *document = result.one()
        except Exception as error:  # noqa: BLE001 -- what a driver raises is the observation
            return f"raises {type(getattr(error, 'orig', None) or error).__name__}"
        if edge == "NULL":
            return "" if value is None else f"MISMATCH: NULL read as {value!r}"
        if value is not None:
            p.seen[driver].add(_describe(value))
        if wrong := failure(value, p.annotation) or _undecoded(value, document):
            return f"MISMATCH: {wrong}"
        return await _round_trip(conn, p, sql, value)
    finally:
        await aw(transaction.rollback())


def _undecoded(value: object, document: list[str]) -> str | None:
    """Say how a JSON value is not the ``document`` it holds, if it is not:
    ``object`` admits undecoded text as readily as the decoded document.
    """
    if not document:
        return None
    decoded = json.loads(document[0])
    # `1e400` decodes as infinity, which is equal by `repr` only.
    return None if repr(value) == repr(decoded) else f"{value!r} is not {decoded!r}"


async def _round_trip(conn: Any, p: Probe, sql: str, value: object) -> str:  # noqa: ANN401
    """Bind ``value`` back; empty if PostgreSQL reads what it sent."""
    base = p.type.split("[")[0]
    # JSON is compared as `jsonb`: `json` keeps the text it was given verbatim.
    text = p.type.replace("json", "jsonb") if base == "json" else p.type
    cast = sqlalchemy.text(
        f"SELECT ({sql})::{text}::text, CAST(:v AS {p.type})::{text}::text"
    )
    if base in {"json", "jsonb"}:
        cast = cast.bindparams(
            sqlalchemy.bindparam("v", type_=json_type(Pg(base, p.type.count("[]"))))
        )
    try:
        sent, back = (await aw(conn.execute(cast, {"v": value}))).one()
    except Exception as error:  # noqa: BLE001 -- what a driver raises is the observation
        return f"binding back raises {type(getattr(error, 'orig', None) or error).__name__}"
    return "" if sent == back else f"lossy: {_short(sent)} comes back as {_short(back)}"


def _short(text: str | None, width: int = 32) -> str:
    text = "NULL" if text is None else text
    return f"`{text if len(text) <= width else text[: width - 3] + '...'}`"


def _describe(value: object) -> str:
    """A value's type as an annotation would spell it, ``list[int]``, and an
    aware datetime's zone: ``datetime(UTC)``.
    """
    if isinstance(value, list):
        items = sorted({_describe(v) for v in typing.cast("list[object]", value)})
        return f"list[{' | '.join(items)}]" if items else "list"
    if isinstance(value, dt.datetime) and value.tzinfo is not None:
        return f"datetime({value.tzinfo})"
    return type(value).__name__


def render(probes: list[Probe], server_version: str, newer: str) -> str:
    """``docs/typemap.md``, observed on ``server_version``; ``newer`` says what
    else ``verify`` held the annotations to.

    Only the server's major version is recorded, so that a patch release of
    the image does not change the document.
    """
    versions = ", ".join(
        f"{name} {importlib.metadata.version(name)}"
        for name in ("psycopg", "asyncpg", "sqlalchemy")
    )
    python = ".".join(map(str, sys.version_info[:2]))
    major = server_version.split(".", maxsplit=1)[0]
    lines = [
        "# PostgreSQL types at runtime",
        "",
        "Generated by `cargo xtask verify` (`tests/py/typemap.py`); do not edit.",
        (
            f"Observed on PostgreSQL {major}, session TimeZone {TIMEZONE}, with"
            f" {versions} on Python {python}; `verify` holds every annotation on"
            f" {newer} too."
        ),
        "",
        (
            "Each type in the generator's map, with the annotation the generator"
            " writes for it, and the Python types each driver returned for NULL and"
            " the edge values in `typemap.py`. A **mismatch** (a value outside the"
            " annotation) fails `verify`. A driver that raises on an edge, or returns"
            " a value that does not bind back to the one it read (**lossy**), is"
            " listed under the table: these are properties of the drivers, which the"
            " annotation cannot express."
        ),
        "",
        "| PostgreSQL | Annotation | psycopg | psycopg async | asyncpg | Verdict |",
        "|---|---|---|---|---|---|",
    ]
    for p in probes:
        seen = [
            " ".join(f"`{t}`" for t in sorted(p.seen.get(d, ()))) or "-"
            for d in DRIVERS
        ]
        verdict = "**MISMATCH**" if p.mismatches else "ok"
        lines.append(
            _row(f"`{p.type}`", f"`{_annotation(p.annotation)}`", *seen, verdict)
        )
    lines += ["", "## Edges that raise or are lossy", ""]
    lines += [_row("PostgreSQL", "Edge", *DRIVERS), "|---|---|---|---|---|"]
    for p in probes:
        for edge, by_driver in p.notes.items():
            notes = [by_driver.get(d, "") for d in DRIVERS]
            lines.append(_row(f"`{p.type}`", f"`{edge}`", *notes))
    return "\n".join(lines) + "\n"


def _row(*cells: str) -> str:
    """A table row; a `|` in a cell, even in code, must be escaped."""
    return "| " + " | ".join(cell.replace("|", "\\|") for cell in cells) + " |"


def _annotation(hint: Any) -> str:  # noqa: ANN401
    if isinstance(hint, type) and not typing.get_args(hint):
        return hint.__name__
    return re.sub(r"\b(?:[a-z_]+\.)+", "", str(hint)).replace("NoneType", "None")


def main() -> int:
    """``typemap.py ROOT``: probe ``ROOT``'s pgtypes models; 1 on any
    mismatch.
    """
    sys.path.insert(0, sys.argv[1])
    with postgres.server() as server:
        probes = probe(server, PGTYPES)
    mismatches = [m for p in probes for m in p.mismatches]
    print(*mismatches, sep="\n")
    return int(bool(mismatches))


if __name__ == "__main__":
    sys.exit(main())
