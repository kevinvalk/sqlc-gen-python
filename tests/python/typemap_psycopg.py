"""The psycopg backend's part of the type-truth probe: every PostgreSQL type
it alone maps (``TEXT`` and the ranges of ``src/backend/psycopg.rs``), a
column of ``examples/pgtext``, at its edges, through psycopg and psycopg
async as the backend reads them (``typemap_columns.py``).

Every other type the backend maps is the engine's, which SQLAlchemy hands
through from the same driver: ``typemap_postgres.py`` probes those.
"""

from __future__ import annotations

from contextlib import asynccontextmanager
from typing import TYPE_CHECKING, Any

import psycopg

import postgres
import typemap_columns
from examples import EXAMPLES
from typemap_columns import Driver, Engine
from typemap_postgres import TIMEZONE
from typemap_probe import Section, versions

if TYPE_CHECKING:
    from collections.abc import AsyncIterator

    import sqlalchemy

SCHEMA = EXAMPLES / "pgtext/schema.sql"

# SQL expressions at the edges of each type's domain, by the type as
# PostgreSQL spells it (`format_type`).
EDGES: dict[str, tuple[str, ...]] = {
    "hstore": ("''", "'a=>1, b=>NULL'", "'\"a b\"=>\"c, d\", \"ü\"=>\"\\\\\"'"),
    "bit(3)": ("B'000'", "B'101'"),
    "bit varying": ("B''", "B'10'", "repeat('1', 100)::varbit"),
    "point": ("'(0,0)'", "'(1.5,-2)'", "'(Infinity,NaN)'"),
    "line": ("'{1,-1,0}'", "'[(0,0),(1,1)]'"),
    "lseg": ("'[(0,0),(1,1)]'", "'(1,2),(1,2)'"),
    "box": ("'(1,1),(0,0)'", "'(0,0),(0,0)'"),
    "path": ("'((0,0),(1,1))'", "'[(0,0),(1,1),(2,0)]'"),
    "polygon": ("'((0,0),(1,1),(1,0))'", "'(0,0)'"),
    "circle": ("'<(0,0),1>'", "'<(1,2),0>'"),
    "cid": ("'0'", "'4294967295'"),
    "xid": ("'0'", "'4294967295'"),
    "xid8": ("'0'", "'18446744073709551615'"),
    "tid": ("'(0,0)'", "'(4294967295,65535)'"),
    "pg_lsn": ("'0/0'", "'16/B374D848'", "'FFFFFFFF/FFFFFFFF'"),
    "int4range": ("'empty'", "'[1,10)'", "'(,)'", "'[-2147483648,2147483647)'"),
    "int8range": ("'empty'", "'[1,10)'", "'(,0]'",
                  "'[-9223372036854775808,9223372036854775807)'"),
    "numrange": ("'empty'", "'[1.50,2.500)'", "'(,0]'", "'[-1e30,1e30]'"),
    "daterange": ("'empty'", "'[2024-01-01,2024-02-01)'", "'[0001-01-01,9999-12-31]'",
                  "'(,infinity)'"),
    "tsrange": ("'empty'", "'[2024-01-01 00:00,2024-01-02 00:00)'", "'(,)'"),
    "tstzrange": ("'empty'", "'[2024-01-01 00:00+00,2024-01-02 00:00+05:30)'",
                  "'[2024-01-01,)'"),
}  # fmt: skip


# Each session commits every statement: one the probe expects to fail must
# not fail those after it.
@asynccontextmanager
async def sync(url: sqlalchemy.URL) -> AsyncIterator[tuple[object, Any]]:
    """A session on a psycopg connection, as the ``psycopg`` backend takes one."""
    with psycopg.connect(postgres.conninfo(url), autocommit=True) as conn:

        async def run(sql: str) -> list[tuple[Any, ...]]:
            cur = conn.execute(sql.encode())
            return cur.fetchall() if cur.description else []

        yield conn, run


@asynccontextmanager
async def asynchronous(url: sqlalchemy.URL) -> AsyncIterator[tuple[object, Any]]:
    """The same, of the backend's coroutines."""
    connecting = psycopg.AsyncConnection.connect(
        postgres.conninfo(url), autocommit=True
    )
    async with await connecting as conn:

        async def run(sql: str) -> list[tuple[Any, ...]]:
            cur = await conn.execute(sql.encode())
            return await cur.fetchall() if cur.description else []

        yield conn, run


ENGINE = Engine(
    example="pgtext",
    drivers=(
        Driver("psycopg", "pgtext_psycopg", sync),
        Driver("psycopg async", "pgtext_psycopg_async", asynchronous),
    ),
    edges=EDGES,
    setup=(f"SET TimeZone = '{TIMEZONE}'",),
    columns=(
        "SELECT attname, format_type(atttypid, atttypmod) FROM pg_attribute"
        " WHERE attrelid = 'scalars'::regclass AND attnum > 0 ORDER BY attnum"
    ),
    held="SELECT {}::text FROM scalars",
)


def probe(server: sqlalchemy.URL) -> Section:
    """Probe every type of ``examples/pgtext`` on ``server``."""
    with postgres.database(server, SCHEMA.read_text()) as url:
        probes = typemap_columns.probe(ENGINE, url)
    # As `typemap_postgres` names the server.
    major = postgres.version(server).split(".", maxsplit=1)[0]
    return Section(
        "psycopg-only PostgreSQL",
        f"PostgreSQL {major}, session TimeZone {TIMEZONE}, with {versions('psycopg')}",
        tuple(driver.name for driver in ENGINE.drivers),
        probes,
    )
