"""Overrides, renames and the types only psycopg maps, run against PostgreSQL.

`custom_psycopg` names an extension's type, a domain and one column through
overrides, and renames a table and a column; `pgtext_psycopg` and its async
twin map the types psycopg returns as their text. Every call is checked
against its annotations, and every function must be called.
"""

import asyncio
import ipaddress
from collections.abc import Iterator
from decimal import Decimal
from typing import Any

import psycopg
import pytest
import sqlalchemy
from postgres import conninfo, database

from conftest import SCHEMAS, Case, cases, functions, queries

# A value of each type `pgtext` maps, as PostgreSQL writes it back.
SHAPE = {
    "flags": "101",
    "mask": "10",
    "spot": "(1,2)",
    "edge": "{1,-1,0}",
    "segment": "[(0,0),(1,1)]",
    "frame": "(1,1),(0,0)",
    "route": "((0,0),(1,1))",
    "outline": "((0,0),(1,1),(1,0))",
    "area": "<(0,0),1>",
    "lsn": "16/B374D848",
}


@pytest.fixture
def schema_db(request: pytest.FixtureRequest, server: sqlalchemy.URL) -> Iterator[str]:
    """A fresh database of the example's schema, as a libpq URL."""
    example: str = request.param
    with database(server, (SCHEMAS / example / "schema.sql").read_text()) as url:
        yield conninfo(url)


@pytest.mark.parametrize("schema_db", ["custom"], indirect=True)
def test_overrides_and_renames(schema_db: str) -> None:
    (case,) = cases("custom")
    called: set[str] = set()
    with psycopg.connect(schema_db) as conn:
        api = queries(case, conn, called)
        asyncio.run(_custom(api))
    assert called == functions(case)


async def _custom(api: Any) -> None:  # noqa: ANN401
    machine = await api.query.add_device(
        address=ipaddress.IPv4Address("10.0.0.7"),
        location="lab.rack1",
        budget=Decimal("12.50"),
    )
    assert type(machine) is api.models.Machine
    assert machine.address == ipaddress.IPv4Address("10.0.0.7")
    assert (machine.location, str(machine.budget)) == ("lab.rack1", "12.50")
    other = await api.query.add_device(
        address=ipaddress.IPv4Address("10.0.0.8"), location=None, budget=Decimal(1)
    )
    assert other.location is None
    assert await api.query.list_devices_under(location="lab") == [machine]
    assert await api.query.list_devices_under(location="office") == []


@pytest.mark.parametrize("schema_db", ["pgtext"], indirect=True)
@pytest.mark.parametrize("case", cases("pgtext"), ids=lambda case: case.name)
def test_text_types_on_psycopg(case: Case, schema_db: str) -> None:
    called: set[str] = set()

    async def run() -> None:
        if case.is_async:
            async with await psycopg.AsyncConnection.connect(schema_db) as conn:
                await _pgtext(queries(case, conn, called))
        else:
            with psycopg.connect(schema_db) as conn:
                await _pgtext(queries(case, conn, called))

    asyncio.run(run())
    assert called == functions(case)


async def _pgtext(api: Any) -> None:  # noqa: ANN401
    shape = await api.query.add_shape(**SHAPE)
    assert {name: getattr(shape, name) for name in SHAPE} == SHAPE
    assert await api.query.get_shape(id=shape.id) == shape
