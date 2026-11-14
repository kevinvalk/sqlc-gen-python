"""One call of a generated function in the runtime sweep (see ``autoprop``).

``calls`` pairs a query module's functions with sqlc's queries, and
``argument`` draws each parameter as sqlc typed it. A ``Call`` runs its
function once with drawn arguments, names the outcome, and holds the result to
sqlc's own text run directly through psycopg (``Call.direct``), which the value
helpers below make comparable.
"""

from __future__ import annotations

import dataclasses
import enum
import inspect
import re
import typing
from dataclasses import dataclass
from typing import TYPE_CHECKING, Any

import psycopg
import sqlalchemy
from hypothesis import strategies as st
from psycopg import sql as pgsql
from psycopg.types.enum import EnumInfo, register_enum
from psycopg.types.json import Json as PgJson
from psycopg.types.json import Jsonb
from psycopg.types.range import Range as PgRange
from sqlalchemy.dialects import postgresql

import pgvalues
from autoprop_pool import PICK
from autoprop_refusals import is_error, refused, rejected
from postgres import aw
from suite.deep import checked, failure, strip_none

if TYPE_CHECKING:
    from collections.abc import Callable
    from types import ModuleType

    from autoprop_pool import Pool

COMPARED = {":one", ":many", ":execrows"}
WRITERS = re.compile(r"\s*(?:--[^\n]*\n\s*)*(INSERT|DELETE|TRUNCATE)\b", re.IGNORECASE)

Json = dict[str, Any]


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
    """A generated function, each call of it checked against its annotations,
    and the query it implements.
    """

    name: str
    function: Callable[..., Any]
    query: Json
    arguments: list[Argument]
    # The record a `:copyfrom` or batch function takes an iterable of, or
    # one of, as `params`: more parameters than the options' `max_params`.
    record: Any = None
    bundled: bool = False

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
        if self.bundled:
            rows = [data.draw(fields, label=self.name)]
            kwargs: dict[str, object] = {"params": self.record(**rows[0])}
        elif self.record:
            rows = data.draw(st.lists(fields, max_size=3), label=self.name)
            kwargs = {"rows": [self.record(**r) for r in rows]}
        else:
            rows = [data.draw(fields, label=self.name)]
            kwargs = rows[0]
        # A value drawn for sqlc's type that the annotation does not take (an
        # override's type, a driver's own class) proves nothing.
        if any(failure(r[a.name], a.hint) for r in rows for a in self.arguments):
            return "inconclusive", False
        cmd = self.query["cmd"]
        expected = (
            self.direct(conn, rows[0] if self.bundled else kwargs, enums)
            if enums is not None and cmd in COMPARED
            else None
        )
        nested = await aw(conn.begin_nested())
        try:
            result = await aw(self.function(callee, **kwargs))
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
            # `None` is no row, or a row of one NULL: whichever the direct
            # run found, so a value where there is none still differs.
            case ":one" if result is None and expected == []:
                got: object = []
            case ":one":
                got = [repr(flat(result))]
            case ":many":
                got = sorted(repr(flat(r)) for r in typing.cast("list[object]", result))
            case _:
                got = result
        # A `:one` returned its row, or `None` without one (else it raised).
        none: dict[str, object] = {
            ":one": None,
            ":many": [],
            ":execrows": 0,
            ":execresult": 0,
        }
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
        record, bundled = None, parameters == ["params"]
        # A `:copyfrom` or a batch takes `rows` of its parameters; a query of
        # more than `max_params`, one record of them.
        if query["cmd"] == ":copyfrom" or query["cmd"].startswith(":batch"):
            (rows,) = parameters
            (record,) = typing.get_args(hints[rows])
        elif bundled:
            record = hints["params"]
        if record is not None:
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
        call = checked(function, set())
        found.append(Call(name, call, query, arguments, record, bundled))
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
