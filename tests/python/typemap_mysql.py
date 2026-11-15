"""MySQL's part of the type-truth probe: every type in the generator's map
(``TYPES`` in ``src/engine/mysql/types.rs``), a column of
``examples/mysqltypes``, at its edges, through PyMySQL and asyncmy as the
``pymysql`` backend reads them, asyncmy also with the statements it prepares
on the server when asked to, and through SQLAlchemy on either
(``typemap_columns.py``).
"""

from __future__ import annotations

from contextlib import asynccontextmanager
from typing import TYPE_CHECKING, Any

import mysql
import typemap_columns
from examples import EXAMPLES
from typemap_columns import Driver, Engine, sqlalchemy_async, sqlalchemy_sync
from typemap_probe import Section, release, versions

if TYPE_CHECKING:
    from collections.abc import AsyncIterator, Callable

    import sqlalchemy

    from typemap_columns import Session

SCHEMA = EXAMPLES / "mysqltypes/schema.sql"
# The session's time zone. Not UTC, which would hide what a driver does with it.
TIME_ZONE = "+05:30"

# Every byte, as a literal.
BYTES = f"x'{bytes(range(256)).hex()}'"

# SQL expressions at the edges of each type's domain, by the type as MySQL
# spells it (`information_schema.columns.column_type`). None holds a `%`,
# which a driver would format.
EDGES: dict[str, tuple[str, ...]] = {
    "tinyint(1)": ("TRUE", "FALSE", "2", "-128"),
    "tinyint": ("-128", "127"),
    "tinyint unsigned": ("0", "255"),
    "smallint": ("-32768", "32767"),
    "smallint unsigned": ("65535",),
    "mediumint": ("-8388608", "8388607"),
    "mediumint unsigned": ("16777215",),
    "int": ("-2147483648", "2147483647"),
    "int unsigned": ("4294967295",),
    "bigint": ("-9223372036854775808", "9223372036854775807"),
    "bigint unsigned": ("18446744073709551615",),
    "year": ("0", "1901", "2155"),
    "float": ("0.1", "-0.0", "3.402823466e38", "1.175494351e-38", "16777217"),
    "double": ("0.1", "-0.0", "1.7976931348623157e308", "2.2250738585072014e-308",
               "5e-324"),
    "decimal(65,30)": ("0", "2.500", "-0.000000000000000000000000000001",
                       "99999999999999999999999999999999999.999999999999999999999999999999"),
    "char(3)": ("''", "'ab '", "'é🧾'"),
    "varchar(20)": ("''", "'ab '", "'zażółć gęślą jaźń 🧾'"),
    "tinytext": ("''", "REPEAT('é', 127)"),
    "text": ("''", r"'tab\t, newline\n, quote '', backslash \\, NUL \0'",
             "REPEAT('x', 65535)"),
    "mediumtext": ("''", "REPEAT('é', 100000)"),
    "longtext": ("''", "REPEAT('🧾', 100000)"),
    "json": ("'null'", "'[]'", "'\"ü\"'", "'{\"b\": 1, \"a\": [1, 2.5, true, null]}'",
             "'123456789012345678901234567890'"),
    "binary(3)": ("''", "x'00ff'"),
    "varbinary(20)": ("''", "x'00ff'"),
    "tinyblob": ("''", "x'00ff'"),
    "blob": ("''", BYTES),
    "mediumblob": ("''", "REPEAT(x'00ff', 50000)"),
    "longblob": ("''", "REPEAT(x'00ff', 50000)"),
    "bit(1)": ("b'0'", "b'1'"),
    "bit(64)": ("b'0'", "b'1'", "18446744073709551615"),
    "date": ("'0001-01-01'", "'1000-01-01'", "'9999-12-31'", "'2000-02-29'"),
    "datetime(6)": ("'1000-01-01 00:00:00.000000'", "'9999-12-31 23:59:59.999999'"),
    "timestamp(6)": ("FROM_UNIXTIME(1)", "FROM_UNIXTIME(2147483647.999999)",
                     "'2024-03-31 01:30:00.123456'"),
    "time(6)": ("'00:00:00'", "'-838:59:59.000000'", "'838:59:59.000000'",
                "'00:00:00.000001'"),
}  # fmt: skip


@asynccontextmanager
async def pymysql(url: sqlalchemy.URL) -> AsyncIterator[tuple[object, Any]]:
    """A session on a PyMySQL connection, as the ``pymysql`` backend takes one."""
    conn = mysql.connect(url)
    try:

        async def run(sql: str) -> list[tuple[Any, ...]]:
            with conn.cursor() as cur:
                cur.execute(sql)
                return list(cur.fetchall())

        yield conn, run
    finally:
        conn.close()


def asyncmy(prepared: int) -> Callable[[sqlalchemy.URL], Session]:
    """A session on an asyncmy connection, as the ``pymysql`` backend's
    coroutines take one, which keeps up to ``prepared`` statements prepared
    on the server: the values of one cross as binary, not as text.
    """

    @asynccontextmanager
    async def connect(url: sqlalchemy.URL) -> AsyncIterator[tuple[object, Any]]:
        conn = await mysql.connect_async(url, stmt_cache_size=prepared)
        try:

            async def run(sql: str) -> list[tuple[Any, ...]]:
                async with conn.cursor() as cur:
                    await cur.execute(sql)
                    return list(await cur.fetchall())

            yield conn, run
        finally:
            await conn.ensure_closed()

    return connect


ENGINE = Engine(
    example="mysqltypes",
    drivers=(
        Driver("PyMySQL", "mysqltypes_pymysql", pymysql),
        Driver("asyncmy", "mysqltypes_pymysql_async", asyncmy(prepared=0)),
        Driver(
            "asyncmy, prepared",
            "mysqltypes_pymysql_async",
            asyncmy(prepared=8),
        ),
        Driver(
            "SQLAlchemy, PyMySQL",
            "mysqltypes_sqlalchemy",
            sqlalchemy_sync("mysql+pymysql"),
        ),
        Driver(
            "SQLAlchemy, asyncmy",
            "mysqltypes_sqlalchemy_async",
            sqlalchemy_async("mysql+asyncmy"),
        ),
    ),
    edges=EDGES,
    setup=(f"SET time_zone = '{TIME_ZONE}'",),
    columns=(
        "SELECT column_name, column_type FROM information_schema.columns"
        " WHERE table_schema = DATABASE() AND table_name = 'scalars'"
        " ORDER BY ordinal_position"
    ),
    # JSON holds any value exactly, and legibly: a `FLOAT` as the double it
    # is, bytes as base64.
    held="SELECT JSON_UNQUOTE(JSON_EXTRACT(JSON_ARRAY({}), '$[0]')) FROM scalars",
)


def probe(server: sqlalchemy.URL) -> Section:
    """Probe every type of ``examples/mysqltypes`` on ``server``."""
    with mysql.database(server, SCHEMA.read_text()) as url:
        probes = typemap_columns.probe(ENGINE, url)
    return Section(
        "MySQL",
        f"MySQL {release(mysql.version(server))}, session time zone {TIME_ZONE}, with"
        f" {versions('pymysql', 'asyncmy')}",
        tuple(driver.name for driver in ENGINE.drivers),
        probes,
    )
