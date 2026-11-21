"""Every generated function of the billing example, run against MySQL on each of its drivers.

Each run gets a fresh database, and as in `test_runtime.py`, every call is checked against its annotations and every
function must be called.
"""

import asyncio
import contextlib
import datetime as dt
import functools
import json
from collections.abc import Awaitable, Callable, Coroutine
from decimal import Decimal
from typing import Any

import asyncmy.cursors
import mysql
import pymysql
import pytest
import sqlalchemy
from examples import GoldenCase
from suite.api import calls_every_function, cases, no_row, queries, sqlalchemy_connection, streams_lazily

SCHEMA = cases("billing")[0].schema_sql.read_text()
# Characters PyMySQL's `%` formatting, SQLAlchemy's `:name` binds or a careless generator could misread.
AWKWARD = 'it\'s "quoted" \\ :name %s %(x)s %% ? /*SLICE:ids*/?'
FIRST_ID = 2**63 + 5  # Beyond a signed BIGINT, so we see an unsigned id is read as itself

# How many statements asyncmy keeps prepared on the server when a connection asks it to, as their values then cross as
# binary instead of text. A caller's connection may be either, so the `pymysql` backend's coroutines run on both.
PREPARED = (0, 8)

Run = Callable[[GoldenCase, sqlalchemy.URL, set[str]], Coroutine[Any, Any, None]]

RUNS = [
    pytest.param(
        case,
        driver,
        prepared,
        id=f"{case.name}-{driver}" + "-prepared" * bool(prepared),
    )
    for case in cases("billing")
    # NOTE: Not `mysql+aiomysql`, as aiomysql 0.3.2 cannot bind `bytes` on PyMySQL 1.2, which removed the
    # `escape_bytes_prefixed` it imports.
    for driver in (("asyncmy",) if case.is_async else ("pymysql",))
    for prepared in (PREPARED if (case.backend, driver) == ("pymysql", "asyncmy") else PREPARED[:1])
]


@pytest.mark.parametrize(("case", "driver", "prepared"), RUNS)
def test_every_function_against_mysql(
    case: GoldenCase, driver: str, prepared: int, mysql_server: sqlalchemy.URL
) -> None:
    with mysql.database(mysql_server, SCHEMA) as database, calls_every_function(case) as called:
        _setup(database)
        url = database.set(drivername=f"mysql+{driver}")
        runs: dict[tuple[str, bool], Run] = {
            ("pymysql", False): _pymysql,
            ("pymysql", True): functools.partial(_asyncmy, prepared=prepared),
            ("sqlalchemy", False): _sqlalchemy,
            ("sqlalchemy", True): _sqlalchemy,
        }
        asyncio.run(runs[case.backend, case.is_async](case, url, called))


def _setup(url: sqlalchemy.URL) -> None:
    """Start the ids where only an unsigned BIGINT holds them."""
    connection = mysql.connect(url)
    try:
        with connection.cursor() as cursor:
            for table in ("customers", "invoices"):
                cursor.execute(f"ALTER TABLE {table} AUTO_INCREMENT = {FIRST_ID}")
    finally:
        connection.close()


async def _pymysql(case: GoldenCase, url: sqlalchemy.URL, called: set[str]) -> None:
    connection = mysql.connect(url)
    # A cursor class of the caller's, which generated code must not read rows through.
    connection.cursorclass = pymysql.cursors.DictCursor
    try:
        await scenario(queries(case, connection, called))
        connection.rollback()
    finally:
        connection.close()


async def _asyncmy(case: GoldenCase, url: sqlalchemy.URL, called: set[str], *, prepared: int) -> None:
    connection = await mysql.connect_async(
        url,
        cursor_cls=asyncmy.cursors.DictCursor,  # A cursor class of the caller's, as in `_pymysql`
        stmt_cache_size=prepared,
    )
    try:
        await scenario(queries(case, connection, called))
        await connection.rollback()
    finally:
        await connection.ensure_closed()


async def _sqlalchemy(case: GoldenCase, url: sqlalchemy.URL, called: set[str]) -> None:
    async with sqlalchemy_connection(url, asynchronous=case.is_async) as connection:
        await scenario(queries(case, connection, called))


async def scenario(api: Any) -> None:  # noqa: PLR0915
    models, query = api.models, api.query
    metadata = '{"vip": true, "note": "50%: \'ok\'"}'
    acme = await query.create_customer(
        name=AWKWARD,
        email=None,
        vip=True,
        preferred_method=models.CustomersPreferredMethod.BANK_TRANSFER,
        metadata=metadata,
    )
    assert acme >= FIRST_ID
    customer = await query.get_customer(id=acme)
    assert (customer.id, customer.name, customer.email) == (acme, AWKWARD, None)
    assert customer.vip is True
    assert customer.preferred_method is models.CustomersPreferredMethod.BANK_TRANSFER
    # MySQL stores JSON normalised: the same document, not the same text.
    assert customer.metadata is not None
    assert json.loads(customer.metadata) == json.loads(metadata)
    assert customer.created_at.tzinfo is None
    await no_row(api, query.get_customer(id=acme + 1000))
    quoted = await query.get_quoted_text(id=acme)
    assert (quoted.triple_quotes, quoted.backslash, quoted.name) == ('"""', "\\ %s :x", AWKWARD)

    bare = await query.create_customer(
        name="x:test",
        email="x@example.com",
        vip=False,
        preferred_method=None,
        metadata=None,
    )
    assert bare == acme + 1
    assert (await query.get_customer(id=bare)).vip is False

    # One name for two customers: a `:one` must not pick either.
    assert (await query.find_customer_by_name(name=AWKWARD)).id == acme
    twin = await query.create_customer(
        name=AWKWARD,
        email="twin@example.com",
        vip=False,
        preferred_method=None,
        metadata=None,
    )
    with pytest.raises(api.MultipleRows):
        await query.find_customer_by_name(name=AWKWARD)
    await no_row(api, query.find_customer_by_name(name="nobody"))

    # One parameter, used twice; the literal `%:test%` excludes `x:test`.
    found = await query.search_customers(pattern="%example.com")
    assert [(r.id, r.name) for r in found] == [(twin, AWKWARD)]
    assert sorted(r.id for r in await query.search_customers(pattern="it's%")) == [
        acme,
        twin,
    ]
    assert await query.search_customers(pattern=None) == []

    # A slice: several, one, and none.
    listed = await query.list_customers_by_ids(ids=[twin, acme, acme + 999])
    assert [c.id for c in listed] == [acme, twin]
    assert [c.id for c in await query.list_customers_by_ids(ids=[bare])] == [bare]
    assert await query.list_customers_by_ids(ids=[]) == []
    # Of none, `NOT IN` keeps every row, where `NOT IN (NULL)` would keep none.
    assert [c.id for c in await query.list_customers_except(ids=[])] == [acme, bare, twin]
    assert [c.id for c in await query.list_customers_except(ids=[acme])] == [bare, twin]
    # One slice twice: each of its places gets every item, after the parameter before them.
    assert await query.list_customers_known_as(vip=False, names=["x:test", "twin@example.com"]) == [bare, twin]
    assert await query.list_customers_known_as(vip=False, names=[AWKWARD]) == [twin]
    assert await query.list_customers_known_as(vip=True, names=[AWKWARD]) == [acme]
    assert await query.list_customers_known_as(vip=False, names=[]) == []

    # A filter left out is `None`, which matches every row.
    assert await query.filter_customers() == [acme, bare, twin]
    assert await query.filter_customers(name=AWKWARD) == [acme, twin]
    assert await query.filter_customers(name=AWKWARD, vip=False) == [twin]
    assert await query.filter_customers(vip=True) == [acme]
    bank_transfer = models.CustomersPreferredMethod.BANK_TRANSFER
    assert await query.filter_customers(preferred_method=bank_transfer) == [acme]
    assert await query.filter_customers(name="x:test", preferred_method=bank_transfer) == []

    attachment = bytes(range(256))
    pay_within = -dt.timedelta(hours=1, minutes=2, seconds=3)
    invoice_id = await query.create_invoice(
        customer_id=acme,
        number="2026-001",
        issued_on=None,
        notes=AWKWARD,
        attachment=attachment,
        pay_within=pay_within,
    )
    assert invoice_id >= FIRST_ID
    invoice = await query.get_invoice(id=invoice_id)
    assert invoice.status is models.InvoicesStatus.DRAFT
    assert (invoice.notes, invoice.attachment, invoice.pay_within) == (
        AWKWARD,
        attachment,
        pay_within,
    )
    assert str(invoice.total) == "0.00"
    assert await query.get_invoice_status(id=invoice_id) is models.InvoicesStatus.DRAFT
    await no_row(api, query.get_invoice(id=invoice_id + 1000))
    joined = await query.get_invoice_with_customer(id=invoice_id)
    assert (joined.invoice, joined.customer) == (invoice, customer)

    other = await query.create_invoice(
        customer_id=acme,
        number="2026-002",
        issued_on=dt.date(2026, 11, 13),
        notes=None,
        attachment=None,
        pay_within=dt.timedelta(days=30),
    )
    assert (await query.get_invoice(id=other)).pay_within == dt.timedelta(days=30)
    assert await query.count_invoices(customer_id=acme) == 2
    assert await query.count_invoices(customer_id=bare) == 0

    assert await query.issue_invoice(issued_on=dt.date(2026, 11, 14), id=other) == 1
    assert await query.issue_invoice(issued_on=dt.date(2026, 11, 14), id=other) == 0
    statuses = models.InvoicesStatus
    by_status = await query.list_invoices_by_status(statuses=[statuses.DRAFT, statuses.ISSUED], customer_id=acme)
    assert [i.number for i in by_status] == ["2026-001", "2026-002"]
    issued = await query.list_invoices_by_status(statuses=[statuses.ISSUED], customer_id=acme)
    assert [(i.number, i.issued_on) for i in issued] == [("2026-002", dt.date(2026, 11, 14))]
    assert await query.list_invoices_by_status(statuses=[], customer_id=acme) == []

    rows = [
        query.AddLineItemsParams(
            invoice_id=invoice_id,
            position=1,
            description=AWKWARD,
            quantity=Decimal("1.500"),
            unit_price=Decimal("12.50"),
        ),
        query.AddLineItemsParams(
            invoice_id=invoice_id,
            position=2,
            description="fee",
            quantity=Decimal(2),
            unit_price=Decimal("0.10"),
        ),
    ]
    # `INSERT IGNORE` skips the line at a taken position, and returns how many it wrote.
    assert await query.add_line_items(rows=[*rows, rows[0]]) == 2
    assert await query.add_line_items(rows=[]) == 0
    items = await query.list_line_items(invoice_id=invoice_id)
    assert [(i.description, i.quantity, i.unit_price) for i in items] == [
        (AWKWARD, Decimal("1.500"), Decimal("12.50")),
        ("fee", Decimal("2.000"), Decimal("0.10")),
    ]
    assert str(items[1].quantity) == "2.000"
    # What stands left of a RIGHT JOIN is None for a customer without lines, though sqlc says NOT NULL.
    lines = await query.list_customer_lines()
    assert [(row.id, row.number, row.description) for row in lines] == [
        (acme, "2026-001", AWKWARD),
        (acme, "2026-001", "fee"),
        (bare, None, None),
        (twin, None, None),
    ]
    assert await query.recalculate_total(id=invoice_id) is None
    total = (await query.get_invoice(id=invoice_id)).total
    assert str(total) == "18.95"

    # IMPORTANT: The README says a statement on the connection ends a MySQL stream early with only a warning. We pin that,
    # so a driver that starts to raise or to buffer is seen.
    with pytest.warns(UserWarning, match="Previous unbuffered result was left incomplete"):
        read = await _read_with_lookup(
            query.stream_list_line_items(invoice_id=invoice_id),
            lambda: query.count_invoices(customer_id=acme),
        )
    assert [item.position for item in read] == [1]

    result = await query.delete_draft_invoices(customer_id=acme)
    assert result.rowcount == 1
    assert await query.count_invoices(customer_id=acme) == 1

    await streams_lazily(api, query)


async def _read_with_lookup(stream: Any, lookup: Callable[[], Awaitable[object]]) -> list[Any]:
    """Read a stream's rows, running ``lookup`` on its connection after the first, as a lookup per row would."""
    if isinstance(stream, contextlib.AbstractAsyncContextManager):
        async with stream as rows:
            first = await anext(rows)
            await lookup()
            return [first, *[row async for row in rows]]
    with stream as rows:
        first = next(rows)
        await lookup()
        return [first, *rows]
