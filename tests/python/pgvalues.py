"""PostgreSQL types and tables as a database reports them, and values each
type accepts.

``resolve`` reads a type from the catalog: domains become their base type,
arrays their element type and depth, enums their labels. ``tables`` reads every
user table, its columns and foreign keys, and the generated model that reads
it. ``strategy`` is a Hypothesis strategy of Python values that PostgreSQL
accepts for such a type and that bind through psycopg and asyncpg as the type
map's annotation (docs/python-output.md §5). It draws ordinary values; the
edges (NaN, infinity, extremes) are ``typemap.py``'s concern. ``json_type``
and ``enum_array_type`` are how SQLAlchemy must bind or read a type, as the
generated code does (§6).
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

if TYPE_CHECKING:
    from types import ModuleType

    from hypothesis.strategies import SearchStrategy

# The catalog row of a type, with an enum's labels in their order.
TYPE_ROW = sqlalchemy.text("""
    SELECT t.typname, t.typtype, t.typcategory, t.typelem, t.typbasetype,
           t.typtypmod,
           ARRAY(SELECT e.enumlabel::text FROM pg_enum e WHERE e.enumtypid = t.oid
                 ORDER BY e.enumsortorder)
    FROM pg_type t WHERE t.oid = :oid
""")


@dataclass(frozen=True)
class Pg:
    """A type as the database knows it: the base type's name, its array depth,
    an enum's labels, and the modifier that bounds a length or precision.
    """

    name: str
    dims: int = 0
    labels: tuple[str, ...] = ()
    typmod: int = -1


def resolve(conn: sqlalchemy.Connection, oid: int, typmod: int = -1) -> Pg:
    """Read the type ``oid`` from the catalog, through domains and arrays."""
    dims = 0
    while True:
        name, kind, category, element, base, base_typmod, labels = conn.execute(
            TYPE_ROW, {"oid": oid}
        ).one()
        if kind == "d":
            oid, typmod = base, typmod if typmod != -1 else base_typmod
        elif category == "A" and element:
            oid, dims = element, dims + 1
        else:
            return Pg(name, dims, tuple(labels), typmod)


def resolve_name(conn: sqlalchemy.Connection, name: str) -> Pg | None:
    """Resolve a type as sqlc spells it (``pg_catalog.int4``, ``serial``,
    ``billing.payment_method``, or a name that needs quoting, ``weird enum``).
    """
    name = SERIALS.get(name.removeprefix("pg_catalog."), name)
    if not name:
        return None
    # A declared name, as `schema.name` or `name`, quoted part by part; else
    # SQL's own spelling (`double precision`), which takes no quotes.
    found = conn.execute(
        sqlalchemy.text("""
            SELECT coalesce(
                to_regtype(array_to_string(
                    ARRAY(SELECT quote_ident(part)
                          FROM unnest(string_to_array(:name, '.')) part), '.')),
                to_regtype(:name))::oid
        """),
        {"name": name},
    ).scalar_one_or_none()
    return None if found is None else resolve(conn, found)


@dataclass(frozen=True)
class Column:
    """A table column as the database declares it."""

    name: str
    pg: Pg
    nullable: bool
    # Has a default, so an insert may leave it out.
    optional: bool
    # Generated, or an identity column that refuses values.
    generated: bool


@dataclass(frozen=True)
class Table:
    """A table to seed, its foreign keys, and the model that reads it."""

    name: str
    sql: str
    columns: list[Column]
    # Each key: its columns, the table it references and the columns there.
    references: list[tuple[list[str], str, list[str]]]
    model: Any


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
KEYS = sqlalchemy.text(
    "SELECT conrelid, conkey, confrelid, confkey FROM pg_constraint WHERE contype = 'f'"
)


def tables(conn: sqlalchemy.Connection, models: ModuleType | None) -> list[Table]:
    """Every user table, its columns and foreign keys, in seeding order."""
    relations = conn.execute(TABLES).all()
    sql = {oid: qualified for oid, _, qualified in relations}
    columns = {
        oid: {
            num: Column(name, column_type(conn, typ, typmod, ndims), *flags)
            for num, name, typ, typmod, ndims, *flags in conn.execute(
                COLUMNS, {"oid": oid}
            )
        }
        for oid in sql
    }
    references = defaultdict(list)
    for oid, keys, other, other_keys in conn.execute(KEYS):
        if oid in sql and other in sql:
            names = [columns[oid][k].name for k in keys]
            other_names = [columns[other][k].name for k in other_keys]
            references[oid].append((names, sql[other], other_names))
    found = [
        Table(name, sql[oid], cs, references[oid], model_for(models, cs))
        for oid, name, _ in relations
        if (cs := [*columns[oid].values()])  # `RETURNING *` needs a column
    ]
    return seed_order(found)


def column_type(conn: sqlalchemy.Connection, oid: int, typmod: int, ndims: int) -> Pg:
    """A column's type, read from the catalog; its declared array depth wins."""
    pg = resolve(conn, oid, typmod)
    return dataclasses.replace(pg, dims=max(pg.dims, ndims)) if pg.dims else pg


def seed_order(found: list[Table]) -> list[Table]:
    """Referenced tables first; tables in a cycle in name order, last."""
    ordered: list[Table] = []
    pending = list(found)
    while pending:
        done = {t.sql for t in ordered}
        ready = [
            t
            for t in pending
            if all(r in done or r == t.sql for _, r, _ in t.references)
        ] or pending[:1]
        ordered += ready
        pending = [t for t in pending if t not in ready]
    return ordered


def model_for(models: ModuleType | None, columns: list[Column]) -> Any:  # noqa: ANN401
    """The model whose fields are these columns, or ``None`` if there is none,
    or several that read them differently.
    """
    if models is None:
        return None
    names = [_bare(c.name) for c in columns]
    candidates = [
        cls
        for cls in vars(models).values()
        if isinstance(cls, type)
        and dataclasses.is_dataclass(cls)
        and [_bare(f.name) for f in dataclasses.fields(cls)] == names
    ]
    readings = {repr(list(typing.get_type_hints(c).values())) for c in candidates}
    return candidates[0] if len(readings) == 1 else None


def _bare(name: str) -> str:
    """A name without case or separators, the same in SQL and Python."""
    return re.sub(r"[^0-9a-z]", "", name.lower())


def json_type(pg: Pg) -> Any:  # noqa: ANN401
    """What a JSON parameter must be bound as (§6, typed statements)."""
    base = (postgresql.JSONB if pg.name == "jsonb" else postgresql.JSON)(
        none_as_null=True
    )
    return postgresql.ARRAY(base, dimensions=pg.dims) if pg.dims else base


def enum_array_type(pg: Pg) -> Any:  # noqa: ANN401
    """How an enum array result must be read for psycopg to make it a list (§6)."""
    if not (pg.labels and pg.dims):
        return None
    return postgresql.ARRAY(sqlalchemy.Enum(*pg.labels), dimensions=pg.dims)


# A length's or precision's type modifier counts from this header size.
VARHDRSZ = 4
# Pseudo-types only a column declaration knows.
SERIALS = {
    "smallserial": "int2",
    "serial2": "int2",
    "serial": "int4",
    "serial4": "int4",
    "bigserial": "int8",
    "serial8": "int8",
}


def strategy(pg: Pg, dims: int | None = None) -> SearchStrategy[object] | None:
    """Values of ``pg``, as arrays ``dims`` deep (its own depth by default),
    or ``None`` if nobody has said what the type's values are.
    """
    scalar = st.sampled_from(pg.labels) if pg.labels else _scalar(pg)
    if scalar is None:
        return None
    return arrays(scalar, pg.dims if dims is None else dims)


def arrays(element: SearchStrategy[object], dims: int) -> SearchStrategy[object]:
    """Rectangular arrays ``dims`` deep, as PostgreSQL requires; no NULL items."""
    if not dims:
        return element

    def nest(shape: list[int]) -> SearchStrategy[object]:
        if not shape:
            return element
        size, *rest = shape
        return st.lists(nest(rest), min_size=size, max_size=size)

    return st.lists(st.integers(0, 3), min_size=dims, max_size=dims).flatmap(nest)


def _scalar(pg: Pg) -> SearchStrategy[object] | None:
    length = (
        pg.typmod - VARHDRSZ
        if pg.name in {"bpchar", "varchar"} and pg.typmod > 0
        else 8
    )
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
    """Strings PostgreSQL stores: any character but NUL and lone surrogates."""
    alphabet = st.characters(codec="utf-8", exclude_characters="\x00")
    return st.text(alphabet, max_size=max_size)


def ints(bits: int) -> SearchStrategy[object]:
    """Signed integers of a width."""
    return st.integers(-(2 ** (bits - 1)), 2 ** (bits - 1) - 1)


def json() -> SearchStrategy[object]:
    """JSON documents, as Python values both drivers encode."""
    leaves = (
        st.none()
        | st.booleans()
        | ints(53)
        | st.floats(allow_nan=False, allow_infinity=False)
        | text()
    )
    return st.recursive(
        leaves,
        lambda inner: (
            st.lists(inner, max_size=3) | st.dictionaries(text(), inner, max_size=3)
        ),
        max_leaves=6,
    )


def offsets() -> SearchStrategy[dt.timezone]:
    """Fixed UTC offsets, to the second, within PostgreSQL's ±15:59:59."""
    limit = 16 * 3600 - 1
    return st.integers(-limit, limit).map(
        lambda s: dt.timezone(dt.timedelta(seconds=s))
    )


def hexbytes(size: int) -> SearchStrategy[object]:
    """MAC addresses of ``size`` bytes, as PostgreSQL writes them."""
    return st.binary(min_size=size, max_size=size).map(
        lambda raw: ":".join(f"{b:02x}" for b in raw)
    )


def interfaces() -> SearchStrategy[ipaddress.IPv4Interface | ipaddress.IPv6Interface]:
    """Addresses with a prefix, of either IP version."""
    return st.builds(
        lambda address, prefix: ipaddress.ip_interface(
            (address, prefix % (address.max_prefixlen + 1))
        ),
        st.ip_addresses(),
        st.integers(0, 128),
    )


TEXT = {"text", "varchar", "bpchar", "name", "citext"}
SCALARS: dict[str, SearchStrategy[object]] = {
    "bool": st.booleans(),
    "int2": ints(16),
    "int4": ints(32),
    "int8": ints(64),
    "oid": st.integers(0, 2**32 - 1),
    "float4": st.floats(width=32),
    "float8": st.floats(),
    "numeric": st.decimals(allow_nan=False, allow_infinity=False, places=4).filter(
        lambda d: abs(d) < 10**20
    ),
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
    "tsvector": st.lists(st.from_regex(r"[a-z]{1,6}", fullmatch=True), max_size=4).map(
        " ".join
    ),
}
