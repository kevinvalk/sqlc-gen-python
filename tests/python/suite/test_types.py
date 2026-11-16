"""Overrides and renames, run against PostgreSQL.

`custom_psycopg` names an extension's type, a domain and one column through overrides, and renames a table and a
column. Every call is checked against its annotations, and every function must be called.
"""

import asyncio
import ipaddress
from decimal import Decimal
from typing import Any

import psycopg
import sqlalchemy
from postgres import conninfo, database
from suite.api import cases, functions, queries


def test_overrides_and_renames(server: sqlalchemy.URL) -> None:
    (case,) = (case for case in cases("custom") if case.name == "custom_psycopg")
    called: set[str] = set()
    with (
        database(server, case.schema_sql.read_text()) as url,
        psycopg.connect(conninfo(url)) as connection,
    ):
        asyncio.run(_custom(queries(case, connection, called)))
    assert called == functions(case)


async def _custom(api: Any) -> None:
    machine = await api.query.add_device(
        address=ipaddress.IPv4Address("10.0.0.7"),
        location="lab.rack1",
        budget=Decimal("12.50"),
    )
    assert type(machine) is api.models.Machine
    assert machine.address == ipaddress.IPv4Address("10.0.0.7")
    assert (machine.location, str(machine.budget)) == ("lab.rack1", "12.50")
    other = await api.query.add_device(address=ipaddress.IPv4Address("10.0.0.8"), location=None, budget=Decimal(1))
    assert other.location is None
    assert await api.query.list_devices_under(location="lab") == [machine]
    assert await api.query.list_devices_under(location="office") == []
    # NOTE: Ruff takes these names of an enum member and a query for secrets.
    assert machine.sign_in is api.models.SignIn.TOKEN
    assert await api.query.list_devices_by_token() == [machine, other]
