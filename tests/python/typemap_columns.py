"""The type-truth probe of an engine whose types are a column's: MySQL, SQLite, and psycopg's own PostgreSQL types.

None of them casts a value to every type it stores, so each mapped type has a nullable column in the engine's example
(a Rust spec holds this), and that schema cannot have a column of a type outside the map or it would not generate. So
its columns are the map. Each edge of a column's type is stored by the database itself, alone in its row, and read
through every checked function of the case (``deep.checked``), so what sqlc types otherwise than a column (a comparison,
a cast) is held to its annotation as well. It is then bound back through the generated insert, and when the database
does not hold what it held before, the edge is **lossy** (MySQL prints a ``FLOAT`` to six digits): a property of the
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

    Value = TypeVar("Value")

    # Runs one statement without parameters, as the driver takes it, and returns its rows.
    Run = Callable[[str], Awaitable[list[tuple[Any, ...]]]]
    # A connection as generated code takes it, and `Run` on that connection.
    Session = AbstractAsyncContextManager[tuple[object, Run]]

# Every example has the same table of every type, with a generated query that reads its rows up to a limit and one that
# inserts a row.
CLEAR = "DELETE FROM scalars"
STORE = "INSERT INTO scalars ({}) VALUES ({})"
READ = "list_scalars"
WRITE = "add_scalars"


@dataclass(frozen=True)
class Driver:
    name: str  # As the document names it
    case: str  # The golden case whose generated code runs on this driver
    connect: Callable[[sqlalchemy.URL], Session]


@dataclass(frozen=True)
class Engine:
    example: str
    drivers: tuple[Driver, ...]
    edges: Mapping[str, tuple[str, ...]]  # SQL expressions at the edges of each declared type, as `columns` spells it
    setup: tuple[str, ...]  # The statements each session starts with
    columns: str  # Selects each column of the table in order, as its name and declared type
    # Selects what the row's column `{}` holds, as the database's own text that tells any two values apart. We show the
    # first value it selects.
    held: str


def probe(engine: Engine, url: sqlalchemy.URL) -> list[Probe]:
    probes: dict[str, Probe] = {}
    for driver in engine.drivers:
        asyncio.run(_drive(engine, driver, url, probes))
    return list(probes.values())


async def _drive(engine: Engine, driver: Driver, url: sqlalchemy.URL, probes: dict[str, Probe]) -> None:
    (case,) = (c for c in cases(engine.example) if c.name == driver.case)
    called: set[str] = set()
    async with driver.connect(url) as (connection, run):
        for statement in engine.setup:
            await run(statement)

        api = queries(case, connection, called)
        hints = typing.get_type_hints(api.models.Scalar)
        columns = [(str(name), str(declared)) for name, declared in await run(engine.columns)]
        if [name for name, _ in columns] != list(hints):
            message = f"{case.name}: its model does not read the table of every type"
            raise LookupError(message)

        # We read each edge through every other query that reads the table too.
        readers = [
            name
            for name, function in public_functions(case, "query").items()
            if len(inspect.signature(function).parameters) == 1
        ]
        stored = _Stored(engine, api.query, run, [name for name, _ in columns], readers)
        for column, declared in columns:
            annotation = strip_none(hints[column])[0]
            probe = probes.setdefault(declared, Probe(declared, annotation))
            probe.seen[driver.name] = set()
            if probe.annotation != annotation:
                wrong = f"MISMATCH: annotated {annotation} here, {probe.annotation} elsewhere"
                probe.notes["annotation"][driver.name] = wrong
            if declared not in engine.edges:
                probe.notes["-"][driver.name] = "MISMATCH: no edge values to probe"
            for edge in ("NULL", *engine.edges.get(declared, ())):
                if (note := await stored.edge(probe.seen[driver.name], column, edge)) != "":
                    probe.notes[edge][driver.name] = note

    if len(uncalled := functions(case) - called) > 0:
        message = f"{case.name}: never called: {sorted(uncalled)}"
        raise LookupError(message)


@dataclass(frozen=True)
class _Stored:
    """The table on one connection: the case's checked queries of it, and the database's own word on what it holds."""

    engine: Engine
    query: Any
    run: Run
    columns: list[str]
    readers: list[str]

    async def edge(self, seen: set[str], column: str, edge: str) -> str:
        """Store one edge, read it and bind it back, and return what was notable (empty when nothing was)."""
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
        _, note = await _observed(getattr(self.query, WRITE)(**row), "binding back raises")
        back = await self.run(self.engine.held.format(column))
        if note != "" or back == stored:
            return note
        return f"lossy: {short(stored[0][0])} comes back as {short(back[0][0])}"

    async def _read(self) -> list[Any]:
        for name in self.readers:
            await getattr(self.query, name)()

        # We ask for more than the one row there is, through the `integer` sqlc types a `LIMIT` with.
        rows: list[Any] = await getattr(self.query, READ)(limit=2)
        return rows


async def _observed(call: Awaitable[Value], raising: str) -> tuple[Value | None, str]:
    """Await a checked call of generated code, and say how it failed: a mismatch on a value outside its annotation, or
    whatever its driver raised.
    """
    try:
        return await call, ""
    except AssertionError as wrong:
        return None, f"MISMATCH: {wrong}"
    except Exception as error:  # noqa: BLE001 -- what a driver raises is the observation
        return None, f"{raising} {raised(error)}"


def sqlalchemy_sync(drivername: str) -> Callable[[sqlalchemy.URL], Session]:

    @asynccontextmanager
    async def connect(url: sqlalchemy.URL) -> AsyncIterator[tuple[object, Run]]:
        engine = sqlalchemy.create_engine(url.set(drivername=drivername))
        try:
            with engine.connect() as connection:

                async def run(sql: str) -> list[tuple[Any, ...]]:
                    result = connection.exec_driver_sql(sql)
                    return [tuple(row) for row in result] if result.returns_rows else []

                yield connection, run
        finally:
            engine.dispose()

    return connect


def sqlalchemy_async(drivername: str) -> Callable[[sqlalchemy.URL], Session]:

    @asynccontextmanager
    async def connect(url: sqlalchemy.URL) -> AsyncIterator[tuple[object, Run]]:
        engine = create_async_engine(url.set(drivername=drivername))
        try:
            async with engine.connect() as connection:

                async def run(sql: str) -> list[tuple[Any, ...]]:
                    result = await connection.exec_driver_sql(sql)
                    return [tuple(row) for row in result] if result.returns_rows else []

                yield connection, run
        finally:
            await engine.dispose()

    return connect
