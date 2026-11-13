"""The runtime sweep: run what the generator emits against PostgreSQL.

For every PostgreSQL package generated on SQLAlchemy or psycopg from a golden
case or a corpus request, the request's schema is applied to a fresh database,
the package is imported, and Hypothesis draws examples for its functions (its
coroutines, in an async package: on asyncpg, or psycopg's own on psycopg).
A psycopg package's functions take the psycopg connection under SQLAlchemy's,
in its transaction. Each example, in a transaction it rolls back:

1. seeds every table in foreign-key order, taking referenced values from rows
   seeded before, and reads each row back through its model, which must
   inhabit its annotations exactly (``deep.check``);
2. calls every generated function once, writers first and deletes last, with
   arguments drawn for its annotations and sqlc's parameter types, often
   reusing values seen earlier so that lookups find rows. Every argument and
   result is checked against its annotation (``deep.checked``).

A call returned ``rows``, came back ``empty`` (for a ``:one``, raised
``NoResultFound``, or the package's ``NoRowError``), ``ran`` (``:exec`` and
``:copyfrom`` return nothing), or was ``inconclusive``: the database refused
the drawn data (an integrity, data or cardinality error, a timeout), or it
rejects the SQL itself. Anything else fails the package: a value that does not
match its annotation, a crash, a parameter the driver cannot bind, a module
whose functions do not match its queries.

A function is **exercised** once a call of it checked everything it can
return: one that returned ``rows``, or, for a command that returns no rows
(``:exec``, ``:execrows``, ``:execresult``, ``:copyfrom``), any call that was
not inconclusive. An empty result checked no value against its annotation. A
function never exercised is a warning; the share exercised across all
packages must stay at least ``FLOOR``. Which functions are exercised varies a
little between runs (one in about a thousand flipped between two runs), so a
committed list of them would fail at random.

Each call of a function for ``:one``, ``:many`` or ``:execrows`` is also
compared with sqlc's own text run directly, with the same arguments, through
psycopg's ``RawCursor``, which takes ``$N`` as written: the rows (as a
multiset) or the row count must be the same. That holds the rewriting and the
reading of every result to a reference that shares none of the generator's
code. The direct run happens twice, each rolled back; if the two differ (a
sequence, ``random()``), nothing is compared. A coroutine runs the same SQL as
its function.

Functions run on psycopg, coroutines on asyncpg, or on psycopg for a psycopg
package.
"""

from __future__ import annotations

import asyncio
import dataclasses
import enum
import importlib
import inspect
import json
import os
import re
import sys
import typing
from collections import Counter, defaultdict
from concurrent.futures import ProcessPoolExecutor
from dataclasses import dataclass, field
from functools import partial
from pathlib import Path
from typing import TYPE_CHECKING, Any

import psycopg
import sqlalchemy
from hypothesis import HealthCheck, Phase, given, settings
from hypothesis import strategies as st
from psycopg import sql as pgsql
from psycopg.types.enum import EnumInfo, register_enum
from psycopg.types.json import Json as PgJson
from psycopg.types.json import Jsonb
from psycopg.types.range import Range as PgRange
from sqlalchemy.dialects import postgresql
from sqlalchemy.ext.asyncio import create_async_engine

import pgvalues
from examples import golden_cases
from pgvalues import Column, Table, enum_array_type, json_type, sqlalchemy_type, tables
from postgres import SchemaRejectedError, aw, conninfo, database
from suite.deep import build, check, checked, failure, strip_none

if TYPE_CHECKING:
    from collections.abc import Callable, Sequence
    from types import ModuleType

ROOT = Path(__file__).resolve().parents[2]
# The share of generated functions that must be exercised (see above): 92%
# when an empty result stopped counting, less a margin for the calls a
# statement timeout makes inconclusive on a slower machine. Raise it as the
# sweep improves, never lower it to pass.
FLOOR = 0.91
EXAMPLES = 100
# An index into what a ``Pool`` holds, taken modulo its length. What Hypothesis
# draws must not depend on the database: it records each example's draws, and
# calls an example flaky (``FlakyStrategyDefinition``) when the same draws
# meet another strategy, as ``sampled_from`` a pool that grew would be.
PICK = st.integers(0, 15)
# SQLSTATE classes that mean the database refused the drawn data, not the call:
# cardinality, data, integrity, transaction state, rollback, insufficient
# resources (memory), program limits (an array too large), object state,
# operator intervention (timeouts), PL/pgSQL raise, feature not supported.
REFUSALS = ("21", "22", "23", "25", "40", "53", "54", "55", "57", "P0", "0A")
COMPARED = {":one", ":many", ":execrows"}
# Failures the sweep must keep finding, and why they are not the generator's.
KNOWN = Path(__file__).with_name("known-failures.tsv")
WRITERS = re.compile(r"\s*(?:--[^\n]*\n\s*)*(INSERT|DELETE|TRUNCATE)\b", re.IGNORECASE)

Json = dict[str, Any]


@dataclass(frozen=True)
class Target:
    """A generated package, the request it came from, and that request's schema."""

    package: str
    request: Path
    schema: Path
    # What the failures listed for it in `KNOWN` say.
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
        """The functions a call checked everything of (see above)."""
        return [
            n
            for n, c in self.calls.items()
            if c["rows"] or (n not in self.returning and c["empty"] + c["ran"])
        ]


def targets(golden: Path, generated: Path) -> list[Target]:
    """Every PostgreSQL package generated on SQLAlchemy to sweep, sync or
    async: the golden cases laid out under ``golden`` (``<case>/``),
    and the corpus outputs ``cargo test`` exported to ``generated``, as
    ``packages.tsv`` lists them.
    """
    found = []
    for case in golden_cases():
        if not (golden / case.name).is_dir() or not is_swept(
            case.request, case.options
        ):
            continue
        asynchronous, raw = bool(case.options["async"]), case.backend == "psycopg"
        found.append(
            Target(case.name, case.request, case.schema_sql, (), asynchronous, raw)
        )
    profiles = json.loads((ROOT / "tests/sqlc/endtoend/profiles.json").read_text())
    # Each package to sweep, a request it came from, and its options.
    modes: dict[str, tuple[Path, Json]] = {}
    for (name, profile), package in corpus_packages(generated).items():
        path = ROOT / "tests/sqlc/endtoend" / name
        if is_swept(path, profiles[profile]):
            modes.setdefault(package, (path, profiles[profile]))
    for package, (path, options) in modes.items():
        known = tuple(text for case, text in known_failures() if case in path.parents)
        asynchronous, raw = bool(options["async"]), options["backend"] == "psycopg"
        found.append(Target(package, path, schema_of(path), known, asynchronous, raw))
    return found


def is_swept(request: Path, options: Json) -> bool:
    """Whether the sweep runs what ``request`` generates with ``options``:
    PostgreSQL, on SQLAlchemy or psycopg.
    """
    engine = json.loads(request.read_text())["settings"]["engine"]
    return engine == "postgresql" and options["backend"] in {"sqlalchemy", "psycopg"}


def corpus_packages(generated: Path) -> dict[tuple[str, str], str]:
    """The package ``cargo test`` exported each generated corpus request and
    profile as (``packages.tsv``): one per distinct output, so several share
    one. Raise if a package is missing.
    """
    rows = [
        line.split("\t")
        for line in (generated / "packages.tsv").read_text().splitlines()[1:]
    ]
    if missing := sorted({p for *_, p in rows if not (generated / p).is_dir()}):
        message = (
            f"packages.tsv names packages cargo test did not export: {missing[:3]}"
        )
        raise FileNotFoundError(message)
    return {(request, profile): package for request, profile, package in rows}


def known_failures() -> list[tuple[Path, str]]:
    """``KNOWN``'s entries: a corpus case, and what its failure says."""
    rows = [line.split("\t") for line in KNOWN.read_text().splitlines()]
    corpus = ROOT / "tests/sqlc/endtoend"
    return [(corpus / row[0], row[1]) for row in rows if row[0] and row[0][0] != "#"]


def schema_of(request: Path) -> Path:
    """The schema script ``capture.py`` stored beside a request."""
    package = re.fullmatch(r"request(?:\.managed-db)?(\.\d+)?\.json", request.name)
    if package is None:
        message = f"not a captured request: {request}"
        raise ValueError(message)
    return request.with_name(f"schema{package[1] or ''}.sql")


def sweep(
    server: sqlalchemy.URL, found: list[Target], roots: list[Path]
) -> list[Report]:
    """Sweep every target, a process per CPU, each in a database of its own."""
    with psycopg.connect(conninfo(server)) as conn:
        if conn.info.server_version < 160000:  # noqa: PLR2004
            # `rejected` plans with EXPLAIN (GENERIC_PLAN), which is new in 16.
            message = "the runtime sweep needs PostgreSQL 16 or newer"
            raise RuntimeError(message)
    workers = min(os.cpu_count() or 1, 8)
    with ProcessPoolExecutor(
        workers, initializer=_importable, initargs=(roots,)
    ) as pool:
        return list(pool.map(partial(run, server), found))


def summarize(reports: list[Report]) -> tuple[bool, list[str]]:
    """Print what the sweep found: each package that failed or could not run,
    each known failure that no longer happens, each function never exercised.
    Return whether that fails the sweep, and its totals.

    A package that could not run fails it too, and so does a sweep that called
    nothing: a schema PostgreSQL rejects proves nothing about its package, and
    a new image or extension could make it reject all of them.
    """
    for r in reports:
        if r.failure:
            print(f"  FAILED {r.target.package}\n    {r.failure}")
    happened = {k for r in reports for k in r.known}
    stale = [(case, text) for case, text in known_failures() if text not in happened]
    for case, text in stale:
        where = case.relative_to(ROOT)
        print(f"  known failure no longer happens; remove it: {where}: {text}")
    functions = sum(len(r.calls) for r in reports)
    exercised = sum(len(r.exercised) for r in reports)
    share = exercised / functions if functions else 0.0
    failed = sum(1 for r in reports if r.failure)
    same = sum(r.same for r in reports)
    totals = [
        f"{len(reports)} APIs: {failed} failed, {len(stale)} known failures stale",
        f"{exercised}/{functions} functions exercised ({share:.0%}, floor {FLOOR:.0%})",
        f"{same} calls returned what sqlc's SQL run directly did",
    ]
    return bool(failed or stale or not same) or share < FLOOR, totals


def _importable(roots: list[Path]) -> None:
    sys.path[:0] = [str(root) for root in roots]
    sys.dont_write_bytecode = True


def run(server: sqlalchemy.URL, target: Target) -> Report:
    """Sweep one target; a failure is reported, never raised."""
    report = Report(target)
    try:
        package = importlib.import_module(target.package)
        request: Json = json.loads(target.request.read_text())
        with database(server, target.schema.read_text()) as url:
            Sweep(package, request, url, report, target).run()
    except SchemaRejectedError as error:
        report.failure = f"schema: {error}"
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


@dataclass(frozen=True)
class Argument:
    """One parameter of a generated function, and the values to draw for it."""

    name: str
    hint: Any
    # Where sqlc says the parameter's value goes: ``table.column`` and ``column``.
    keys: tuple[str, ...]
    fresh: st.SearchStrategy[object]

    def values(self, pool: Pool) -> st.SearchStrategy[object]:
        """Values seen earlier that fit (for a list, a list of items seen),
        or fresh ones; ``None`` if nullable.

        What is drawn is the same whatever the pool holds (see ``PICK``).
        """
        hint, nullable = strip_none(self.hint)
        seen = pool.fitting(self.keys, hint)
        if not seen and typing.get_origin(hint) is list:
            items = pool.fitting((), typing.get_args(hint)[0])
            seen = [items[i : i + 3] for i in range(len(items))]
        values = st.tuples(st.booleans(), PICK, self.fresh).map(
            lambda drawn: seen[drawn[1] % len(seen)] if drawn[0] and seen else drawn[2]
        )
        return st.none() | values if nullable else values


@dataclass(frozen=True)
class Call:
    """A generated function and the query it implements."""

    name: str
    function: Callable[..., Any]
    query: Json
    arguments: list[Argument]
    # The record a `:copyfrom` or batch function takes an iterable of.
    record: Any = None

    async def run(
        self,
        conn: Any,  # noqa: ANN401
        callee: Any,  # noqa: ANN401
        data: st.DataObject,
        *,
        pool: Pool,
        url: str,
        enums: list[Labels] | None,
    ) -> tuple[str, bool]:
        """Call once with drawn arguments, on ``callee``, the connection the
        function takes, in ``conn``'s transaction; return the outcome, and
        whether it returned what sqlc's SQL run directly did, which it is when
        ``enums`` (the catalog's, for reading them) are given.
        """
        fields = st.fixed_dictionaries({a.name: a.values(pool) for a in self.arguments})
        if self.record:
            rows = data.draw(st.lists(fields, max_size=3), label=self.name)
            kwargs: dict[str, object] = {"rows": [self.record(**r) for r in rows]}
        else:
            rows = [data.draw(fields, label=self.name)]
            kwargs = rows[0]
        # A value drawn for sqlc's type that the annotation does not take (an
        # override's type, a driver's own class) proves nothing.
        if any(failure(r[a.name], a.hint) for r in rows for a in self.arguments):
            return "inconclusive", False
        cmd = self.query["cmd"]
        expected = (
            self.direct(conn, kwargs, enums)
            if enums is not None and cmd in COMPARED
            else None
        )
        nested = await aw(conn.begin_nested())
        try:
            function = checked(self.function, set())
            result = await aw(function(callee, **kwargs))
        except Exception as error:
            await aw(nested.rollback())
            # A `:one` without a row raises, as Go's `ErrNoRows` (§6).
            if is_error(error, sqlalchemy.exc.NoResultFound, "NoRowError"):
                if cmd not in {":one", ":batchone"}:
                    raise
                return "empty", self.compare(expected, [])
            if refused(error) or rejected(error, url, self.query["text"]):
                return "inconclusive", False
            raise
        await aw(nested.commit())
        pool.harvest(result)
        return self.outcome(result, expected)

    def outcome(self, result: object, expected: object) -> tuple[str, bool]:
        """What a call that returned ``result`` did, and how it compared."""
        cmd = self.query["cmd"]
        if cmd in {":exec", ":copyfrom", ":batchexec"}:
            return "ran", False
        # A batch returns each row's result: rows if any of them has one.
        if cmd in {":batchone", ":batchmany"}:
            items = typing.cast("list[object]", result)
            if cmd == ":batchmany":
                items = [i for r in items for i in typing.cast("list[object]", r)]
            return ("rows" if items else "empty"), False
        if isinstance(result, sqlalchemy.CursorResult):
            result = result.rowcount
        match cmd:
            case ":one":
                got: object = [repr(flat(result))]
            case ":many":
                got = sorted(repr(flat(r)) for r in typing.cast("list[object]", result))
            case _:
                got = result
        # A `:one` returned its row (else it raised), even if a NULL or `[]`.
        none = {":many": [], ":execrows": 0, ":execresult": 0}
        empty = cmd in none and result == none[cmd]
        return ("empty" if empty else "rows"), self.compare(expected, got)

    def direct(
        self,
        conn: sqlalchemy.Connection,
        kwargs: dict[str, object],
        enums: list[Labels],
    ) -> object:
        """sqlc's text, run twice with ``kwargs`` by psycopg's ``RawCursor``,
        each time rolled back: its rows, as a sorted list of their reprs, or
        its row count; ``"unstable"`` if the two runs differ, ``"unrunnable"``
        if it raised.
        """
        values = dict(
            zip(
                (p["number"] for p in self.query["params"]),
                (
                    bound(p, kwargs[a.name])
                    for p, a in zip(self.query["params"], self.arguments, strict=True)
                ),
                strict=True,
            )
        )
        args = [values[n] for n in sorted(values)]
        runs = []
        for _ in range(2):
            nested = conn.begin_nested()
            try:
                with psycopg.RawCursor(driver(conn)) as cur:
                    for info, labelled in enums:
                        register_enum(info, cur, labelled)
                    cur.execute(self.query["text"], args)
                    runs.append(
                        cur.rowcount
                        if self.query["cmd"] == ":execrows"
                        else sorted(
                            repr(tuple(map(plain, row))) for row in cur.fetchall()
                        )
                    )
            except psycopg.Error:
                return "unrunnable"
            finally:
                nested.rollback()
        return runs[0] if runs[0] == runs[1] else "unstable"

    def compare(self, expected: object, got: object) -> bool:
        """Whether ``got`` was compared with ``expected`` from :meth:`direct`;
        a difference fails the package.
        """
        if expected is None or expected in ("unstable", "unrunnable"):
            return False
        if got != expected:
            message = (
                f"{self.name}: returned {got!r}, "
                f"but sqlc's SQL run directly returned {expected!r}"
            )
            raise AssertionError(message)
        return True


def driver(conn: sqlalchemy.Connection) -> psycopg.Connection[Any]:
    """The psycopg connection under a sync SQLAlchemy one."""
    raw = conn.connection.driver_connection
    if not isinstance(raw, psycopg.Connection):
        message = f"not a psycopg connection: {raw!r}"
        raise TypeError(message)
    return raw


Labels = tuple[EnumInfo, type[enum.Enum]]


def labels(conn: psycopg.Connection[Any], schema: str, name: str) -> Labels:
    """What psycopg needs to read the catalog's enum ``schema.name`` as its
    labels: its type, and an enum whose members are named and valued by them
    (psycopg's own numbers its values, and cannot have no members).
    """
    info = EnumInfo.fetch(conn, pgsql.Identifier(schema, name))
    if info is None:
        message = f"enum {schema}.{name} of the catalog is not in the database"
        raise AssertionError(message)
    # Through `Any`: mypy checks an `Enum()` call only with literal members.
    make: Any = enum.Enum
    labelled = make("Labels", {label: label for label in info.labels})
    return info, typing.cast("type[enum.Enum]", labelled)


def plain(value: object) -> object:
    """A value as psycopg reads it without the generated code: an enum as its
    label, which is its value in ours and in ``labels``', in a list too.
    """
    if isinstance(value, enum.Enum):
        return value.value
    if isinstance(value, list):
        return [plain(v) for v in value]
    # A range, SQLAlchemy's or psycopg's, as what makes it equal.
    if isinstance(value, (postgresql.Range, PgRange)):
        empty = value.isempty if isinstance(value, PgRange) else value.empty
        if empty:
            return ("range", "empty")
        return ("range", value.lower, value.upper, value.bounds)
    return value


def flat(result: object) -> tuple[object, ...]:
    """A generated result as the row it was read from: a record's fields in
    order, an embedded model's spread in place, or a bare value.
    """
    # SQLAlchemy's `Range` is a dataclass too, but one value.
    if not dataclasses.is_dataclass(result) or isinstance(
        result, (type, postgresql.Range)
    ):
        return (plain(result),)
    return tuple(
        v for f in dataclasses.fields(result) for v in flat(getattr(result, f.name))
    )


def bound(param: Json, value: object) -> object:
    """An argument as psycopg binds it for ``param``: an enum as its label,
    JSON wrapped, as psycopg binds no ``dict`` bare.
    """
    if value is None:
        return None
    kind = param["column"]["type"]["name"].removeprefix("pg_catalog.")
    if kind in {"json", "jsonb"}:
        return (Jsonb if kind == "jsonb" else PgJson)(value)
    if isinstance(value, postgresql.Range):
        if value.empty:
            return PgRange(empty=True)
        return PgRange(value.lower, value.upper, value.bounds)
    return plain(value)


def calls(
    conn: sqlalchemy.Connection,
    module: ModuleType,
    queries: list[Json],
) -> list[Call]:
    """The functions of a query file's module, paired with its queries in
    sqlc's order (§6).
    """
    functions = [
        value
        for name, value in vars(module).items()
        if inspect.isfunction(value)
        and value.__module__ == module.__name__
        and not name.startswith("_")
    ]
    if len(functions) != len(queries):
        message = (
            f"{module.__name__}: {len(functions)} functions, {len(queries)} queries"
        )
        raise AssertionError(message)
    found = []
    for function, query in zip(functions, queries, strict=True):
        hints = typing.get_type_hints(function)
        parameters = list(inspect.signature(function).parameters)[1:]
        record = None
        # A `:copyfrom` or a batch takes `rows` of its parameters.
        if query["cmd"] == ":copyfrom" or query["cmd"].startswith(":batch"):
            (rows,) = parameters
            (record,) = typing.get_args(hints[rows])
            hints = typing.get_type_hints(record)
            parameters = [f.name for f in dataclasses.fields(record)]
        params = query.get("params", [])
        if len(parameters) != len(params):
            message = f"{function.__qualname__}: {len(parameters)} parameters, sqlc has {len(params)}"
            raise AssertionError(message)
        arguments = [
            argument(conn, name, hints[name], param["column"])
            for name, param in zip(parameters, params, strict=True)
        ]
        name = f"{module.__name__.split('.')[-1]}.{function.__name__}"
        found.append(Call(name, function, query, arguments, record))
    return sorted(found, key=_writes_first)


def _writes_first(call: Call) -> int:
    if call.query["cmd"] == ":copyfrom":
        return 0
    verb = WRITERS.match(call.query["text"])
    return 1 if verb is None else 0 if verb[1].upper() == "INSERT" else 2


def argument(
    conn: sqlalchemy.Connection,
    name: str,
    hint: Any,  # noqa: ANN401
    column: Json,
) -> Argument:
    """Draw values for a parameter as sqlc typed it, in the annotation's form."""
    table = (column.get("table") or {}).get("name", "")
    keys = tuple(k for k in (f"{table}.{column['name']}", column["name"]) if k)
    bare, _ = strip_none(hint)
    dims, leaf = 0, bare
    while typing.get_origin(leaf) is list:
        (leaf,) = typing.get_args(leaf)
        dims += 1
    pg_type = column.get("type") or {}
    pg = pgvalues.resolve_name(
        conn, ".".join(p for p in (pg_type.get("schema"), pg_type.get("name")) if p)
    )
    if isinstance(leaf, type) and issubclass(leaf, enum.Enum):
        fresh = pgvalues.arrays(st.sampled_from(list(leaf)), dims)
    elif pg is not None and (values := pgvalues.strategy(pg, dims)) is not None:
        fresh = values
    elif leaf is object:
        fresh = pgvalues.arrays(pgvalues.json(), dims)
    else:
        fresh = pgvalues.arrays(st.from_type(leaf), dims)
    return Argument(name, hint, keys, fresh)


class Pool:
    """Values seen in one example, to feed later arguments and foreign keys."""

    LIMIT = 16

    def __init__(self, tables: dict[Any, str]) -> None:
        # The table each model reads, so a model a function returns pools its
        # values under its table's name too.
        self.tables = tables
        self.rows: defaultdict[str, list[dict[str, object]]] = defaultdict(list)
        self.values: defaultdict[object, list[object]] = defaultdict(list)

    def add(self, key: object, value: object) -> None:
        if value is not None and len(self.values[key]) < self.LIMIT:
            self.values[key].append(value)

    def harvest(self, value: object) -> None:
        """Pool a result: its fields by name, a model's also by its table's,
        and every value by type.
        """
        if dataclasses.is_dataclass(value) and not isinstance(value, type):
            table = self.tables.get(type(value), "")
            for f in dataclasses.fields(value):
                item = getattr(value, f.name)
                self.add(f.name.rstrip("_"), item)
                if table:
                    self.add(f"{table}.{f.name.rstrip('_')}", item)
                self.harvest(item)
        elif isinstance(value, list):
            for item in typing.cast("list[object]", value):
                self.harvest(item)
        elif value is not None and not isinstance(value, sqlalchemy.CursorResult):
            self.add(type(value), value)

    def fitting(self, keys: tuple[str, ...], hint: Any) -> list[object]:  # noqa: ANN401
        """Pooled values under the first of ``keys`` that has any (the most
        precise first), or of ``hint``'s type, that fit it.
        """
        found = next((self.values[key] for key in keys if self.values[key]), [])
        if not found and isinstance(hint, type):
            found = self.values[hint]
        return [v for v in found if failure(v, hint) is None]


def refused(error: Exception) -> bool:
    """Whether the database or driver refused the drawn data, not the call:
    a constraint or data error, a value outside Python's range, a cardinality
    or timeout error, an error raised inside a function the schema defines, or
    a syntax error in a value (``to_tsquery('a:')``), which unlike one in the
    statement points nowhere into it.
    """
    if isinstance(error, OverflowError) or is_error(
        error, sqlalchemy.exc.MultipleResultsFound, "MultipleRowsError"
    ):
        return True
    if isinstance(error, (sqlalchemy.exc.IntegrityError, sqlalchemy.exc.DataError)):
        return True
    orig = database_error(error)
    if orig is None:
        return False
    # psycopg refuses some data itself, before PostgreSQL could (a NUL in text).
    if isinstance(orig, psycopg.DataError) and sqlstate(orig) is None:
        return True
    # psycopg's diagnostics, or the asyncpg error SQLAlchemy's adapter wraps.
    diag = getattr(orig, "diag", None)
    cause = getattr(orig, "__cause__", None)
    context = getattr(diag, "context", None) or getattr(cause, "context", None)
    position = getattr(diag, "statement_position", None) or getattr(
        cause, "position", None
    )
    state = sqlstate(orig) or ""
    return (
        state[:2] in REFUSALS
        or "function" in (context or "")
        or (state == "42601" and not position)
    )


def module_of(filename: str) -> str:
    """The module a query file becomes: its stem, each `-` a `_` (§1)."""
    return Path(filename).stem.replace("-", "_")


def is_error(error: Exception, sqlalchemy_error: type[Exception], package: str) -> bool:
    """Whether ``error`` is SQLAlchemy's error, or the generated package's of
    that name (a class of each package's own ``__init__``).
    """
    return isinstance(error, sqlalchemy_error) or (
        type(error).__name__ == package and isinstance(error, LookupError)
    )


def database_error(error: Exception) -> BaseException | None:
    """The driver's error behind ``error``: SQLAlchemy's wraps it, a psycopg
    package raises it; ``None`` if it is no database error.
    """
    if isinstance(error, sqlalchemy.exc.DBAPIError):
        return error.orig
    return error if isinstance(error, psycopg.Error) else None


def sqlstate(error: BaseException) -> str | None:
    """The SQLSTATE of a driver's error, from psycopg or asyncpg."""
    found = getattr(error, "sqlstate", None) or getattr(error, "pgcode", None)
    return str(found) if found else None


def rejected(error: Exception, url: str, sql: str) -> bool:
    """Whether a syntax or access error is PostgreSQL rejecting sqlc's SQL as
    written, whatever the arguments: it cannot even plan it.
    """
    orig = database_error(error)
    if orig is None or (sqlstate(orig) or "")[:2] != "42":
        return False
    with psycopg.connect(url, autocommit=True) as conn:
        try:
            conn.execute(f"EXPLAIN (GENERIC_PLAN) {sql}")
        except psycopg.Error:
            return True
    return False


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
        conn: Any,  # noqa: ANN401
        callee: Any,  # noqa: ANN401
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
                # Only functions' calls are compared (see above).
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
        conn: Any,  # noqa: ANN401
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


def read(table: Table, row: Sequence[object]) -> object:
    """Read a row through its model's ``from_row``, which must agree with
    reading it by position (§4) and inhabit the model's annotations.
    """
    record = table.model.from_row(row)
    where = f"{table.model.__name__}.from_row on {table.sql}"
    check(record, table.model, where)
    if not same(record, build(table.model, row)):
        message = f"{where}: {record!r} is not the row read by position"
        raise AssertionError(message)
    return record


def same(a: object, b: object) -> bool:
    """Whether two records hold the same values, field by field, a value
    being the same as itself: a NaN is not equal to itself, and since Python
    3.13 a dataclass compares its fields by ``==`` alone.
    """
    if dataclasses.is_dataclass(a) and type(a) is type(b):
        return all(
            same(getattr(a, f.name), getattr(b, f.name)) for f in dataclasses.fields(a)
        )
    return a is b or a == b


def draw_row(data: st.DataObject, table: Table, pool: Pool) -> dict[str, object]:
    """Draw a row's values: foreign keys from rows seeded before, defaults
    left out at random. Every draw is made whatever the pool holds (see
    ``PICK``).
    """
    values: dict[str, object] = {}
    for column in table.columns:
        if column.generated:
            continue
        default = column.optional and data.draw(
            st.booleans(), label=f"{column.name} default"
        )
        value = data.draw(_seeds(column), label=column.name)
        if not default:
            values[column.name] = value
    for columns, referenced, keys in table.references:
        i = data.draw(PICK, label=f"{table.sql} -> {referenced}")
        if rows := pool.rows[referenced]:
            row = rows[i % len(rows)]
            values |= dict(zip(columns, (row[k] for k in keys), strict=True))
    return values


def _seeds(column: Column) -> st.SearchStrategy[object]:
    values = pgvalues.strategy(column.pg)
    if values is None:
        return st.none()  # a type nobody has drawn: only NULL, or a refusal
    return st.none() | values if column.nullable else values


def insert(
    table: Table, values: dict[str, object], *, ctid: bool = False
) -> tuple[sqlalchemy.TextualSelect, dict[str, object]]:
    """``INSERT ... RETURNING *``, typed where a driver needs it (§6), and its
    parameters, bound as ``p0``, ``p1``, ... whatever the column names; with
    ``ctid``, the row's ``ctid`` after it, to read it again by.
    """
    types = {c.name: c.pg for c in table.columns}
    names = ", ".join('"{}"'.format(name.replace('"', '""')) for name in values)
    binds = ", ".join(f":p{i}" for i in range(len(values)))
    body = f"({names}) VALUES ({binds})" if values else "DEFAULT VALUES"
    returning = "*, ctid::text" if ctid else "*"
    statement = sqlalchemy.text(f"INSERT INTO {table.sql} {body} RETURNING {returning}")
    typed = [
        sqlalchemy.bindparam(f"p{i}", type_=json_type(types[name]))
        for i, name in enumerate(values)
        if types[name].name in {"json", "jsonb"}
    ] + [
        sqlalchemy.bindparam(f"p{i}", type_=ranged)
        for i, name in enumerate(values)
        if (ranged := sqlalchemy_type(types[name])) is not None
    ]
    if typed:
        statement = statement.bindparams(*typed)
    columns = [
        sqlalchemy.column(c.name, enum_array_type(c.pg) or sqlalchemy_type(c.pg))
        for c in table.columns
    ]
    typed = statement.columns(*columns, *[sqlalchemy.column("ctid")] * ctid)
    return typed, {f"p{i}": value for i, value in enumerate(values.values())}
