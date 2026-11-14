"""Seeding a table in the runtime sweep (see ``autoprop``).

``draw_row`` draws a row for a table and ``insert`` writes the statement that
stores it; ``read`` reads what the database returned through the table's
generated model, which must agree with the row read by position.
"""

from __future__ import annotations

import dataclasses
from typing import TYPE_CHECKING

import sqlalchemy
from hypothesis import strategies as st

import pgvalues
from autoprop_pool import PICK
from pgvalues import enum_array_type, json_type, sqlalchemy_type
from suite.deep import build, check

if TYPE_CHECKING:
    from collections.abc import Sequence

    from autoprop_pool import Pool
    from pgvalues import Column, Table


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
    places = ", ".join(f":p{i}" for i in range(len(values)))
    body = f"({names}) VALUES ({places})" if values else "DEFAULT VALUES"
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
    if binds:
        statement = statement.bindparams(*binds)
    columns = [
        sqlalchemy.column(c.name, enum_array_type(c.pg) or sqlalchemy_type(c.pg))
        for c in table.columns
    ]
    typed = statement.columns(*columns, *[sqlalchemy.column("ctid")] * ctid)
    return typed, {f"p{i}": value for i, value in enumerate(values.values())}
