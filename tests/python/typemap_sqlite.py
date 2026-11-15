"""SQLite's part of the type-truth probe: ``examples/sqlitetypes`` has a column of each affinity the generator maps, by
each of SQLite's rules for a declared type (``src/engine/sqlite/types.rs``), read through ``sqlite3`` and ``aiosqlite``
as the ``sqlite3`` backend reads them and through SQLAlchemy on each.

NOTE: SQLite stores a value of another class as it is when it cannot convert it, whatever the column's type. So we only
probe edges of the column's own class (and those its affinity converts), as that is what the generated code binds.
"""

from __future__ import annotations

import sqlite3
from contextlib import asynccontextmanager
from typing import TYPE_CHECKING, Any

import aiosqlite
import sqlalchemy

import typemap_columns
from examples import EXAMPLES, statements
from typemap_columns import Driver, Engine, sqlalchemy_async, sqlalchemy_sync
from typemap_probe import Section, versions

if TYPE_CHECKING:
    from collections.abc import AsyncIterator

SCHEMA = EXAMPLES / "sqlitetypes/schema.sql"

# SQL expressions at the edges of each class, plus those of another class that the column's affinity converts: `'12'` in
# an INTEGER column is the integer.
INTEGERS = ("0", "-9223372036854775808", "9223372036854775807", "'12'", "12.0")
TEXTS = ("''", "'zażółć gęślą jaźń 🧾'", "'a' || char(0) || 'b'", "12", "1.5")
REALS = ("0.0", "-0.0", "1.7976931348623157e308", "5e-324", "9e999", "-9e999",
         "12", "'1.5'")  # fmt: skip
BLOBS = ("x''", "x'00ff'", f"x'{bytes(range(256)).hex()}'", "zeroblob(100000)")
BOOLEANS = ("TRUE", "FALSE", "2")

# By the type as declared (`pragma_table_info`)
EDGES: dict[str, tuple[str, ...]] = {
    "INTEGER": INTEGERS,
    "BIGINT": INTEGERS,
    "TEXT": TEXTS,
    "VARCHAR(20)": TEXTS,
    "CLOB": TEXTS,
    "BLOB": BLOBS,
    "REAL": REALS,
    "FLOAT": REALS,
    "DOUBLE PRECISION": REALS,
    "BOOL": BOOLEANS,
    "BOOLEAN": BOOLEANS,
}


@asynccontextmanager
async def sync(_: sqlalchemy.URL) -> AsyncIterator[tuple[object, Any]]:
    connection = sqlite3.connect(":memory:")
    try:

        async def run(sql: str) -> list[tuple[Any, ...]]:
            return connection.execute(sql).fetchall()

        yield connection, run
    finally:
        connection.close()


@asynccontextmanager
async def asynchronous(_: sqlalchemy.URL) -> AsyncIterator[tuple[object, Any]]:
    async with aiosqlite.connect(":memory:") as connection:

        async def run(sql: str) -> list[tuple[Any, ...]]:
            return [tuple(row) for row in await connection.execute_fetchall(sql)]

        yield connection, run


ENGINE = Engine(
    example="sqlitetypes",
    drivers=(
        Driver("sqlite3", "sqlitetypes_sqlite3", sync),
        Driver("aiosqlite", "sqlitetypes_sqlite3_async", asynchronous),
        Driver(
            "SQLAlchemy, sqlite3",
            "sqlitetypes_sqlalchemy",
            sqlalchemy_sync("sqlite+pysqlite"),
        ),
        Driver(
            "SQLAlchemy, aiosqlite",
            "sqlitetypes_sqlalchemy_async",
            sqlalchemy_async("sqlite+aiosqlite"),
        ),
    ),
    edges=EDGES,
    # Each connection is a database of its own in memory, so every session creates the schema.
    setup=tuple(statements(SCHEMA.read_text())),
    columns="SELECT name, type FROM pragma_table_info('scalars')",
    # `quote` stops at a NUL in text, which `hex` does not, and neither says which class an integer and a real that
    # print alike are stored as.
    held="SELECT quote({0}), typeof({0}), hex({0}) FROM scalars",
)


def probe() -> Section:
    probes = typemap_columns.probe(ENGINE, sqlalchemy.make_url("sqlite://"))
    return Section(
        "SQLite",
        # Only the major version, as the build of each Python decides the rest.
        f"SQLite {sqlite3.sqlite_version_info[0]}, with {versions('aiosqlite')}",
        tuple(driver.name for driver in ENGINE.drivers),
        probes,
    )
