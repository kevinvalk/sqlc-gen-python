"""PostgreSQL types and tables as a database reports them, and Hypothesis strategies of the values each type accepts.

A strategy draws Python values that PostgreSQL accepts for a type and that bind through psycopg and asyncpg as the type
map's annotation. It only draws ordinary values, as ``typemap.py`` probes the edges (NaN,
infinity, extremes).
"""

from __future__ import annotations

import dataclasses
import datetime as dt
import ipaddress
import re
import typing
from collections import defaultdict
from dataclasses import dataclass
from decimal import Decimal
from typing import TYPE_CHECKING, Any

import sqlalchemy
from hypothesis import strategies as st
from sqlalchemy.dialects import postgresql

from suite.deep import record_fields

if TYPE_CHECKING:
    from types import ModuleType

    from hypothesis.strategies import SearchStrategy

# The catalog row of a type, with an enum's labels in their order
TYPE_ROW = sqlalchemy.text("""
    SELECT t.typname, t.typtype, t.typcategory, t.typelem, t.typbasetype,
           t.typtypmod,
           ARRAY(SELECT e.enumlabel::text FROM pg_enum e WHERE e.enumtypid = t.oid
                 ORDER BY e.enumsortorder)
    FROM pg_type t WHERE t.oid = :oid
""")


@dataclass(frozen=True)
class Pg:
    """A type as the database knows it, with domains resolved to their base type and arrays to their element type."""

    name: str
    dims: int = 0
    labels: tuple[str, ...] = ()  # An enum's labels
    typmod: int = -1  # The modifier that bounds a length or precision


def resolve(connection: sqlalchemy.Connection, oid: int, typmod: int = -1) -> Pg:
    dims = 0
    while True:
        name, kind, category, element, base, base_typmod, labels = connection.execute(TYPE_ROW, {"oid": oid}).one()
        if kind == "d":
            oid, typmod = base, typmod if typmod != -1 else base_typmod
        elif category == "A" and element != 0:
            oid, dims = element, dims + 1
        else:
            return Pg(name, dims, tuple(labels), typmod)


def resolve_name(connection: sqlalchemy.Connection, name: str) -> Pg | None:
    """Resolve a type as sqlc spells it: ``pg_catalog.int4``, ``serial``, ``billing.payment_method``, or a name that
    needs quoting such as ``weird enum``.
    """
    name = SERIALS.get(name.removeprefix("pg_catalog."), name)
    if name == "":
        return None

    # A declared name (`schema.name` or `name`) is quoted part by part, else we try SQL's own spelling (`double
    # precision`), which takes no quotes.
    found = connection.execute(
        sqlalchemy.text("""
            SELECT coalesce(
                to_regtype(array_to_string(
                    ARRAY(SELECT quote_ident(part)
                          FROM unnest(string_to_array(:name, '.')) part), '.')),
                to_regtype(:name))::oid
        """),
        {"name": name},
    ).scalar_one_or_none()
    return None if found is None else resolve(connection, found)


@dataclass(frozen=True)
class Column:
    name: str
    pg: Pg
    nullable: bool
    optional: bool  # Has a default, so an insert may leave it out
    generated: bool  # Generated, or an identity column that refuses values


@dataclass(frozen=True)
class Table:
    name: str
    sql: str
    columns: list[Column]
    # Each foreign key as its columns, the table it references and the columns there.
    references: list[tuple[list[str], str, list[str]]]
    model: Any  # The generated model that reads the table


TABLES = sqlalchemy.text("""
    SELECT c.oid, c.relname, quote_ident(n.nspname) || '.' || quote_ident(c.relname)
    FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
    WHERE c.relkind IN ('r', 'p') AND NOT c.relispartition
      AND n.nspname NOT IN ('pg_catalog', 'information_schema')
      AND n.nspname NOT LIKE 'pg\\_%'
      AND NOT EXISTS (SELECT FROM pg_depend d
                      WHERE d.objid = c.oid AND d.deptype = 'e')
    ORDER BY 3
""")
COLUMNS = sqlalchemy.text("""
    SELECT attnum, attname, atttypid, atttypmod, attndims, NOT attnotnull,
           atthasdef OR attidentity = 'd', attidentity = 'a' OR attgenerated <> ''
    FROM pg_attribute WHERE attrelid = :oid AND attnum > 0 AND NOT attisdropped
    ORDER BY attnum
""")
KEYS = sqlalchemy.text("SELECT conrelid, conkey, confrelid, confkey FROM pg_constraint WHERE contype = 'f'")


def tables(connection: sqlalchemy.Connection, models: ModuleType | None) -> list[Table]:
    """Every user table with its columns and foreign keys, in seeding order."""
    relations = connection.execute(TABLES).all()
    sql = {oid: qualified for oid, _, qualified in relations}
    columns = {
        oid: {
            number: Column(name, column_type(connection, type_oid, typmod, ndims), *flags)
            for number, name, type_oid, typmod, ndims, *flags in connection.execute(COLUMNS, {"oid": oid})
        }
        for oid in sql
    }

    references = defaultdict(list)
    for oid, keys, other, other_keys in connection.execute(KEYS):
        if oid in sql and other in sql:
            names = [columns[oid][key].name for key in keys]
            other_names = [columns[other][key].name for key in other_keys]
            references[oid].append((names, sql[other], other_names))

    found = [
        Table(name, sql[oid], table_columns, references[oid], model_for(models, table_columns))
        for oid, name, _ in relations
        if len(table_columns := [*columns[oid].values()]) > 0  # `RETURNING *` needs a column
    ]
    return seed_order(found)


def column_type(connection: sqlalchemy.Connection, oid: int, typmod: int, ndims: int) -> Pg:
    pg = resolve(connection, oid, typmod)
    # The column's declared array depth wins over its type's.
    return dataclasses.replace(pg, dims=max(pg.dims, ndims)) if pg.dims > 0 else pg


def seed_order(found: list[Table]) -> list[Table]:
    """Order tables so that a referenced table comes first, and tables in a cycle come last in name order."""
    ordered: list[Table] = []
    pending = list(found)
    while len(pending) > 0:
        done = {t.sql for t in ordered}
        ready = [t for t in pending if all(r in done or r == t.sql for _, r, _ in t.references)] or pending[:1]
        ordered += ready
        pending = [t for t in pending if t not in ready]
    return ordered


def model_for(models: ModuleType | None, columns: list[Column]) -> Any:
    """Find the model whose fields are these columns, or ``None`` when there is none or several that read them
    differently.
    """
    if models is None:
        return None

    names = [_bare(c.name) for c in columns]
    candidates = [
        model
        for model in vars(models).values()
        if isinstance(model, type)
        and (fields := record_fields(model)) is not None
        and [_bare(name) for name in fields] == names
    ]
    readings = {repr(list(typing.get_type_hints(c).values())) for c in candidates}
    return candidates[0] if len(readings) == 1 else None


def _bare(name: str) -> str:
    # Without case or separators, a name is the same in SQL and Python.
    return re.sub(r"[^0-9a-z]", "", name.lower())


def json_type(pg: Pg) -> Any:
    """What a JSON parameter must be bound as."""
    base = (postgresql.JSONB if pg.name == "jsonb" else postgresql.JSON)(none_as_null=True)
    return postgresql.ARRAY(base, dimensions=pg.dims) if pg.dims > 0 else base


# SQLAlchemy's type of each range makes every driver's range its `Range`, and its `hstore` makes every driver's a
# `dict`.
LIBRARY_TYPES: dict[str, Any] = {
    "int4range": postgresql.INT4RANGE,
    "int8range": postgresql.INT8RANGE,
    "numrange": postgresql.NUMRANGE,
    "daterange": postgresql.DATERANGE,
    "tsrange": postgresql.TSRANGE,
    "tstzrange": postgresql.TSTZRANGE,
    "hstore": postgresql.HSTORE,
}


def sqlalchemy_type(pg: Pg) -> Any:
    """How a range or an `hstore` must be bound and read, as the generated code does, so that every driver's value
    is SQLAlchemy's.
    """
    library_type = LIBRARY_TYPES.get(pg.name)
    return library_type() if library_type is not None and pg.dims <= 0 else None


def enum_array_type(pg: Pg) -> Any:
    """How an enum array result must be read for psycopg to make it a list."""
    if not (len(pg.labels) > 0 and pg.dims > 0):
        return None

    return postgresql.ARRAY(sqlalchemy.Enum(*pg.labels), dimensions=pg.dims)


VARHDRSZ = 4  # PostgreSQL counts a length's or precision's type modifier from this header size

# Pseudo-types only a column declaration knows
SERIALS = {
    "smallserial": "int2",
    "serial2": "int2",
    "serial": "int4",
    "serial4": "int4",
    "bigserial": "int8",
    "serial8": "int8",
}


def strategy(pg: Pg, dims: int | None = None) -> SearchStrategy[object] | None:
    """Values of ``pg`` as arrays ``dims`` deep (its own depth by default), or ``None`` when nobody has said what the
    type's values are.
    """
    scalar = st.sampled_from(pg.labels) if len(pg.labels) > 0 else _scalar(pg)
    if scalar is None:
        return None
    return arrays(scalar, pg.dims if dims is None else dims)


def arrays(element: SearchStrategy[object], dims: int) -> SearchStrategy[object]:
    """Rectangular arrays ``dims`` deep, as PostgreSQL requires, without NULL items."""
    if dims <= 0:
        return element

    def nest(shape: list[int]) -> SearchStrategy[object]:
        if len(shape) <= 0:
            return element
        size, *rest = shape
        return st.lists(nest(rest), min_size=size, max_size=size)

    return st.lists(st.integers(0, 3), min_size=dims, max_size=dims).flatmap(nest)


def _scalar(pg: Pg) -> SearchStrategy[object] | None:
    length = pg.typmod - VARHDRSZ if pg.name in {"bpchar", "varchar"} and pg.typmod > 0 else 8
    if pg.name == "bpchar" and pg.typmod == -1:
        length = 1  # `char` is `char(1)`
    if pg.name in TEXT:
        return text(max_size=length)
    if pg.name == "numeric" and pg.typmod > 0:
        precision, scale = (pg.typmod - VARHDRSZ) >> 16, (pg.typmod - VARHDRSZ) & 0xFFFF
        bound = Decimal(10) ** (precision - scale) - 1
        return st.decimals(-bound, bound, places=scale)
    return SCALARS.get(pg.name)


def text(max_size: int = 8) -> SearchStrategy[str]:
    # PostgreSQL stores any character but NUL and lone surrogates.
    alphabet = st.characters(codec="utf-8", exclude_characters="\x00")
    return st.text(alphabet, max_size=max_size)


def ints(bits: int) -> SearchStrategy[object]:
    return st.integers(-(2 ** (bits - 1)), 2 ** (bits - 1) - 1)


def json() -> SearchStrategy[object]:
    leaves = st.none() | st.booleans() | ints(53) | st.floats(allow_nan=False, allow_infinity=False) | text()
    return st.recursive(
        leaves,
        lambda inner: st.lists(inner, max_size=3) | st.dictionaries(text(), inner, max_size=3),
        max_leaves=6,
    )


def offsets() -> SearchStrategy[dt.timezone]:
    limit = 16 * 3600 - 1  # PostgreSQL's ±15:59:59
    return st.integers(-limit, limit).map(lambda s: dt.timezone(dt.timedelta(seconds=s)))


def hexbytes(size: int) -> SearchStrategy[object]:
    """MAC addresses of ``size`` bytes, as PostgreSQL writes them."""
    return st.binary(min_size=size, max_size=size).map(lambda raw: ":".join(f"{b:02x}" for b in raw))


def interfaces() -> SearchStrategy[ipaddress.IPv4Interface | ipaddress.IPv6Interface]:
    return st.builds(
        lambda address, prefix: ipaddress.ip_interface((address, prefix % (address.max_prefixlen + 1))),
        st.ip_addresses(),
        st.integers(0, 128),
    )


def ranges(bounds: SearchStrategy[Any]) -> SearchStrategy[object]:
    """SQLAlchemy ranges of ``bounds``: empty, or ordered with either end open."""
    ends = st.none() | bounds
    shapes = st.sampled_from(["[)", "[]", "()", "(]"])

    def make(lower: Any, upper: Any, shape: str) -> object:
        if lower is not None and upper is not None and lower > upper:
            lower, upper = upper, lower
        return postgresql.Range(lower, upper, bounds=shape)  # type: ignore[arg-type]

    return st.just(postgresql.Range(empty=True)) | st.builds(make, ends, ends, shapes)


TEXT = {"text", "varchar", "bpchar", "name", "citext"}
SCALARS: dict[str, SearchStrategy[object]] = {
    "bool": st.booleans(),
    "int2": ints(16),
    "int4": ints(32),
    "int8": ints(64),
    "oid": st.integers(0, 2**32 - 1),
    "float4": st.floats(width=32),
    "float8": st.floats(),
    "numeric": st.decimals(allow_nan=False, allow_infinity=False, places=4).filter(lambda d: abs(d) < 10**20),
    "money": st.decimals(-(10**12), 10**12, places=2).map(str),
    "bytea": st.binary(max_size=16),
    "uuid": st.uuids(),
    "date": st.dates(),
    "time": st.times(),
    "timetz": st.times(timezones=offsets()),
    "timestamp": st.datetimes(),
    # Away from year 1 and 9999, where an offset leaves Python's range.
    "timestamptz": st.datetimes(
        dt.datetime(1900, 1, 1),  # noqa: DTZ001 -- Hypothesis takes naive bounds
        dt.datetime(2200, 1, 1),  # noqa: DTZ001
        timezones=offsets(),
    ),
    "interval": st.timedeltas(dt.timedelta(days=-(10**6)), dt.timedelta(days=10**6)),
    "json": json(),
    "jsonb": json(),
    "inet": st.ip_addresses() | interfaces(),
    "cidr": interfaces().map(lambda interface: interface.network),
    "macaddr": hexbytes(6),
    "macaddr8": hexbytes(8),
    "xml": st.sampled_from(["<a/>", "<b>ü &amp; ß</b>", "plain text", ""]),
    "tsvector": st.lists(st.from_regex(r"[a-z]{1,6}", fullmatch=True), max_size=4).map(" ".join),
    "hstore": st.dictionaries(text(), st.none() | text(), max_size=3),
    "ltree": st.from_regex(r"[A-Za-z0-9_]{1,5}(\.[A-Za-z0-9_]{1,5}){0,3}", fullmatch=True),
    "regclass": st.sampled_from(["pg_class", "pg_type", "pg_namespace"]),
    "int4range": ranges(ints(31)),
    "int8range": ranges(ints(63)),
    "numrange": ranges(st.decimals(allow_nan=False, allow_infinity=False, places=2).filter(lambda d: abs(d) < 10**12)),
    "daterange": ranges(st.dates(dt.date(1900, 1, 1), dt.date(2200, 1, 1))),
    "tsrange": ranges(st.datetimes(dt.datetime(1900, 1, 1), dt.datetime(2200, 1, 1))),  # noqa: DTZ001
    "tstzrange": ranges(
        st.datetimes(
            dt.datetime(1900, 1, 1),  # noqa: DTZ001
            dt.datetime(2200, 1, 1),  # noqa: DTZ001
            timezones=offsets(),
        )
    ),
}
