"""One call of a generated function in the runtime sweep (see ``autoprop``), held to sqlc's own text run directly."""

from __future__ import annotations

import enum
import inspect
import itertools
import re
import types
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
from suite.deep import checked, failure, is_stream, record_fields, streamed, strip_none

if TYPE_CHECKING:
    from collections.abc import Callable
    from types import ModuleType

    from autoprop_pool import Pool

COMPARED = {":one", ":many", ":execrows", ":copyfrom"}
WRITERS = re.compile(r"\s*(?:--[^\n]*\n\s*)*(INSERT|DELETE|TRUNCATE)\b", re.IGNORECASE)

Json = dict[str, Any]


@dataclass(frozen=True)
class Argument:
    name: str
    hint: Any
    keys: tuple[str, ...]  # Where sqlc says the parameter's value goes: ``table.column`` and ``column``
    fresh: st.SearchStrategy[object]

    def values(self, pool: Pool) -> st.SearchStrategy[object]:
        """Values seen earlier that fit (for a list, a list of items seen), or fresh ones, and ``None`` when nullable.

        What is drawn is the same whatever the pool holds (see ``PICK``).
        """
        hint, nullable = strip_none(self.hint)
        seen = pool.fitting(self.keys, hint)
        if len(seen) <= 0 and typing.get_origin(hint) is list:
            element, _ = strip_none(typing.get_args(hint)[0])
            items = pool.fitting((), element)
            seen = [items[i : i + 3] for i in range(len(items))]
        values = st.tuples(st.booleans(), PICK, self.fresh).map(
            lambda drawn: seen[drawn[1] % len(seen)] if drawn[0] and len(seen) > 0 else drawn[2]
        )
        return st.none() | values if nullable else values


@dataclass(frozen=True)
class Call:
    name: str
    function: Callable[..., Any]  # Checks each call against its annotations
    query: Json
    arguments: list[Argument]

    # The record a `:copyfrom` or batch function takes an iterable of, or one of as `params` when a query has more
    # parameters than the options' `max_params`.
    record: Any = None
    bundled: bool = False
    ordered: bool = False
    raise_on_none: bool = False

    async def run(
        self,
        connection: Any,
        callee: Any,
        data: st.DataObject,
        *,
        pool: Pool,
        url: str,
        enums: list[Labels] | None,
    ) -> tuple[str, bool]:
        """Call once with drawn arguments on ``callee`` (the connection the function takes), in ``connection``'s
        transaction. Return the outcome, and whether it returned what sqlc's SQL run directly did, which we only compare
        when given the catalog's ``enums`` to read them.
        """
        fields = st.fixed_dictionaries({a.name: a.values(pool) for a in self.arguments})
        if self.bundled:
            rows = [data.draw(fields, label=self.name)]
            kwargs: dict[str, object] = {"params": self.record(**rows[0])}
        elif self.record is not None:
            rows = data.draw(st.lists(fields, max_size=3), label=self.name)
            kwargs = {"rows": [self.record(**row) for row in rows]}
        else:
            rows = [data.draw(fields, label=self.name)]
            kwargs = rows[0]

        # A value drawn for sqlc's type that the annotation does not take (an override's type, a driver's own class)
        # proves nothing.
        if any(failure(row[a.name], a.hint) for row in rows for a in self.arguments):
            return "inconclusive", False

        cmd = self.query["cmd"]
        expected = self.direct(connection, rows, enums) if enums is not None and cmd in COMPARED else None
        nested = await aw(connection.begin_nested())
        try:
            result = await aw(self.function(callee, **kwargs))
        except Exception as error:
            await aw(nested.rollback())

            # A `:one` without a row raises, as Go's `ErrNoRows`.
            if is_error(error, sqlalchemy.exc.NoResultFound, "NoRowError"):
                if cmd not in {":one", ":batchone"} or not self.raise_on_none:
                    raise
                return "empty", self.compare(expected, [])
            if isinstance(expected, (int, list)):
                if (
                    cmd == ":one"
                    and isinstance(expected, list)
                    and len(expected) > 1
                    and is_error(error, sqlalchemy.exc.MultipleResultsFound, "MultipleRowsError")
                ):
                    return "inconclusive", True
                message = f"{self.name}: direct SQL returned {expected!r}, generated call raised {error!r}"
                raise AssertionError(message) from error
            if refused(error) or rejected(error, url, self.query["text"]):
                return "inconclusive", False
            raise
        await aw(nested.commit())
        pool.harvest(result)
        return self.outcome(result, expected)

    def outcome(self, result: object, expected: object) -> tuple[str, bool]:
        cmd = self.query["cmd"]
        if cmd == ":copyfrom":
            return "ran", self.compare(expected, result)
        if cmd in {":exec", ":batchexec"}:
            return "ran", False
        # A batch returns each row's result, so it returned rows if any of them has one.
        if cmd in {":batchone", ":batchmany"}:
            items = typing.cast("list[object]", result)
            if cmd == ":batchmany":
                items = [item for each in items for item in typing.cast("list[object]", each)]
            return ("rows" if len(items) > 0 else "empty"), False

        if isinstance(result, sqlalchemy.CursorResult):
            result = result.rowcount
        match cmd:
            # `None` is no row or a row of one NULL, whichever the direct run found, so a value where there is none
            # still differs.
            case ":one" if result is None and expected == []:
                if self.raise_on_none:
                    message = f"{self.name}: returned None for a missing row with raise_on_none enabled"
                    raise AssertionError(message)
                returned: object = []
            case ":one":
                returned = [repr(flat(result))]
            case ":many":
                returned = [repr(flat(row)) for row in typing.cast("list[object]", result)]
            case _:
                returned = result

        # A `:one` returned its row, or `None` without one (else it raised).
        none: dict[str, object] = {
            ":one": None,
            ":many": [],
            ":execrows": 0,
            ":execresult": 0,
        }
        empty = cmd in none and result == none[cmd]
        return ("empty" if empty else "rows"), self.compare(expected, returned)

    def direct(
        self,
        connection: sqlalchemy.Connection,
        rows: list[dict[str, object]],
        enums: list[Labels],
    ) -> object:
        """sqlc's text, run twice with ``rows`` by psycopg's ``RawCursor`` and rolled back each time.

        Returns its rows as a list of their reprs or its row count, ``"unstable"`` when the two runs differ, or
        ``"unrunnable"`` when it raised.
        """
        parameters = sorted(zip(self.query["params"], self.arguments, strict=True), key=lambda pair: pair[0]["number"])
        positional = [[bound(parameter, row[arg.name]) for parameter, arg in parameters] for row in rows]
        runs: list[int | list[str]] = []
        for _ in range(2):
            nested = connection.begin_nested()
            try:
                with psycopg.RawCursor(driver(connection)) as cursor:
                    for info, labelled in enums:
                        register_enum(info, cursor, labelled)
                    if self.query["cmd"] == ":copyfrom":
                        cursor.executemany(self.query["text"], positional)
                        runs.append(cursor.rowcount)
                    else:
                        cursor.execute(self.query["text"], positional[0])
                        runs.append(
                            cursor.rowcount
                            if self.query["cmd"] == ":execrows"
                            else [repr(tuple(map(plain, row))) for row in cursor.fetchall()]
                        )
            except psycopg.Error:
                return "unrunnable"
            finally:
                nested.rollback()
        if not self.ordered:
            for run in runs:
                if isinstance(run, list):
                    run.sort()
        return runs[0] if runs[0] == runs[1] else "unstable"

    def compare(self, expected: object, returned: object) -> bool:
        """Whether we compared ``returned`` with ``expected`` from ``direct``, raising when they differ."""
        if expected is None or expected in ("unstable", "unrunnable"):
            return False
        if not self.ordered and isinstance(returned, list):
            returned = sorted(returned)
        if returned != expected:
            message = f"{self.name}: returned {returned!r}, but sqlc's SQL run directly returned {expected!r}"
            raise AssertionError(message)
        return True


def driver(connection: sqlalchemy.Connection) -> psycopg.Connection[Any]:
    raw = connection.connection.driver_connection
    if not isinstance(raw, psycopg.Connection):
        message = f"not a psycopg connection: {raw!r}"
        raise TypeError(message)
    return raw


Labels = tuple[EnumInfo, type[enum.Enum]]


def labels(connection: psycopg.Connection[Any], schema: str, name: str) -> Labels:
    """What psycopg needs to read the catalog's enum ``schema.name`` as its labels: its type, and an enum whose members
    are named and valued by them (psycopg's own numbers its values, and cannot have no members).
    """
    info = EnumInfo.fetch(connection, pgsql.Identifier(schema, name))
    if info is None:
        message = f"enum {schema}.{name} of the catalog is not in the database"
        raise AssertionError(message)
    # We go through `Any` as mypy only accepts an `Enum()` call with literal members.
    make: Any = enum.Enum
    labelled = make("Labels", {label: label for label in info.labels})
    return info, typing.cast("type[enum.Enum]", labelled)


def plain(value: object) -> object:
    """A value as psycopg reads it without the generated code: an enum as its label, which is its value in ours and in
    ``labels``', in a list too.
    """
    if isinstance(value, enum.Enum):
        return value.value
    if isinstance(value, list):
        return [plain(item) for item in value]

    # A range, SQLAlchemy's or psycopg's, becomes what makes it equal.
    if isinstance(value, (postgresql.Range, PgRange)):
        empty = value.isempty if isinstance(value, PgRange) else value.empty
        if empty:
            return ("range", "empty")
        return ("range", value.lower, value.upper, value.bounds)
    return value


def flat(result: object) -> tuple[object, ...]:
    """A generated result as the row it was read from: a record's fields in order, an embedded model's spread in place,
    or a bare value.
    """
    # SQLAlchemy's `Range` is a dataclass too, but one value.
    names = None if isinstance(result, (type, postgresql.Range)) else record_fields(result)
    if names is None:
        return (plain(result),)
    return tuple(value for name in names for value in flat(getattr(result, name)))


def bound(parameter: Json, value: object) -> object:
    """An argument as psycopg binds it for sqlc's ``parameter``: an enum as its label, and JSON wrapped, as psycopg
    binds no ``dict`` bare.
    """
    column = parameter["column"]
    depth = column.get("array_dims", 0)
    if value is None and (depth or not column["not_null"]):
        return None

    type_name = column["type"]["name"].removeprefix("pg_catalog.")
    if type_name in {"json", "jsonb"}:
        wrapper = Jsonb if type_name == "jsonb" else PgJson

        def document(element: object, depth: int) -> object:
            if depth == 0:
                return wrapper(element)
            return [
                None if child is None else document(child, depth - 1) for child in typing.cast("list[object]", element)
            ]

        return document(value, depth)
    if isinstance(value, postgresql.Range):
        if value.empty:
            return PgRange(empty=True)
        return PgRange(value.lower, value.upper, value.bounds)
    return plain(value)


def calls(
    connection: sqlalchemy.Connection,
    module: ModuleType,
    queries: list[Json],
    *,
    ordered_queries: set[str],
    raise_on_none: bool,
) -> list[Call]:
    """The functions of a query file's module, paired with its queries in sqlc's order."""
    functions = [
        value
        for name, value in vars(module).items()
        if inspect.isfunction(value) and value.__module__ == module.__name__ and not name.startswith("_")
    ]

    # A `:many` that only reads is followed by its stream, which we call as a function of its own
    streams = {before.__name__: stream for before, stream in itertools.pairwise(functions) if is_stream(stream)}
    functions = [function for function in functions if not is_stream(function)]
    if len(functions) != len(queries):
        message = f"{module.__name__}: {len(functions)} functions, {len(queries)} queries"
        raise AssertionError(message)

    found = []
    for function, query in zip(functions, queries, strict=True):
        hints = typing.get_type_hints(function)
        parameters = list(inspect.signature(function).parameters)[1:]
        record, bundled = None, parameters == ["params"]
        # A `:copyfrom` or a batch takes `rows` of its parameters, and a query of more than `max_params` one record of
        # them.
        if query["cmd"] == ":copyfrom" or query["cmd"].startswith(":batch"):
            (rows,) = parameters
            (record,) = typing.get_args(hints[rows])
        elif bundled:
            record = hints["params"]
        if record is not None:
            hints = typing.get_type_hints(record)
            parameters = record_fields(record) or []
        params = query.get("params", [])
        if len(parameters) != len(params):
            message = f"{function.__qualname__}: {len(parameters)} parameters, sqlc has {len(params)}"
            raise AssertionError(message)
        arguments = [
            argument(connection, name, hints[name], param, query["text"])
            for name, param in zip(parameters, params, strict=True)
        ]
        name = f"{module.__name__.split('.')[-1]}.{function.__name__}"
        call = checked(function, set())
        ordered = query["name"] in ordered_queries
        found.append(Call(name, call, query, arguments, record, bundled, ordered, raise_on_none))
        if (stream := streams.get(function.__name__)) is not None:
            name = f"{module.__name__.split('.')[-1]}.{stream.__name__}"
            found.append(Call(name, streamed(stream, set()), query, arguments, record, bundled, ordered, raise_on_none))
    return sorted(found, key=_writes_first)


def _writes_first(call: Call) -> int:
    if call.query["cmd"] == ":copyfrom":
        return 0
    verb = WRITERS.match(call.query["text"])
    return 1 if verb is None else 0 if verb[1].upper() == "INSERT" else 2


def argument(
    connection: sqlalchemy.Connection,
    name: str,
    hint: Any,
    parameter: Json,
    text: str,
) -> Argument:
    """Draw values for a parameter as sqlc typed it, in the annotation's form."""
    column = parameter["column"]
    table = (column.get("table") or {}).get("name", "")
    # For an equality lookup we want an existing value of the column, even when sqlc.arg(first) named
    # the parameter. For a bound such as age < @max_age the row's own age cannot select it.
    placeholder = rf"\${parameter['number']}\b"
    equal = re.search(rf"(?<![<>=!])=\s*{placeholder}|{placeholder}\s*=(?!=)", text)
    source = (column.get("original_name") if equal else None) or column["name"]
    keys = tuple(key for key in (f"{table}.{source}", source) if key != "")
    bare, nullable = strip_none(hint)
    # Both list annotations accept a list whose leaves are present. We draw from the arm that also admits NULL.
    arms = typing.get_args(bare)
    if typing.get_origin(bare) in (types.UnionType, typing.Union) and all(
        typing.get_origin(arm) is list for arm in arms
    ):
        bare = arms[0]
        hint = bare | None if nullable else bare
    dimensions, leaf = 0, bare
    while typing.get_origin(leaf) is list:
        (element,) = typing.get_args(leaf)
        leaf, _ = strip_none(element)
        dimensions += 1

    column_type = column.get("type") or {}
    resolved = pgvalues.resolve_name(
        connection, ".".join(part for part in (column_type.get("schema"), column_type.get("name")) if part)
    )
    if isinstance(leaf, type) and issubclass(leaf, enum.Enum):
        fresh = pgvalues.arrays(st.sampled_from(list(leaf)), dimensions)
    elif resolved is not None and (values := pgvalues.strategy(resolved, dimensions)) is not None:
        fresh = values
    elif leaf is object:
        fresh = pgvalues.arrays(pgvalues.json(), dimensions)
    else:
        fresh = pgvalues.arrays(st.from_type(leaf), dimensions)
    return Argument(name, hint, keys, fresh)
