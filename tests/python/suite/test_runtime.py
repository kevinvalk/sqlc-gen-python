"""Every generated function, run against PostgreSQL through every driver.

One scenario drives the invoicing API end to end. It is written once and run
through each invoicing case: SQLAlchemy's functions on psycopg, and its
coroutines on asyncpg and psycopg; psycopg's own, sync and async. Every call is wrapped by `deep.checked`, so each argument
and result must match its annotation exactly, and the run fails if any public
function was never called.
"""

import asyncio
import datetime as dt
import importlib
import re
from collections.abc import Awaitable, Callable
from decimal import Decimal
from typing import Any
from uuid import uuid4
from zoneinfo import ZoneInfo

import psycopg
import pytest
import sqlalchemy
from postgres import aw, conninfo
from sqlalchemy.ext.asyncio import create_async_engine

from conftest import Case, cases, functions, queries

AMSTERDAM = ZoneInfo("Europe/Amsterdam")
# Characters that SQLAlchemy, a driver or a careless generator could misread.
AWKWARD = 'it\'s "quoted" \\ :name $1 %s %(x)s ::text {}'

# The drivers each case runs on: a backend's, sync or async.
DRIVERS = {
    ("sqlalchemy", False): ["psycopg"],
    ("sqlalchemy", True): ["asyncpg", "psycopg"],
    ("psycopg", False): ["psycopg"],
    ("psycopg", True): ["psycopg"],
}
RUNS = [
    pytest.param(case, driver, id=f"{case.name}-{driver}")
    for case in cases()
    for driver in DRIVERS[case.backend, case.is_async]
]


@pytest.mark.parametrize(("case", "driver"), RUNS)
def test_every_function_against_postgres(
    case: Case, driver: str, database: sqlalchemy.URL
) -> None:
    called: set[str] = set()
    run = {
        ("sqlalchemy", False): _sqlalchemy_sync,
        ("sqlalchemy", True): _sqlalchemy_async,
        ("psycopg", False): _psycopg_sync,
        ("psycopg", True): _psycopg_async,
    }[case.backend, case.is_async]
    asyncio.run(run(case, database.set(drivername=f"postgresql+{driver}"), called))
    public = functions(case)
    assert called == public, f"never called: {sorted(public - called)}"


async def _sqlalchemy_sync(case: Case, url: sqlalchemy.URL, called: set[str]) -> None:
    engine = sqlalchemy.create_engine(url)
    try:
        with engine.connect() as conn:
            conn.begin()
            api = _api(case, conn, called)
            await scenario(api, _sqlalchemy_sql(conn))
            conn.rollback()
    finally:
        engine.dispose()


async def _sqlalchemy_async(case: Case, url: sqlalchemy.URL, called: set[str]) -> None:
    engine = create_async_engine(url)
    try:
        async with engine.connect() as conn:
            await conn.begin()
            api = _api(case, conn, called)
            await scenario(api, _sqlalchemy_sql(conn))
            await conn.rollback()
    finally:
        await engine.dispose()


async def _psycopg_sync(case: Case, url: sqlalchemy.URL, called: set[str]) -> None:
    with psycopg.connect(conninfo(url)) as conn:
        await scenario(_api(case, conn, called), _psycopg_sql(conn))
        conn.rollback()


async def _psycopg_async(case: Case, url: sqlalchemy.URL, called: set[str]) -> None:
    async with await psycopg.AsyncConnection.connect(conninfo(url)) as conn:
        await scenario(_api(case, conn, called), _psycopg_sql(conn))
        await conn.rollback()


def _api(case: Case, conn: object, called: set[str]) -> Any:  # noqa: ANN401
    """The case's API on ``conn``, with the errors a ``:one`` raises."""
    api = queries(case, conn, called)
    package = importlib.import_module(case.name)
    if case.backend == "sqlalchemy":
        api.NoRow = sqlalchemy.exc.NoResultFound
        api.MultipleRows = sqlalchemy.exc.MultipleResultsFound
    else:
        api.NoRow, api.MultipleRows = package.NoRowError, package.MultipleRowsError
    return api


def _sqlalchemy_sql(conn: Any) -> Callable[..., Awaitable[Any]]:  # noqa: ANN401
    """Raw SQL for looking behind the API, e.g. at SQL NULL versus JSON null."""

    async def scalar(statement: str, **params: object) -> object:
        result = await aw(conn.execute(sqlalchemy.text(statement), params))
        return result.scalar_one()

    return scalar


def _psycopg_sql(conn: Any) -> Callable[..., Awaitable[Any]]:  # noqa: ANN401
    """The same, on a psycopg connection: ``:name`` becomes ``%(name)s``."""

    async def scalar(statement: str, **params: object) -> object:
        query = re.sub(r":(\w+)", r"%(\1)s", statement)
        cur = await aw(conn.execute(query, params))
        (row,) = await aw(cur.fetchall())
        return row[0]

    return scalar


async def no_row(api: Any, call: Awaitable[object]) -> None:  # noqa: ANN401
    """A `:one` without a row raises, as sqlc's Go raises `ErrNoRows`."""
    with pytest.raises(api.NoRow):
        await call


async def scenario(api: Any, sql: Callable[..., Awaitable[Any]]) -> None:  # noqa: ANN401, PLR0915
    m = api.models
    today = dt.date(2026, 11, 11)
    now = dt.datetime(2026, 11, 11, 9, 30, 15, 123456, tzinfo=AMSTERDAM)

    # Customers: enum, enum array, text array, JSON and NULLs both ways.
    metadata = {
        "vip": True,
        "limit": 2**62,
        "rate": 0.5,
        "tags": ["a", None],
        "n": None,
    }
    ada = await api.customers.create_customer(
        name="Ada",
        email=None,
        tags=["vip", "b2b", 'quote"d', "comma,", ""],
        preferred_method=m.BillingPaymentMethod.DIRECT_DEBIT,
        accepted_methods=[
            m.BillingPaymentMethod.CARD,
            m.BillingPaymentMethod.DIRECT_DEBIT,
        ],
        metadata=metadata,
    )
    assert ada.email is None
    assert ada.tags == ["vip", "b2b", 'quote"d', "comma,", ""]
    assert ada.preferred_method is m.BillingPaymentMethod.DIRECT_DEBIT
    assert ada.accepted_methods == [
        m.BillingPaymentMethod.CARD,
        m.BillingPaymentMethod.DIRECT_DEBIT,
    ]
    assert ada.metadata == metadata
    assert ada.created_at.utcoffset() is not None

    bob = await api.customers.create_customer(
        name="Bob",
        email="bob@example.com",
        tags=[],
        preferred_method=None,
        accepted_methods=[],
        metadata=None,
    )
    assert (bob.tags, bob.accepted_methods, bob.metadata) == ([], [], None)
    assert await sql("SELECT metadata IS NULL FROM customers WHERE id = :id", id=bob.id)
    # Bare enums, converted one by one, NULL kept as `None`.
    assert await api.customers.list_preferred_methods() == [
        m.BillingPaymentMethod.DIRECT_DEBIT,
        None,
    ]

    assert await api.customers.get_customer(id=ada.id) == ada
    await no_row(api, api.customers.get_customer(id=uuid4()))

    assert (
        await api.customers.update_customer_email(email="ada@example.com", id=ada.id)
        == 1
    )
    assert await api.customers.update_customer_email(email=None, id=uuid4()) == 0
    ada = await api.customers.get_customer(id=ada.id)
    assert ada.email == "ada@example.com"

    # Enum arrays read by position, whatever PostgreSQL names the column.
    assert await api.customers.get_accepted_methods(id=ada.id) == ada.accepted_methods
    assert await api.customers.get_accepted_methods(id=bob.id) == []
    await no_row(api, api.customers.get_accepted_methods(id=uuid4()))
    both = await api.customers.compare_accepted_methods(first=ada.id, second=bob.id)
    assert (both.accepted_methods, both.accepted_methods_2) == (
        ada.accepted_methods,
        [],
    )

    assert await api.customers.search_customers(query="ADA") == [ada]
    assert await api.customers.search_customers(query="example") == [ada, bob]
    assert await api.customers.search_customers(query=AWKWARD) == []
    assert await api.customers.list_customer_names_by_tag(tag="b2b") == ["Ada"]
    assert await api.customers.list_customer_names_by_tag(tag="none") == []
    vips = await api.customers.list_vip_customers()
    assert [(v.id, v.name, v.email) for v in vips] == [
        (ada.id, "Ada", "ada@example.com")
    ]

    # Invoices: char(n) padding, nullable dates, text that looks like SQL.
    first = await api.invoices.create_invoice(
        customer_id=ada.id,
        number="2026-0001",
        currency="EU",
        issued_on=None,
        due_on=dt.date(2026, 11, 1),
        notes=AWKWARD,
    )
    assert first.currency == "EU "  # char(3) pads; the driver returns it as is
    assert first.notes == AWKWARD
    assert first.status is m.InvoiceStatus.DRAFT
    assert (first.issued_on, first.paid_at) == (None, None)
    second = await api.invoices.create_invoice(
        customer_id=ada.id,
        number="2026-0002",
        currency="EUR",
        issued_on=today,
        due_on=None,
        notes=None,
    )
    for_bob = await api.invoices.create_invoice(
        customer_id=bob.id,
        number="2026-0003",
        currency="USD",
        issued_on=None,
        due_on=None,
        notes=None,
    )

    assert await api.invoices.get_invoice(id=first.id) == first
    await no_row(api, api.invoices.get_invoice(id=uuid4()))
    assert await api.invoices.get_invoice_status(id=first.id) is m.InvoiceStatus.DRAFT
    await no_row(api, api.invoices.get_invoice_status(id=uuid4()))

    # A nullable single column is bare: NULL is `None`, a missing row raises.
    due = await api.invoices.get_invoice_due_date(id=first.id)
    assert due == dt.date(2026, 11, 1)
    assert await api.invoices.get_invoice_due_date(id=second.id) is None
    await no_row(api, api.invoices.get_invoice_due_date(id=uuid4()))

    joined = await api.invoices.get_invoice_with_customer(id=first.id)
    assert (joined.invoice, joined.customer) == (first, ada)
    await no_row(api, api.invoices.get_invoice_with_customer(id=uuid4()))

    by_ids = await api.invoices.list_invoices_by_ids(ids=[second.id, first.id, uuid4()])
    assert by_ids == [first, second]
    assert await api.invoices.list_invoices_by_ids(ids=[]) == []
    drafts = await api.invoices.list_invoices_by_status(
        statuses=[m.InvoiceStatus.DRAFT]
    )
    assert drafts == [first, second, for_bob]
    assert await api.invoices.list_invoices_by_status(statuses=[]) == []
    assert await api.invoices.list_invoice_numbers(customer_id=ada.id) == [
        "2026-0001",
        "2026-0002",
    ]
    assert await api.invoices.list_invoice_numbers(customer_id=uuid4()) == []

    # Line items: exact decimals with their scale, and an empty batch.
    lines = [
        api.line_items.CreateLineItemsParams(
            invoice_id=first.id,
            position=1,
            description=AWKWARD,
            quantity=Decimal("2.500"),
            unit_price=Decimal("19.99"),
            vat_rate=Decimal("0.2100"),
        ),
        api.line_items.CreateLineItemsParams(
            invoice_id=first.id,
            position=2,
            description="Support",
            quantity=Decimal(1),
            unit_price=Decimal("100.00"),
            vat_rate=Decimal("0.0900"),
        ),
    ]
    assert await api.line_items.create_line_items(lines) is None
    assert await api.line_items.create_line_items([]) is None
    stored = await api.line_items.list_line_items(invoice_id=first.id)
    assert [(line.position, line.description) for line in stored] == [
        (1, AWKWARD),
        (2, "Support"),
    ]
    assert [str(line.quantity) for line in stored] == ["2.500", "1.000"]
    assert [str(line.vat_rate) for line in stored] == ["0.2100", "0.0900"]
    assert await api.line_items.list_line_items(invoice_id=uuid4()) == []

    totals = await api.invoices.get_invoice_totals(id=first.id)
    # 2.500 * 19.99 = 49.975 -> 49.98; VAT 10.49475 -> 10.49 and 9.00.
    assert (totals.line_count, str(totals.subtotal), str(totals.vat)) == (
        2,
        "149.98",
        "19.49",
    )
    empty = await api.invoices.get_invoice_totals(id=second.id)
    assert (empty.line_count, str(empty.subtotal), str(empty.vat)) == (
        0,
        "0.00",
        "0.00",
    )
    await no_row(api, api.invoices.get_invoice_totals(id=uuid4()))

    # Lifecycle: :execrows counts, optional filters, :execresult.
    assert (
        await api.invoices.issue_invoice(issued_on=dt.date(2026, 10, 1), id=first.id)
        == 1
    )
    assert await api.invoices.issue_invoice(issued_on=today, id=first.id) == 0
    between = await api.invoices.list_invoices_issued_between(
        issued_on=dt.date(2026, 10, 1), issued_on_2=today
    )
    assert [invoice.id for invoice in between] == [first.id]
    assert (
        await api.invoices.list_invoices_issued_between(
            issued_on=None, issued_on_2=today
        )
        == []
    )

    everything = await api.invoices.search_invoices(
        customer_id=ada.id, status=None, due_before=None
    )
    assert [(row.number, row.status) for row in everything] == [
        ("2026-0001", m.InvoiceStatus.ISSUED),
        ("2026-0002", m.InvoiceStatus.DRAFT),
    ]
    issued = await api.invoices.search_invoices(
        customer_id=ada.id, status=m.InvoiceStatus.ISSUED, due_before=today
    )
    assert [row.id for row in issued] == [first.id]
    assert (
        await api.invoices.search_invoices(
            customer_id=ada.id, status=m.InvoiceStatus.PAID, due_before=None
        )
        == []
    )

    overdue = await api.invoices.list_overdue_invoices(as_of=today)
    assert [(row.number, row.customer_name, row.email) for row in overdue] == [
        ("2026-0001", "Ada", "ada@example.com")
    ]
    assert await api.invoices.list_overdue_invoices(as_of=None) == []

    assert await api.invoices.list_invoice_statuses() == [
        m.InvoiceStatus.DRAFT,
        m.InvoiceStatus.ISSUED,
    ]
    counts = await api.invoices.count_invoices_by_status()
    assert [(row.status, row.count) for row in counts] == [
        (m.InvoiceStatus.DRAFT, 2),
        (m.InvoiceStatus.ISSUED, 1),
    ]
    assert await api.invoices.count_invoices(customer_id=ada.id) == 2
    assert await api.invoices.count_invoices(customer_id=uuid4()) == 0

    # Payments: a keyword column, a non-public enum array parameter.
    payment = await api.payments.record_payment(
        invoice_id=first.id,
        method=m.BillingPaymentMethod.DIRECT_DEBIT,
        amount=Decimal("169.47"),
        from_="NL91 ABNA 0417 1643 00",
        received_at=now,
    )
    assert payment.from_ == "NL91 ABNA 0417 1643 00"
    assert payment.received_at == now
    assert str(payment.amount) == "169.47"
    # A view's cast sum is its model's `Decimal`, scale and all.
    balances = await api.payments.list_invoice_balances(customer_id=ada.id)
    assert [(b.invoice_id, b.number, str(b.paid)) for b in balances] == [
        (first.id, "2026-0001", "169.47"),
        (second.id, "2026-0002", "0.00"),
    ]
    assert type(balances[0]) is m.InvoiceBalance
    assert await api.payments.list_invoice_payment_methods(invoice_id=first.id) == [
        m.BillingPaymentMethod.DIRECT_DEBIT
    ]
    assert await api.payments.list_invoice_payment_methods(invoice_id=second.id) == []
    debits = await api.payments.list_payments_by_methods(
        methods=[m.BillingPaymentMethod.DIRECT_DEBIT, m.BillingPaymentMethod.CARD]
    )
    assert debits == [payment]
    assert (
        await api.payments.list_payments_by_methods(
            methods=[m.BillingPaymentMethod.CARD]
        )
        == []
    )

    assert await api.invoices.mark_invoice_paid(paid_at=now, id=first.id) is None
    paid = await api.invoices.get_invoice(id=first.id)
    assert (paid.status, paid.paid_at) == (m.InvoiceStatus.PAID, now)
    assert (await api.invoices.void_invoice(id=first.id)).rowcount == 0
    assert (await api.invoices.void_invoice(id=second.id)).rowcount == 1

    # Media: binary content and a table whose model name is singularised.
    content = bytes(range(256))
    medium = await api.invoices.attach_medium(
        invoice_id=first.id, filename="scan.pdf", content=content
    )
    assert type(medium) is m.Medium
    assert medium.content == content
    listed = await api.invoices.list_media(invoice_id=first.id)
    assert [(row.id, row.filename) for row in listed] == [(medium.id, "scan.pdf")]

    assert (
        await api.invoices.delete_draft_invoices(
            created_before=dt.datetime.now(dt.timezone.utc) + dt.timedelta(days=1)
        )
        == 1
    )

    # A `:one` that matches no row, or more than one, is an error, never a guess.
    assert await api.customers.get_customer_by_name(name="Bob") == bob
    await no_row(api, api.customers.get_customer_by_name(name="Nobody"))
    email = "bob@example.com"
    assert await api.customers.get_customer_id_by_email(email=email) == bob.id
    await no_row(api, api.customers.get_customer_id_by_email(email=None))
    assert await api.customers.create_customer(
        name="Bob",
        email=email,
        tags=[],
        preferred_method=None,
        accepted_methods=[],
        metadata=None,
    )
    with pytest.raises(api.MultipleRows):
        await api.customers.get_customer_by_name(name="Bob")
    with pytest.raises(api.MultipleRows):
        await api.customers.get_customer_id_by_email(email=email)

    # Enums print as their database label on every Python version.
    assert (str(m.InvoiceStatus.PAID), f"{m.BillingPaymentMethod.DIRECT_DEBIT}") == (
        "paid",
        "direct-debit",
    )
