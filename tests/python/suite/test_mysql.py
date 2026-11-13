"""Every generated function of the billing fixture, run against MySQL.

One scenario drives the billing API, through `billing_pymysql` on PyMySQL,
`billing_sqlalchemy` on `mysql+pymysql`, and `billing_sqlalchemy_async` on
`mysql+asyncmy`, each on a fresh database of the newest
long-term MySQL (`mysql.IMAGE`, or `SQLC_TEST_MYSQL_URL`). As in
`test_runtime.py`, every call is checked against its annotations, and every
function must be called.
"""

import asyncio
import datetime as dt
import importlib
import json
from collections.abc import Iterator
from decimal import Decimal
from typing import Any

import mysql
import pytest
import sqlalchemy
from examples import GoldenCase
from postgres import aw
from sqlalchemy.ext.asyncio import create_async_engine

from conftest import cases, functions, queries

SCHEMA = cases("billing")[0].schema_sql.read_text()
# Characters PyMySQL's `%` formatting, SQLAlchemy's `:name` binds or a careless
# generator could misread.
AWKWARD = 'it\'s "quoted" \\ :name %s %(x)s %% ? /*SLICE:ids*/?'
# Ids beyond a signed BIGINT, so that an unsigned one is read as itself.
FIRST_ID = 2**63 + 5

RUNS = [
    pytest.param(case, driver, id=f"{case.name}-{driver}")
    for case in cases("billing")
    # Not `mysql+aiomysql`: aiomysql 0.3.2 cannot bind `bytes` on PyMySQL 1.2,
    # which removed the `escape_bytes_prefixed` it imports.
    for driver in (("asyncmy",) if case.is_async else ("pymysql",))
]


@pytest.fixture(scope="session")
def mysql_server() -> Iterator[sqlalchemy.URL]:
    try:
        with mysql.server() as url:
            yield url
    except SystemExit as error:  # end the session once, not fail every test
        pytest.exit(str(error), returncode=1)


@pytest.mark.parametrize(("case", "driver"), RUNS)
def test_every_function_against_mysql(
    case: GoldenCase, driver: str, mysql_server: sqlalchemy.URL
) -> None:
    called: set[str] = set()
    with mysql.database(mysql_server, SCHEMA) as database:
        _setup(database)
        url = database.set(drivername=f"mysql+{driver}")
        if case.backend == "pymysql":
            _pymysql(case, url, called)
        elif case.is_async:
            asyncio.run(_sqlalchemy_async(case, url, called))
        else:
            _sqlalchemy_sync(case, url, called)
    public = functions(case)
    assert called == public, f"never called: {sorted(public - called)}"


def _setup(url: sqlalchemy.URL) -> None:
    """Start the ids where only an unsigned BIGINT holds them."""
    conn = mysql.connect(url)
    try:
        with conn.cursor() as cur:
            for table in ("customers", "invoices"):
                cur.execute(f"ALTER TABLE {table} AUTO_INCREMENT = {FIRST_ID}")
    finally:
        conn.close()


def _pymysql(case: GoldenCase, url: sqlalchemy.URL, called: set[str]) -> None:
    conn = mysql.connect(url)
    try:
        asyncio.run(scenario(_api(case, conn, called)))
        conn.rollback()
    finally:
        conn.close()


def _sqlalchemy_sync(case: GoldenCase, url: sqlalchemy.URL, called: set[str]) -> None:
    engine = sqlalchemy.create_engine(url)
    try:
        with engine.connect() as conn:
            asyncio.run(scenario(_api(case, conn, called)))
            conn.rollback()
    finally:
        engine.dispose()


async def _sqlalchemy_async(
    case: GoldenCase, url: sqlalchemy.URL, called: set[str]
) -> None:
    engine = create_async_engine(url)
    try:
        async with engine.connect() as conn:
            await scenario(_api(case, conn, called))
            await conn.rollback()
    finally:
        await engine.dispose()


def _api(case: GoldenCase, conn: object, called: set[str]) -> Any:  # noqa: ANN401
    api = queries(case, conn, called)
    if case.backend == "sqlalchemy":
        api.NoRow = sqlalchemy.exc.NoResultFound
        api.MultipleRows = sqlalchemy.exc.MultipleResultsFound
    else:
        package = importlib.import_module(case.name)
        api.NoRow, api.MultipleRows = package.NoRowError, package.MultipleRowsError
    return api


async def scenario(api: Any) -> None:  # noqa: ANN401, PLR0915
    m, q = api.models, api.query
    metadata = '{"vip": true, "note": "50%: \'ok\'"}'
    acme = await q.create_customer(
        name=AWKWARD,
        email=None,
        vip=True,
        preferred_method=m.CustomersPreferredMethod.BANK_TRANSFER,
        metadata=metadata,
    )
    assert acme >= FIRST_ID
    customer = await q.get_customer(id=acme)
    assert (customer.id, customer.name, customer.email) == (acme, AWKWARD, None)
    assert customer.vip is True
    assert customer.preferred_method is m.CustomersPreferredMethod.BANK_TRANSFER
    # MySQL stores JSON normalised: the same document, not the same text.
    assert customer.metadata is not None
    assert json.loads(customer.metadata) == json.loads(metadata)
    assert customer.created_at.tzinfo is None
    with pytest.raises(api.NoRow):
        await q.get_customer(id=acme + 1000)

    bare = await q.create_customer(
        name="x:test",
        email="x@example.com",
        vip=False,
        preferred_method=None,
        metadata=None,
    )
    assert bare == acme + 1
    assert (await q.get_customer(id=bare)).vip is False

    # One name for two customers: a `:one` must not pick either.
    assert (await q.find_customer_by_name(name=AWKWARD)).id == acme
    twin = await q.create_customer(
        name=AWKWARD,
        email="twin@example.com",
        vip=False,
        preferred_method=None,
        metadata=None,
    )
    with pytest.raises(api.MultipleRows):
        await q.find_customer_by_name(name=AWKWARD)
    with pytest.raises(api.NoRow):
        await q.find_customer_by_name(name="nobody")

    # One parameter, used twice; the literal `%:test%` excludes `x:test`.
    found = await q.search_customers(pattern="%example.com")
    assert [(r.id, r.name) for r in found] == [(twin, AWKWARD)]
    assert sorted(r.id for r in await q.search_customers(pattern="it's%")) == [
        acme,
        twin,
    ]
    assert await q.search_customers(pattern=None) == []

    # A slice: several, one, and none.
    listed = await q.list_customers_by_ids(ids=[twin, acme, acme + 999])
    assert [c.id for c in listed] == [acme, twin]
    assert [c.id for c in await q.list_customers_by_ids(ids=[bare])] == [bare]
    assert await q.list_customers_by_ids(ids=[]) == []

    attachment = bytes(range(256))
    pay_within = -dt.timedelta(hours=1, minutes=2, seconds=3)
    invoice_id = await q.create_invoice(
        customer_id=acme,
        number="2026-001",
        issued_on=None,
        notes=AWKWARD,
        attachment=attachment,
        pay_within=pay_within,
    )
    assert invoice_id >= FIRST_ID
    invoice = await q.get_invoice(id=invoice_id)
    assert invoice.status is m.InvoicesStatus.DRAFT
    assert (invoice.notes, invoice.attachment, invoice.pay_within) == (
        AWKWARD,
        attachment,
        pay_within,
    )
    assert invoice.total == Decimal("0.00")
    assert str(invoice.total) == "0.00"
    assert await q.get_invoice_status(id=invoice_id) is m.InvoicesStatus.DRAFT
    with pytest.raises(api.NoRow):
        await q.get_invoice(id=invoice_id + 1000)
    joined = await q.get_invoice_with_customer(id=invoice_id)
    assert (joined.invoice, joined.customer) == (invoice, customer)

    other = await q.create_invoice(
        customer_id=acme,
        number="2026-002",
        issued_on=dt.date(2026, 11, 13),
        notes=None,
        attachment=None,
        pay_within=dt.timedelta(days=30),
    )
    assert (await q.get_invoice(id=other)).pay_within == dt.timedelta(days=30)
    assert await q.count_invoices(customer_id=acme) == 2
    assert await q.count_invoices(customer_id=bare) == 0

    assert await q.issue_invoice(issued_on=dt.date(2026, 11, 14), id=other) == 1
    assert await q.issue_invoice(issued_on=dt.date(2026, 11, 14), id=other) == 0
    statuses = m.InvoicesStatus
    by_status = await q.list_invoices_by_status(
        statuses=[statuses.DRAFT, statuses.ISSUED], customer_id=acme
    )
    assert [i.number for i in by_status] == ["2026-001", "2026-002"]
    issued = await q.list_invoices_by_status(
        statuses=[statuses.ISSUED], customer_id=acme
    )
    assert [(i.number, i.issued_on) for i in issued] == [
        ("2026-002", dt.date(2026, 11, 14))
    ]
    assert await q.list_invoices_by_status(statuses=[], customer_id=acme) == []

    rows = [
        q.AddLineItemsParams(
            invoice_id=invoice_id,
            position=1,
            description=AWKWARD,
            quantity=Decimal("1.500"),
            unit_price=Decimal("12.50"),
        ),
        q.AddLineItemsParams(
            invoice_id=invoice_id,
            position=2,
            description="fee",
            quantity=Decimal(2),
            unit_price=Decimal("0.10"),
        ),
    ]
    assert await q.add_line_items(rows=rows) is None
    items = await q.list_line_items(invoice_id=invoice_id)
    assert [(i.description, i.quantity, i.unit_price) for i in items] == [
        (AWKWARD, Decimal("1.500"), Decimal("12.50")),
        ("fee", Decimal("2.000"), Decimal("0.10")),
    ]
    assert str(items[1].quantity) == "2.000"
    assert await q.recalculate_total(id=invoice_id) is None
    total = (await q.get_invoice(id=invoice_id)).total
    assert (total, str(total)) == (Decimal("18.95"), "18.95")

    result = await q.delete_draft_invoices(customer_id=acme)
    assert await aw(result.rowcount) == 1
    assert await q.count_invoices(customer_id=acme) == 1
