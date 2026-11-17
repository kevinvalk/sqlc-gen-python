"""Seeding a table in the runtime sweep (see ``autoprop``)."""

from __future__ import annotations

from typing import TYPE_CHECKING

import pydantic
import sqlalchemy
from hypothesis import strategies as st

import pgvalues
from autoprop_pool import PICK
from pgvalues import enum_array_type, json_type, sqlalchemy_type
from suite.deep import build, check, record_fields, refused

if TYPE_CHECKING:
    from collections.abc import Sequence

    from autoprop_pool import Pool
    from pgvalues import Column, Table


def read(table: Table, row: Sequence[object]) -> object:
    """Read a row through its model's ``from_row``, which must agree with reading it by position and inhabit the
    model's annotations.
    """
    where = f"{table.model.__name__}.from_row on {table.sql}"
    try:
        record = table.model.from_row(row)
    except pydantic.ValidationError as error:
        refused(error, table.model, where)
    check(record, table.model, where)
    if not same(record, build(table.model, row)):
        message = f"{where}: {record!r} is not the row read by position"
        raise AssertionError(message)
    return record


def same(left: object, right: object) -> bool:
    """Whether two records hold the same values field by field, a value being the same as itself.

    We need this as a NaN is not equal to itself, and since Python 3.13 a dataclass compares its fields by ``==`` alone.
    """
    names = record_fields(left)
    if names is not None and type(left) is type(right):
        return all(same(getattr(left, name), getattr(right, name)) for name in names)
    return left is right or left == right


def draw_row(data: st.DataObject, table: Table, pool: Pool) -> dict[str, object]:
    """Draw a row's values: foreign keys from rows seeded before, and defaults left out at random.

    We make every draw whatever the pool holds (see ``PICK``).
    """
    values: dict[str, object] = {}
    for column in table.columns:
        if column.generated:
            continue
        default = column.optional and data.draw(st.booleans(), label=f"{column.name} default")
        value = data.draw(_seeds(column), label=column.name)
        if not default:
            values[column.name] = value

    for columns, referenced, keys in table.references:
        i = data.draw(PICK, label=f"{table.sql} -> {referenced}")
        rows = pool.rows[referenced]
        if len(rows) > 0:
            row = rows[i % len(rows)]
            values |= dict(zip(columns, (row[key] for key in keys), strict=True))
    return values


def _seeds(column: Column) -> st.SearchStrategy[object]:
    values = pgvalues.strategy(column.pg)
    if values is None:
        return st.none()  # A type nobody draws yet, so only NULL or a refusal
    return st.none() | values if column.nullable else values


def insert(
    table: Table, values: dict[str, object], *, ctid: bool = False
) -> tuple[sqlalchemy.TextualSelect, dict[str, object]]:
    """``INSERT ... RETURNING *``, typed where a driver needs it, and its parameters bound as ``p0``, ``p1``, ...
    whatever the column names. With ``ctid`` it returns the row's ``ctid`` after it too, to read it again by.
    """
    types = {column.name: column.pg for column in table.columns}
    names = ", ".join('"{}"'.format(name.replace('"', '""')) for name in values)
    places = ", ".join(f":p{i}" for i in range(len(values)))
    body = f"({names}) VALUES ({places})" if len(values) > 0 else "DEFAULT VALUES"
    returning = "*, ctid::text" if ctid else "*"
    statement = sqlalchemy.text(f"INSERT INTO {table.sql} {body} RETURNING {returning}")
    binds = [
        sqlalchemy.bindparam(f"p{i}", type_=json_type(types[name]))
        for i, name in enumerate(values)
        if types[name].name in {"json", "jsonb"}
    ] + [
        sqlalchemy.bindparam(f"p{i}", type_=ranged)
        for i, name in enumerate(values)
        if (ranged := sqlalchemy_type(types[name])) is not None
    ]
    if len(binds) > 0:
        statement = statement.bindparams(*binds)

    columns = [
        sqlalchemy.column(column.name, enum_array_type(column.pg) or sqlalchemy_type(column.pg))
        for column in table.columns
    ]
    typed = statement.columns(*columns, *[sqlalchemy.column("ctid")] * ctid)
    return typed, {f"p{i}": value for i, value in enumerate(values.values())}
