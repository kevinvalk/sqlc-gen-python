"""Every generated function of the invoicing example, run against PostgreSQL through every driver.

Every call is wrapped by `deep.checked`, so each argument and result must match its annotation exactly, and the run
fails if any public function was never called.
"""

import asyncio
import contextlib
import datetime as dt
import re
from collections.abc import AsyncIterator, Awaitable, Callable
from decimal import Decimal
from typing import Any
from uuid import uuid4
from zoneinfo import ZoneInfo

import psycopg
import pytest
import sqlalchemy
from examples import GoldenCase
from postgres import aw, conninfo
from suite.api import calls_every_function, cases, no_row, queries, sqlalchemy_connection, streams_lazily

AMSTERDAM = ZoneInfo("Europe/Amsterdam")
# Characters that SQLAlchemy, a driver or a careless generator could misread.
AWKWARD = 'it\'s "quoted" \\ :name $1 %s %(x)s ::text {}'

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
def test_every_function_against_postgres(case: GoldenCase, driver: str, database: sqlalchemy.URL) -> None:
    run = {
        ("sqlalchemy", False): _sqlalchemy,
        ("sqlalchemy", True): _sqlalchemy,
        ("psycopg", False): _psycopg_sync,
        ("psycopg", True): _psycopg_async,
    }[case.backend, case.is_async]
    with calls_every_function(case) as called:
        asyncio.run(run(case, database.set(drivername=f"postgresql+{driver}"), called))


async def _sqlalchemy(case: GoldenCase, url: sqlalchemy.URL, called: set[str]) -> None:
    async with sqlalchemy_connection(url, asynchronous=case.is_async) as connection:
        await scenario(_api(case, connection, called), _sqlalchemy_sql(connection))


async def _psycopg_sync(case: GoldenCase, url: sqlalchemy.URL, called: set[str]) -> None:
    with psycopg.connect(conninfo(url)) as connection:
        await scenario(_api(case, connection, called), _psycopg_sql(connection))
        connection.rollback()


async def _psycopg_async(case: GoldenCase, url: sqlalchemy.URL, called: set[str]) -> None:
    async with await psycopg.AsyncConnection.connect(conninfo(url)) as connection:
        await scenario(_api(case, connection, called), _psycopg_sql(connection))
        await connection.rollback()


def _api(case: GoldenCase, connection: object, called: set[str]) -> Any:
    """The case's API on ``connection``, with a pipeline to batch calls in."""
    api = queries(case, connection, called)
    api.pipeline = lambda: _pipeline(connection)
    return api


@contextlib.asynccontextmanager
async def _pipeline(connection: object) -> AsyncIterator[None]:
    """A pipeline of the caller's on psycopg, as a count read in it must still be the statement's, though psycopg only
    counts one once the pipeline syncs.
    """
    if isinstance(connection, psycopg.AsyncConnection):
        async with connection.pipeline():
            yield
    elif isinstance(connection, psycopg.Connection):
        with connection.pipeline():
            yield
    else:
        yield


def _sqlalchemy_sql(connection: Any) -> Callable[..., Awaitable[Any]]:
    """Raw SQL so we can look behind the API, at SQL NULL versus JSON null for example."""

    async def scalar(statement: str, **parameters: object) -> object:
        result = await aw(connection.execute(sqlalchemy.text(statement), parameters))
        return result.scalar_one()

    return scalar


def _psycopg_sql(connection: Any) -> Callable[..., Awaitable[Any]]:
    """The same on a psycopg connection, where ``:name`` becomes ``%(name)s``."""

    async def scalar(statement: str, **parameters: object) -> object:
        query = re.sub(r":(\w+)", r"%(\1)s", statement)
        cursor = await aw(connection.execute(query, parameters))
        (row,) = await aw(cursor.fetchall())
        return row[0]

    return scalar


async def scenario(api: Any, sql: Callable[..., Awaitable[Any]]) -> None:  # noqa: PLR0915
    models = api.models
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
        preferred_method=models.BillingPaymentMethod.DIRECT_DEBIT,
        accepted_methods=[
            models.BillingPaymentMethod.CARD,
            models.BillingPaymentMethod.DIRECT_DEBIT,
        ],
        metadata=metadata,
    )
    assert ada.email is None
    assert ada.tags == ["vip", "b2b", 'quote"d', "comma,", ""]
    assert ada.preferred_method is models.BillingPaymentMethod.DIRECT_DEBIT
    assert ada.accepted_methods == [
        models.BillingPaymentMethod.CARD,
        models.BillingPaymentMethod.DIRECT_DEBIT,
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
    stored = await sql("SELECT metadata IS NULL FROM customers WHERE id = :id", id=bob.id)
    assert stored, "metadata=None is stored as JSON null"
    # Bare enums, converted one by one, NULL kept as `None`.
    assert await api.customers.list_preferred_methods() == [
        models.BillingPaymentMethod.DIRECT_DEBIT,
        None,
    ]

    assert await api.customers.get_customer(id=ada.id) == ada
    await no_row(api, api.customers.get_customer(id=uuid4()))
    quoted = await api.customers.get_quoted_text(id=ada.id)
    assert (quoted.triple_quotes, quoted.backslash, quoted.escaped) == ('"""', "\\ %s :x", "\\ $2\n")

    async with api.pipeline():
        assert await api.customers.update_customer_email(email="ada@example.com", id=ada.id) == 1
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
    assert [(v.id, v.name, v.email) for v in vips] == [(ada.id, "Ada", "ada@example.com")]

    # Invoices: char(n) padding, nullable dates, text that looks like SQL.
    first = await api.invoices.create_invoice(
        customer_id=ada.id,
        number="2026-0001",
        currency="EU",
        issued_on=None,
        due_on=dt.date(2026, 11, 1),
        notes=AWKWARD,
    )
    assert first.currency == "EU "  # char(3) pads and the driver returns it as is
    assert first.notes == AWKWARD
    assert first.status is models.InvoiceStatus.DRAFT
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
    # A CASE that takes no branch is None, though sqlc says NOT NULL.
    numbers = await api.invoices.list_issued_numbers(customer_id=ada.id)
    assert [(row.id, row.issued_number) for row in numbers] == [
        (first.id, None),
        (second.id, "2026-0002"),
    ]
    assert await api.invoices.get_invoice_status(id=first.id) is models.InvoiceStatus.DRAFT
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
    drafts = await api.invoices.list_invoices_by_status(statuses=[models.InvoiceStatus.DRAFT])
    assert drafts == [first, second, for_bob]
    assert await api.invoices.list_invoices_by_status(statuses=[]) == []
    assert await api.invoices.list_invoice_numbers(customer_id=ada.id) == [
        "2026-0001",
        "2026-0002",
    ]
    assert await api.invoices.list_invoice_numbers(customer_id=uuid4()) == []

    # Batches: each row's result in order, a missing row raising as `:one`'s.
    batch_get = api.invoices.GetInvoicesBatchParams
    assert await api.invoices.get_invoices_batch([batch_get(id=second.id), batch_get(id=first.id)]) == [second, first]
    assert await api.invoices.get_invoices_batch([]) == []
    # A row without its result is `None` in the list, or raises.
    missing = api.invoices.get_invoices_batch([batch_get(id=first.id), batch_get(id=uuid4())])
    if api.raises:
        await no_row(api, missing)
    else:
        assert await missing == [first, None]
    batch_numbers = api.invoices.ListInvoiceNumbersBatchParams
    assert await api.invoices.list_invoice_numbers_batch(
        [batch_numbers(customer_id=ada.id), batch_numbers(customer_id=uuid4())]
    ) == [["2026-0001", "2026-0002"], []]
    assert await api.invoices.list_invoice_numbers_batch([]) == []

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
    assert await api.line_items.create_line_items(lines) == 2
    assert await api.line_items.create_line_items([]) == 0
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
    assert await api.invoices.issue_invoice(issued_on=dt.date(2026, 10, 1), id=first.id) == 1
    assert await api.invoices.issue_invoice(issued_on=today, id=first.id) == 0
    between = await api.invoices.list_invoices_issued_between(issued_on=dt.date(2026, 10, 1), issued_on_2=today)
    assert [invoice.id for invoice in between] == [first.id]
    assert await api.invoices.list_invoices_issued_between(issued_on=None, issued_on_2=today) == []

    everything = await api.invoices.search_invoices(customer_id=ada.id, status=None, due_before=None)
    assert [(row.number, row.status) for row in everything] == [
        ("2026-0001", models.InvoiceStatus.ISSUED),
        ("2026-0002", models.InvoiceStatus.DRAFT),
    ]
    issued = await api.invoices.search_invoices(
        customer_id=ada.id, status=models.InvoiceStatus.ISSUED, due_before=today
    )
    assert [row.id for row in issued] == [first.id]
    assert (
        await api.invoices.search_invoices(customer_id=ada.id, status=models.InvoiceStatus.PAID, due_before=None) == []
    )

    overdue = await api.invoices.list_overdue_invoices(as_of=today)
    assert [(row.number, row.customer_name, row.email) for row in overdue] == [("2026-0001", "Ada", "ada@example.com")]
    assert await api.invoices.list_overdue_invoices(as_of=None) == []

    assert await api.invoices.list_invoice_statuses() == [
        models.InvoiceStatus.DRAFT,
        models.InvoiceStatus.ISSUED,
    ]
    counts = await api.invoices.count_invoices_by_status()
    assert [(row.status, row.count) for row in counts] == [
        (models.InvoiceStatus.DRAFT, 2),
        (models.InvoiceStatus.ISSUED, 1),
    ]
    assert await api.invoices.count_invoices(customer_id=ada.id) == 2
    assert await api.invoices.count_invoices(customer_id=uuid4()) == 0
    # A count of the rows a filter keeps, and one over a window.
    counted = await api.invoices.count_invoices_issued(customer_id=ada.id)
    assert (counted.total, counted.issued) == (2, 1)
    none = await api.invoices.count_invoices_issued(customer_id=uuid4())
    assert (none.total, none.issued) == (0, 0)
    page = await api.invoices.list_invoice_numbers_page(customer_id=ada.id, limit=1)
    assert [(row.number, row.total) for row in page] == [("2026-0001", 2)]
    assert await api.invoices.list_invoice_numbers_page(customer_id=uuid4(), limit=1) == []
    # A subquery an outer join leaves without a row has its column None, though sqlc says NOT NULL.
    issued_numbers = await api.payments.list_customers_issued()
    assert [(row.name, row.number) for row in issued_numbers] == [
        ("Ada", "2026-0001"),
        ("Bob", None),
    ]

    # Payments: a keyword column, a non-public enum array parameter.
    payment = await api.payments.record_payment(
        invoice_id=first.id,
        method=models.BillingPaymentMethod.DIRECT_DEBIT,
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
    assert await api.payments.list_invoice_payment_methods(invoice_id=first.id) == [
        models.BillingPaymentMethod.DIRECT_DEBIT
    ]
    assert await api.payments.list_invoice_payment_methods(invoice_id=second.id) == []
    debits = await api.payments.list_payments_by_methods(
        methods=[models.BillingPaymentMethod.DIRECT_DEBIT, models.BillingPaymentMethod.CARD]
    )
    assert debits == [payment]
    assert await api.payments.list_payments_by_methods(methods=[models.BillingPaymentMethod.CARD]) == []
    # What stands left of a RIGHT JOIN is None for a customer without payments, though sqlc says NOT NULL.
    paid_by = await api.payments.list_customer_payments()
    assert [(row.name, row.number, row.amount) for row in paid_by] == [
        ("Ada", "2026-0001", Decimal("169.47")),
        ("Bob", None, None),
    ]

    assert await api.invoices.mark_invoice_paid(paid_at=now, id=first.id) is None
    paid = await api.invoices.get_invoice(id=first.id)
    assert (paid.status, paid.paid_at) == (models.InvoiceStatus.PAID, now)
    async with api.pipeline():
        assert (await api.invoices.void_invoice(id=first.id)).rowcount == 0
        assert (await api.invoices.void_invoice(id=second.id)).rowcount == 1

    # Media: binary content and a table whose model name is singularised.
    content = bytes(range(256))
    medium = await api.invoices.attach_medium(invoice_id=first.id, filename="scan.pdf", content=content)
    assert medium.content == content
    listed = await api.invoices.list_media(invoice_id=first.id)
    assert [(row.id, row.filename) for row in listed] == [(medium.id, "scan.pdf")]

    assert await api.invoices.delete_draft_invoices(created_before=dt.datetime.now(dt.UTC) + dt.timedelta(days=1)) == 1

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

    # A batch of updates: only the issued invoice is voided.
    batch_void = api.invoices.VoidInvoicesBatchParams
    fourth = await api.invoices.create_invoice(
        customer_id=bob.id,
        number="2026-0004",
        currency="EUR",
        issued_on=None,
        due_on=None,
        notes=None,
    )
    assert await api.invoices.issue_invoice(issued_on=today, id=fourth.id) == 1
    assert await api.invoices.void_invoices_batch([batch_void(id=fourth.id), batch_void(id=first.id)]) is None
    assert await api.invoices.void_invoices_batch([]) is None
    assert await api.invoices.get_invoice_status(id=fourth.id) is models.InvoiceStatus.VOID
    assert await api.invoices.get_invoice_status(id=first.id) is models.InvoiceStatus.PAID

    await streams_lazily(api, api.countdown)
