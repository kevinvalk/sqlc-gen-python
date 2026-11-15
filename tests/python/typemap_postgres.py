"""PostgreSQL's part of the type-truth probe: each mapped type, at its edges,
per driver.

Every type in the generator's map (``TYPES`` in
``src/engine/postgresql/types.rs``) has a column in ``examples/pgtypes``,
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
"""

from __future__ import annotations

import asyncio
import importlib
import json
import typing
from typing import Any

import sqlalchemy
from sqlalchemy.ext.asyncio import create_async_engine

import postgres
from examples import EXAMPLES
from pgvalues import Pg, json_type, sqlalchemy_type, tables
from postgres import aw, database
from suite.deep import failure, strip_none
from typemap_probe import Probe, Section, describe, raised, short, versions

SCHEMA = EXAMPLES / "pgtypes/schema.sql"
# The generated models of that schema, which the probe reads every type through.
PGTYPES = "pgtypes_sqlalchemy"
# Each driver as the document names it, and as SQLAlchemy does.
DRIVERS = {"psycopg": "psycopg", "psycopg async": "psycopg_async", "asyncpg": "asyncpg"}
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
    "hstore": ("''", "'a=>1, b=>NULL'", "'\"k y\"=>\"ü\"'"),
    "ltree": ("''", "'a.b.c'", "'Top.Science_1'"),
    "regclass": ("'pg_class'", "'pg_catalog.pg_type'"),
    "int4range": ("'empty'", "'[1,5)'", "'[1,5]'", "'(,5)'", "'[1,)'", "'(,)'",
                  "'[-2147483648,2147483647)'"),
    "int8range": ("'empty'", "'[1,5)'", "'(,)'", "'[-9223372036854775808,9223372036854775807)'"),
    "numrange": ("'empty'", "'[1.50,2.500)'", "'(,0]'", "'[-1e30,1e30]'"),
    "daterange": ("'empty'", "'[2024-01-01,2024-02-01)'", "'[0001-01-01,9999-12-31]'",
                  "'(,infinity)'"),
    "tsrange": ("'empty'", "'[2024-01-01 00:00,2024-01-02 00:00)'", "'(,)'"),
    "tstzrange": ("'empty'", "'[2024-01-01 00:00+00,2024-01-02 00:00+05:30)'", "'[2024-01-01,)'"),
}  # fmt: skip


# A type as PostgreSQL knows it, and what the probe records of it.
Typed = tuple[Pg, Probe]


def spelt(pg: Pg) -> str:
    """The type as SQL names it: ``int4[]``."""
    return pg.name + "[]" * pg.dims


def probe(server: sqlalchemy.URL) -> Section:
    """Probe every type of a column of the generated pgtypes models, at every
    array depth a column has. Enums are the runtime suite's.
    """
    models = importlib.import_module(f"{PGTYPES}.models")
    with database(server, SCHEMA.read_text()) as url:
        engine = sqlalchemy.create_engine(url.set(drivername="postgresql+psycopg"))
        with engine.connect() as conn:
            # For every connection after this one.
            conn.exec_driver_sql(
                f"ALTER DATABASE {url.database} SET timezone = '{TIMEZONE}'"
            )
            conn.commit()
            annotations: dict[tuple[str, int], object] = {}
            for t in tables(conn, models):
                if t.model is None:
                    # Its types would otherwise go unprobed, and the verdict stay ok.
                    message = f"{PGTYPES}: no one model reads the table {t.sql}"
                    raise LookupError(message)
                hints = typing.get_type_hints(t.model).values()
                for column, hint in zip(t.columns, hints, strict=True):
                    if column.pg.labels:
                        continue
                    key = (column.pg.name, column.pg.dims)
                    annotation = strip_none(hint)[0]
                    if annotations.setdefault(key, annotation) != annotation:
                        message = f"{PGTYPES}: {column.pg.name} is annotated two ways"
                        raise LookupError(message)
        engine.dispose()
        probes = [
            (pg, Probe(spelt(pg), annotation))
            for pg, annotation in (
                (Pg(name, dims), annotation)
                for (name, dims), annotation in sorted(annotations.items())
            )
        ]
        for driver in DRIVERS:
            asyncio.run(_drive(url, driver, probes))
        # Only the server's major version, so that a patch release of the
        # image does not change the document.
        major = postgres.version(server).split(".", maxsplit=1)[0]
        return Section(
            "PostgreSQL",
            f"PostgreSQL {major}, session TimeZone {TIMEZONE}, with"
            f" {versions('psycopg', 'asyncpg')}",
            tuple(DRIVERS),
            [p for _, p in probes],
        )


async def _drive(url: sqlalchemy.URL, driver: str, probes: list[Typed]) -> None:
    url = url.set(drivername=f"postgresql+{DRIVERS[driver]}")
    is_async = url.drivername != "postgresql+psycopg"
    engine: Any = (
        create_async_engine(url) if is_async else sqlalchemy.create_engine(url)
    )
    try:
        conn = await aw(engine.connect())
        for pg, p in probes:
            p.seen[driver] = set()
            if pg.name not in EDGES:
                p.notes["-"][driver] = "MISMATCH: no edge values to probe"
            for edge, sql in expressions(pg):
                if note := await _edge(conn, (pg, p), driver, edge, sql):
                    p.notes[edge][driver] = note
        await aw(conn.close())
    finally:
        await aw(engine.dispose())


def expressions(pg: Pg) -> list[tuple[str, str]]:
    """Each edge of a type, and the SQL that selects it: alone, or in an
    array as deep as the type's (NULL and empty arrays too, but never a NULL
    item: see "Known limitations" in docs/python-output.md).
    """

    def nested(sql: str) -> str:
        for _ in range(pg.dims):
            sql = f"ARRAY[{sql}]"
        return sql

    # A `\:` keeps SQLAlchemy from reading a colon in a literal as a bind.
    edges = [
        (edge, nested(f"({edge.replace(':', chr(92) + ':')})::{pg.name}"))
        for edge in EDGES.get(pg.name, ())
    ]
    empty = [("{}", f"'{{}}'::{spelt(pg)}")] if pg.dims else []
    return [("NULL", f"NULL::{spelt(pg)}"), *empty, *edges]


async def _edge(conn: Any, probed: Typed, driver: str, edge: str, sql: str) -> str:
    """Select one edge, check it, bind it back; say what was notable."""
    pg, p = probed
    # JSON is also selected as the text of the document it holds.
    is_json = pg.name in {"json", "jsonb"}
    columns = f"{sql}, to_json({sql})::text" if is_json else sql
    transaction = await aw(conn.begin())
    try:
        # A range is read through SQLAlchemy's type, as the generated code
        # does (§6, typed statements).
        text = sqlalchemy.text(f"SELECT {columns}")
        ranged = sqlalchemy_type(pg)
        typed = text if ranged is None else text.columns(sqlalchemy.column("v", ranged))
        try:
            result = await aw(conn.execute(typed))
            value, *document = result.one()
        except Exception as error:  # noqa: BLE001 -- what a driver raises is the observation
            return f"raises {raised(error)}"
        if edge == "NULL":
            return "" if value is None else f"MISMATCH: NULL read as {value!r}"
        if value is not None:
            p.seen[driver].add(describe(value))
        if wrong := failure(value, p.annotation) or _undecoded(value, document):
            return f"MISMATCH: {wrong}"
        return await _round_trip(conn, pg, sql, value)
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


async def _round_trip(conn: Any, pg: Pg, sql: str, value: object) -> str:
    """Bind ``value`` back; empty if PostgreSQL reads what it sent."""
    # JSON is compared as `jsonb`: `json` keeps the text it was given verbatim.
    own = spelt(pg)
    text = own.replace("json", "jsonb") if pg.name == "json" else own
    cast = sqlalchemy.text(
        f"SELECT ({sql})::{text}::text, CAST(:v AS {own})::{text}::text"
    )
    if pg.name in {"json", "jsonb"}:
        cast = cast.bindparams(sqlalchemy.bindparam("v", type_=json_type(pg)))
    elif (ranged := sqlalchemy_type(pg)) is not None:
        cast = cast.bindparams(sqlalchemy.bindparam("v", type_=ranged))
    try:
        sent, back = (await aw(conn.execute(cast, {"v": value}))).one()
    except Exception as error:  # noqa: BLE001 -- what a driver raises is the observation
        return f"binding back raises {raised(error)}"
    return "" if sent == back else f"lossy: {short(sent)} comes back as {short(back)}"
