"""The type-truth probe of an engine whose types are a column's: MySQL, SQLite.

Neither casts a value to every type it stores, so each mapped type has a
nullable column in the engine's example (a Rust spec holds this), and that
schema has no column of a type outside the map, or it would not generate. So
its columns are the map. For each golden case of the example, on each driver
it runs on, each edge of a column's type is:

- stored by the database itself, alone in its row, and read through the
  case's generated functions, each call checked (``deep.checked``): a value
  outside its annotation is a **mismatch**, and fails ``cargo xtask verify``.
  The example's other queries read the row too, so what sqlc types otherwise
  than a column (a comparison, a cast) is held to its annotation as well;
- bound back through the generated insert, as an application would write it,
  and the database must then hold what it stored before; when not, the edge
  is **lossy** (MySQL prints a ``FLOAT`` to six digits), a property of the
  database or the driver worth knowing, not a wrong annotation.
"""

from __future__ import annotations

import asyncio
import inspect
import typing
from contextlib import asynccontextmanager
from dataclasses import dataclass
from typing import TYPE_CHECKING, Any

import sqlalchemy
from sqlalchemy.ext.asyncio import create_async_engine

from suite.api import cases, functions, public_functions, queries
from suite.deep import strip_none
from typemap_probe import Probe, describe, raised, short

if TYPE_CHECKING:
    from collections.abc import AsyncIterator, Awaitable, Callable, Mapping
    from contextlib import AbstractAsyncContextManager
    from typing import TypeVar

    T = TypeVar("T")

    # Runs one statement on a connection as its driver takes it, without
    # parameters, and returns its rows.
    Run = Callable[[str], Awaitable[list[tuple[Any, ...]]]]
    # A connection as generated code takes it, and `Run` on it.
    Session = AbstractAsyncContextManager[tuple[object, Run]]

# Each example's table of every type: how the probe empties it and stores a
# value in one of its columns, and the generated queries that read its rows,
# up to a limit, and insert one.
CLEAR = "DELETE FROM scalars"
STORE = "INSERT INTO scalars ({}) VALUES ({})"
READ = "list_scalars"
WRITE = "add_scalars"


@dataclass(frozen=True)
class Driver:
    """A driver, as the document names it, the golden case whose generated
    code runs on it, and a session on the database at a URL.
    """

    name: str
    case: str
    connect: Callable[[sqlalchemy.URL], Session]


@dataclass(frozen=True)
class Engine:
    """An engine's example, its drivers, and how its database speaks."""

    example: str
    drivers: tuple[Driver, ...]
    # Declared type, as `columns` reports it -> SQL expressions at its edges.
    edges: Mapping[str, tuple[str, ...]]
    # What each session starts with.
    setup: tuple[str, ...]
    # Selects each column of the table, in order: its name and declared type.
    columns: str
    # Selects what the row's column `{}` holds, as text of the database's own
    # that tells any two values apart; the first is shown.
    held: str


def probe(engine: Engine, url: sqlalchemy.URL) -> list[Probe]:
    """Probe every column's type of ``engine``'s example on the database at
    ``url``, through each driver.
    """
    probes: dict[str, Probe] = {}
    for driver in engine.drivers:
        asyncio.run(_drive(engine, driver, url, probes))
    return list(probes.values())


async def _drive(
    engine: Engine, driver: Driver, url: sqlalchemy.URL, probes: dict[str, Probe]
) -> None:
    (case,) = (c for c in cases(engine.example) if c.name == driver.case)
    called: set[str] = set()
    async with driver.connect(url) as (conn, run):
        for statement in engine.setup:
            await run(statement)
        api = queries(case, conn, called)
        hints = typing.get_type_hints(api.models.Scalar)
        columns = [
            (str(name), str(declared)) for name, declared in await run(engine.columns)
        ]
        if [name for name, _ in columns] != list(hints):
            message = f"{case.name}: its model does not read the table of every type"
            raise LookupError(message)
        # Every other query that reads the table, which each edge is read
        # through too.
        readers = [
            name
            for name, function in public_functions(case, "query").items()
            if len(inspect.signature(function).parameters) == 1
        ]
        stored = _Stored(engine, api.query, run, [name for name, _ in columns], readers)
        for column, declared in columns:
            annotation = strip_none(hints[column])[0]
            p = probes.setdefault(declared, Probe(declared, annotation))
            p.seen[driver.name] = set()
            if p.annotation != annotation:
                wrong = (
                    f"MISMATCH: annotated {annotation} here, {p.annotation} elsewhere"
                )
                p.notes["annotation"][driver.name] = wrong
            if declared not in engine.edges:
                p.notes["-"][driver.name] = "MISMATCH: no edge values to probe"
            for edge in ("NULL", *engine.edges.get(declared, ())):
                if note := await stored.edge(p.seen[driver.name], column, edge):
                    p.notes[edge][driver.name] = note
    if uncalled := functions(case) - called:
        message = f"{case.name}: never called: {sorted(uncalled)}"
        raise LookupError(message)


@dataclass(frozen=True)
class _Stored:
    """The table on one connection: a case's generated queries of it, checked,
    and the database's own word on what it holds.
    """

    engine: Engine
    query: Any
    run: Run
    columns: list[str]
    readers: list[str]

    async def edge(self, seen: set[str], column: str, edge: str) -> str:
        """Store one edge, read it, bind it back; say what was notable."""
        await self.run(CLEAR)
        await self.run(STORE.format(column, edge))
        stored = await self.run(self.engine.held.format(column))
        rows, note = await _observed(self._read(), "raises")
        if rows is None:
            return note
        (row,) = rows
        value = getattr(row, column)
        if value is None or edge == "NULL":
            null = (value is None) == (edge == "NULL")
            return "" if null else f"MISMATCH: {edge} read as {value!r}"
        seen.add(describe(value))

        await self.run(CLEAR)
        row = dict.fromkeys(self.columns) | {column: value}
        _, note = await _observed(
            getattr(self.query, WRITE)(**row), "binding back raises"
        )
        back = await self.run(self.engine.held.format(column))
        if note or back == stored:
            return note
        return f"lossy: {short(stored[0][0])} comes back as {short(back[0][0])}"

    async def _read(self) -> list[Any]:
        """The table's rows, as the generated code reads them. Every other
        query of the table reads them too.
        """
        for name in self.readers:
            await getattr(self.query, name)()
        # More than the one row there is, through the `integer` sqlc types a
        # `LIMIT` with.
        rows: list[Any] = await getattr(self.query, READ)(limit=2)
        return rows


async def _observed(call: Awaitable[T], raising: str) -> tuple[T | None, str]:
    """What a checked call of generated code returned, or how it failed: with
    a value outside its annotation, a mismatch; or as its driver raised.
    """
    try:
        return await call, ""
    except AssertionError as wrong:
        return None, f"MISMATCH: {wrong}"
    except Exception as error:  # noqa: BLE001 -- what a driver raises is the observation
        return None, f"{raising} {raised(error)}"


def sqlalchemy_sync(drivername: str) -> Callable[[sqlalchemy.URL], Session]:
    """A session on SQLAlchemy's connection of a sync driver, by its name to
    SQLAlchemy: ``mysql+pymysql``.
    """

    @asynccontextmanager
    async def connect(url: sqlalchemy.URL) -> AsyncIterator[tuple[object, Run]]:
        engine = sqlalchemy.create_engine(url.set(drivername=drivername))
        try:
            with engine.connect() as conn:

                async def run(sql: str) -> list[tuple[Any, ...]]:
                    result = conn.exec_driver_sql(sql)
                    return [tuple(row) for row in result] if result.returns_rows else []

                yield conn, run
        finally:
            engine.dispose()

    return connect


def sqlalchemy_async(drivername: str) -> Callable[[sqlalchemy.URL], Session]:
    """The same, of an async driver."""

    @asynccontextmanager
    async def connect(url: sqlalchemy.URL) -> AsyncIterator[tuple[object, Run]]:
        engine = create_async_engine(url.set(drivername=drivername))
        try:
            async with engine.connect() as conn:

                async def run(sql: str) -> list[tuple[Any, ...]]:
                    result = await conn.exec_driver_sql(sql)
                    return [tuple(row) for row in result] if result.returns_rows else []

                yield conn, run
        finally:
            await engine.dispose()

    return connect
