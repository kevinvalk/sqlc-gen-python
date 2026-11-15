"""The runtime sweep of one generated package (see ``autoprop``).

``run`` sweeps a ``Target`` in a database of its own and returns a ``Report``;
it is what each worker process of ``autoprop.sweep`` runs. ``Sweep`` is the
Hypothesis test behind it: each example seeds every table and calls every
function, in a transaction it rolls back.
"""

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
    """A generated package, the request it came from, and that request's schema."""

    package: str
    request: Path
    schema: Path
    # What the failures listed for it in `autoprop.KNOWN` say.
    known: tuple[str, ...] = ()
    # Whether its functions are coroutines.
    asynchronous: bool = False
    # Whether its functions take a psycopg connection (the psycopg backend).
    raw: bool = False


@dataclass
class Report:
    """What sweeping one package found."""

    target: Target
    # Why it failed, or could not run at all (a schema PostgreSQL rejects:
    # perhaps not the generator's fault, but nothing proven).
    failure: str = ""
    calls: dict[str, Counter[str]] = field(default_factory=dict)
    # The functions whose query returns rows, so an empty result checks nothing.
    returning: set[str] = field(default_factory=set)
    # How often each of the target's known failures happened.
    known: Counter[str] = field(default_factory=Counter)
    # The calls that returned what sqlc's own SQL, run directly, did.
    same: int = 0

    @property
    def exercised(self) -> list[str]:
        """The functions a call checked everything of (see ``autoprop``)."""
        return [
            n
            for n, c in self.calls.items()
            if c["rows"] or (n not in self.returning and c["empty"] + c["ran"])
        ]


def run(server: sqlalchemy.URL, target: Target) -> Report:
    """Sweep one target; a failure is reported, never raised."""
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
    if group := getattr(error, "exceptions", None):  # several distinct failures
        return "\n  ".join(describe(e) for e in group)
    first = f"{type(error).__name__}: {error}".splitlines()[0]
    notes = [n for n in getattr(error, "__notes__", []) if n.startswith("Draw ")]
    return "\n    ".join([first, *notes])


class Sweep:
    """Hypothesis over one package in one database."""

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
        modules = {
            stem: importlib.import_module(f"{package.__name__}.{stem}")
            for stem in sorted(stems - {"__init__"})
        }
        models = modules.pop("models", None)

        def queries(stem: str) -> list[Json]:
            return [q for q in request["queries"] if module_of(q["filename"]) == stem]

        oracle = sqlalchemy.create_engine(
            url.set(drivername="postgresql+psycopg"), isolation_level="AUTOCOMMIT"
        )
        try:
            with oracle.connect() as conn:
                self.tables = tables(conn, models)
                self.enums = [
                    labels(driver(conn), schema["name"], e["name"])
                    for schema in request["catalog"]["schemas"]
                    # An enum without labels has no value to read.
                    for e in schema.get("enums", [])
                    if e["vals"]
                ]
                self.calls = [
                    call
                    for stem, module in modules.items()
                    for call in calls(conn, module, queries(stem))
                ]
        finally:
            oracle.dispose()
        if missing := {module_of(q["filename"]) for q in request["queries"]} - set(
            modules
        ):
            message = f"no module for {', '.join(sorted(missing))}"
            raise AssertionError(message)
        report.calls = {call.name: Counter() for call in self.calls}
        report.returning = {
            call.name for call in self.calls if call.query["cmd"] in {":one", ":many"}
        }
        # A psycopg package's coroutines take psycopg's own async connection.
        asyncpg = self.asynchronous and not self.raw
        self.url = url.set(
            drivername="postgresql+asyncpg" if asyncpg else "postgresql+psycopg"
        )

    def run(self) -> None:
        """Run the examples on one connection, each in its own transaction."""
        loop = asyncio.new_event_loop()
        # SQLAlchemy would register psycopg's `hstore` loader on the very
        # connection a psycopg package reads from.
        native = {"use_native_hstore": False} if self.raw else {}
        engine: Any = (
            create_async_engine(self.url, **native)
            if self.asynchronous
            else sqlalchemy.create_engine(self.url, **native)
        )
        conn = loop.run_until_complete(aw(engine.connect()))
        callee = self.callee = conn
        if self.raw:
            fairy = (
                loop.run_until_complete(conn.get_raw_connection())
                if self.asynchronous
                else conn.connection
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
            loop.run_until_complete(self.example(conn, callee, data))

        try:
            examples()
        finally:
            loop.run_until_complete(aw(conn.close()))
            loop.run_until_complete(aw(engine.dispose()))
            loop.close()

    async def example(
        self,
        conn: Any,
        callee: Any,
        data: st.DataObject,
    ) -> None:
        """Seed every table, call every function on ``callee``, roll back."""
        transaction = await aw(conn.begin())
        try:
            pool = Pool({t.model: t.name for t in self.tables if t.model})
            for table in self.tables:
                for _ in range(data.draw(st.integers(0, 2), label=table.sql)):
                    await self.seed(conn, data, table, pool)
            for call in self.calls:
                # Only functions' calls are compared (see ``autoprop``).
                enums = None if self.asynchronous else self.enums
                try:
                    outcome, same = await call.run(
                        conn, callee, data, pool=pool, url=self.conninfo, enums=enums
                    )
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
        conn: Any,
        data: st.DataObject,
        table: Table,
        pool: Pool,
    ) -> None:
        """Insert one row, read it back through its model, and pool its values."""
        raw = self.raw
        statement, params = insert(table, draw_row(data, table, pool), ctid=raw)
        nested = await aw(conn.begin_nested())
        try:
            row = (await aw(conn.execute(statement, params))).one()
        except (sqlalchemy.exc.DBAPIError, OverflowError) as error:
            # A value PostgreSQL holds and Python cannot (a date before year
            # 1) is refused too.
            await aw(nested.rollback())
            if not refused(error):
                raise
            return
        await aw(nested.commit())
        if raw:
            # A psycopg package reads what psycopg itself returns.
            cursor = psycopg.AsyncRawCursor if self.asynchronous else psycopg.RawCursor
            cur = cursor(self.callee)
            select = f"SELECT * FROM {table.sql} WHERE ctid = $1::tid"  # noqa: S608
            await aw(cur.execute(select, [row[-1]]))
            row = await aw(cur.fetchone())
        names = [c.name for c in table.columns]
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
        """Count ``error``, a value outside its annotation, if it is one of the
        target's known failures. Which item of a list does not matter: ``[3]``
        is matched as ``[]``.
        """
        text = re.sub(r"\[\d+\]", "[]", str(error))
        known = next((k for k in self.report.target.known if k in text), None)
        if known:
            self.report.known[known] += 1
        return known is not None


def module_of(filename: str) -> str:
    """The module a query file becomes: its stem, each `-` a `_` (§1)."""
    return Path(filename).stem.replace("-", "_")
