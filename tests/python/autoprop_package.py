"""The runtime sweep of one generated package (see ``autoprop``), which each worker of ``autoprop.sweep`` runs."""

from __future__ import annotations

import asyncio
import importlib
import json
import re
from collections import Counter
from dataclasses import dataclass, field
from pathlib import Path
from typing import TYPE_CHECKING, Any

import psycopg
import sqlalchemy
from hypothesis import HealthCheck, Phase, given, settings
from hypothesis import strategies as st
from sqlalchemy.ext.asyncio import create_async_engine

from autoprop_calls import calls, driver, labels
from autoprop_pool import Pool
from autoprop_refusals import refused
from autoprop_seeds import draw_row, insert, read
from pgvalues import tables
from postgres import SchemaRejectedError, aw, conninfo, database

if TYPE_CHECKING:
    from types import ModuleType

    from autoprop_calls import Json
    from pgvalues import Table

EXAMPLES = 100


@dataclass(frozen=True)
class Target:
    package: str
    request: Path
    schema: Path
    known: tuple[str, ...] = ()  # What the failures listed for it in `autoprop.KNOWN` say
    asynchronous: bool = False
    raw: bool = False  # Whether its functions take a psycopg connection (the psycopg backend)


@dataclass
class Report:
    target: Target

    # Why it failed, or could not run at all. A schema PostgreSQL rejects is perhaps not the generator's fault, but it
    # proves nothing either.
    failure: str = ""

    calls: dict[str, Counter[str]] = field(default_factory=dict)
    returning: set[str] = field(default_factory=set)  # Functions whose query returns rows, so empty checks nothing
    known: Counter[str] = field(default_factory=Counter)  # How often each of the target's known failures happened
    same: int = 0  # The calls that returned what sqlc's own SQL, run directly, did

    @property
    def exercised(self) -> list[str]:
        """The functions a call checked everything of (see ``autoprop``)."""
        return [
            name
            for name, outcomes in self.calls.items()
            if outcomes["rows"] > 0 or (name not in self.returning and outcomes["empty"] + outcomes["ran"] > 0)
        ]


def run(server: sqlalchemy.URL, target: Target) -> Report:
    report = Report(target)
    try:
        package = importlib.import_module(target.package)
        request: Json = json.loads(target.request.read_text())
        with database(server, target.schema.read_text()) as url:
            Sweep(package, request, url, report, target).run()
    except SchemaRejectedError as error:
        report.failure = str(error)
    except Exception as error:  # noqa: BLE001 -- a failure is a finding to report
        report.failure = describe(error)
    return report


def describe(error: BaseException) -> str:
    """An error, and the draws of the example Hypothesis found, in a few lines."""
    group = getattr(error, "exceptions", None)  # Hypothesis found several distinct failures
    if group is not None:
        return "\n  ".join(describe(member) for member in group)

    first = f"{type(error).__name__}: {error}".splitlines()[0]
    notes = [note for note in getattr(error, "__notes__", []) if note.startswith("Draw ")]
    return "\n    ".join([first, *notes])


class Sweep:
    def __init__(
        self,
        package: ModuleType,
        request: Json,
        url: sqlalchemy.URL,
        report: Report,
        target: Target,
    ) -> None:
        self.report = report
        self.asynchronous, self.raw = target.asynchronous, target.raw
        self.conninfo = conninfo(url)
        stems = {path.stem for path in Path(package.__path__[0]).glob("*.py")}
        modules = {stem: importlib.import_module(f"{package.__name__}.{stem}") for stem in sorted(stems - {"__init__"})}
        models = modules.pop("models", None)

        def queries(stem: str) -> list[Json]:
            return [query for query in request["queries"] if module_of(query["filename"]) == stem]

        oracle = sqlalchemy.create_engine(url.set(drivername="postgresql+psycopg"), isolation_level="AUTOCOMMIT")
        try:
            with oracle.connect() as connection:
                self.tables = tables(connection, models)
                self.enums = [
                    labels(driver(connection), schema["name"], enum["name"])
                    for schema in request["catalog"]["schemas"]
                    for enum in schema.get("enums", [])
                    if len(enum["vals"]) > 0  # An enum without labels has no value to read
                ]
                self.calls = [
                    call for stem, module in modules.items() for call in calls(connection, module, queries(stem))
                ]
        finally:
            oracle.dispose()

        missing = {module_of(query["filename"]) for query in request["queries"]} - set(modules)
        if len(missing) > 0:
            message = f"no module for {', '.join(sorted(missing))}"
            raise AssertionError(message)

        report.calls = {call.name: Counter() for call in self.calls}
        report.returning = {call.name for call in self.calls if call.query["cmd"] in {":one", ":many"}}
        # A psycopg package's coroutines take psycopg's own async connection.
        asyncpg = self.asynchronous and not self.raw
        self.url = url.set(drivername="postgresql+asyncpg" if asyncpg else "postgresql+psycopg")

    def run(self) -> None:
        """Run the examples on one connection, each in its own transaction."""
        loop = asyncio.new_event_loop()

        # SQLAlchemy would otherwise register psycopg's `hstore` loader on the very connection a psycopg package reads
        # from.
        native = {"use_native_hstore": False} if self.raw else {}
        engine: Any = (
            create_async_engine(self.url, **native)
            if self.asynchronous
            else sqlalchemy.create_engine(self.url, **native)
        )
        connection = loop.run_until_complete(aw(engine.connect()))
        callee = self.callee = connection
        if self.raw:
            fairy = (
                loop.run_until_complete(connection.get_raw_connection()) if self.asynchronous else connection.connection
            )
            callee = self.callee = fairy.driver_connection

        @settings(
            max_examples=EXAMPLES,
            deadline=None,
            database=None,
            derandomize=True,
            suppress_health_check=list(HealthCheck),
            phases=[Phase.generate, Phase.shrink],
        )
        @given(st.data())
        def examples(data: st.DataObject) -> None:
            loop.run_until_complete(self.example(connection, callee, data))

        try:
            examples()
        finally:
            loop.run_until_complete(aw(connection.close()))
            loop.run_until_complete(aw(engine.dispose()))
            loop.close()

    async def example(
        self,
        connection: Any,
        callee: Any,
        data: st.DataObject,
    ) -> None:
        """Seed every table and call every function on ``callee``, then roll back."""
        transaction = await aw(connection.begin())
        try:
            pool = Pool({table.model: table.name for table in self.tables if table.model is not None})
            for table in self.tables:
                for _ in range(data.draw(st.integers(0, 2), label=table.sql)):
                    await self.seed(connection, data, table, pool)

            for call in self.calls:
                # We only compare the calls of functions, as a coroutine runs the same SQL (see ``autoprop``).
                enums = None if self.asynchronous else self.enums
                try:
                    outcome, same = await call.run(connection, callee, data, pool=pool, url=self.conninfo, enums=enums)
                except AssertionError as error:
                    if not self.expected(error):
                        raise
                    outcome, same = "known", False
                self.report.calls[call.name][outcome] += 1
                self.report.same += same
        finally:
            await aw(transaction.rollback())

    async def seed(
        self,
        connection: Any,
        data: st.DataObject,
        table: Table,
        pool: Pool,
    ) -> None:
        """Insert one row, read it back through its model, and pool its values."""
        raw = self.raw
        statement, parameters = insert(table, draw_row(data, table, pool), ctid=raw)
        nested = await aw(connection.begin_nested())
        try:
            row = (await aw(connection.execute(statement, parameters))).one()
        except (sqlalchemy.exc.DBAPIError, OverflowError) as error:
            # We refuse a value PostgreSQL holds and Python cannot (a date before year 1) too.
            await aw(nested.rollback())
            if not refused(error):
                raise
            return
        await aw(nested.commit())

        # A psycopg package reads what psycopg itself returns, so we read the row again through psycopg.
        if raw:
            cursor_class = psycopg.AsyncRawCursor if self.asynchronous else psycopg.RawCursor
            cursor = cursor_class(self.callee)
            select = f"SELECT * FROM {table.sql} WHERE ctid = $1::tid"  # noqa: S608
            await aw(cursor.execute(select, [row[-1]]))
            row = await aw(cursor.fetchone())

        names = [column.name for column in table.columns]
        pool.rows[table.sql].append(dict(zip(names, row, strict=True)))
        if table.model is None:
            return

        try:
            record = read(table, row)
        except AssertionError as error:
            if not self.expected(error):
                raise
            return
        pool.harvest(record)

    def expected(self, error: AssertionError) -> bool:
        """Count ``error``, a value outside its annotation, when it is one of the target's known failures.

        Which item of a list does not matter, so we match ``[3]`` as ``[]``.
        """
        text = re.sub(r"\[\d+\]", "[]", str(error))
        known = next((known for known in self.report.target.known if known in text), None)
        if known is not None:
            self.report.known[known] += 1
        return known is not None


def module_of(filename: str) -> str:
    """The module a query file becomes: its stem, each `-` a `_` (§1)."""
    return Path(filename).stem.replace("-", "_")
