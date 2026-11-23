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


def test_asyncpg_copyfrom_respects_insert_rules_and_the_callers_savepoint(database: sqlalchemy.URL) -> None:
    case = next(case for case in cases() if case.backend == "sqlalchemy" and case.is_async)
    asyncio.run(_insert_rule(case, database.set(drivername="postgresql+asyncpg")))


async def _insert_rule(case: GoldenCase, url: sqlalchemy.URL) -> None:
    async with sqlalchemy_connection(url, asynchronous=True) as connection:
        api = queries(case, connection, set()).line_items
        # An INSERT rule can write its rows while providing no RETURNING list.
        for statement in (
            "CREATE TEMP TABLE inserted (position integer CHECK (position > 0))",
            (
                "CREATE TEMP VIEW line_items AS SELECT CAST(NULL AS uuid) AS invoice_id, position, "
                "CAST(NULL AS text) AS description, CAST(0 AS numeric) AS quantity, "
                "CAST(0 AS numeric) AS unit_price, CAST(0 AS numeric) AS vat_rate FROM inserted"
            ),
            "CREATE RULE import_line AS ON INSERT TO line_items DO INSTEAD INSERT INTO inserted VALUES (NEW.position)",
        ):
            await connection.exec_driver_sql(statement)

        def line(position: int) -> Any:
            return api.CreateLineItemsParams(
                invoice_id=uuid4(),
                position=position,
                description="rule",
                quantity=Decimal(1),
                unit_price=Decimal(1),
                vat_rate=Decimal(0),
            )

        assert await api.create_line_items([]) == 0
        assert await api.create_line_items([line(1), line(2)]) == 2
        with pytest.raises(sqlalchemy.exc.IntegrityError):
            async with connection.begin_nested():
                await api.create_line_items([line(3), line(-1)])
        assert (
            await connection.exec_driver_sql("SELECT position FROM inserted ORDER BY position")
        ).scalars().all() == [1, 2]


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
        "referral": "conference",
    }
    ada = await api.customers.create_customer(
        name="Ada",
        email=None,
        tags=["vip", "b2b", 'quote"d', "comma,", "", None],
        preferred_method=models.BillingPaymentMethod.DIRECT_DEBIT,
        accepted_methods=[
            models.BillingPaymentMethod.CARD,
            models.BillingPaymentMethod.DIRECT_DEBIT,
            None,
        ],
        metadata=metadata,
    )
    assert await api.customers.count_customers_by_metadata(documents=[["nested", "list"], metadata]) == 1
    assert await api.customers.count_customers_by_metadata(documents=[]) == 0
    assert await api.customers.count_customers_by_metadata(documents=None) == 0
    assert ada.email is None
    assert ada.tags == ["vip", "b2b", 'quote"d', "comma,", "", None]
    assert ada.preferred_method is models.BillingPaymentMethod.DIRECT_DEBIT
    assert ada.accepted_methods == [
        models.BillingPaymentMethod.CARD,
        models.BillingPaymentMethod.DIRECT_DEBIT,
        None,
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
    await sql("UPDATE customers SET tags = ARRAY[['nested']] WHERE id = :id RETURNING id", id=bob.id)
    with pytest.raises(TypeError, match="dimensions"):
        await api.customers.get_customer(id=bob.id)
    await sql("UPDATE customers SET tags = CAST('{}' AS text[]) WHERE id = :id RETURNING id", id=bob.id)
    emails = await api.customers.list_last_emails_by_name()
    assert [(row.name, row.last_email) for row in emails] == [("Ada", None), ("Bob", "bob@example.com")]
    assert [row.emails for row in emails] == [[None], ["bob@example.com"]]
    assert [row.methods for row in emails] == [[models.BillingPaymentMethod.DIRECT_DEBIT], [None]]
    referrals = await api.customers.list_customer_referrals()
    assert [row.email for row in referrals] == [None, "bob@example.com"]
    assert [(row.name, row.referral, row.previous_name) for row in referrals] == [
        ("Ada", "conference", None),
        ("Bob", None, "Ada"),
    ]
    contact = await api.customers.get_customer_contact(id=ada.id)
    assert (contact.name, contact.contact_address) == ("Ada", None)
    assert (await api.customers.get_customer_contact(id=bob.id)).contact_address == "bob@example.com!"
    stored = await sql("SELECT metadata IS NULL FROM customers WHERE id = :id", id=bob.id)
    assert stored, "nullable metadata was stored as JSON null"
    assert await api.customers.set_customer_metadata(id=bob.id, document=None) == 1
    kind = await sql("SELECT jsonb_typeof(metadata) FROM customers WHERE id = :id", id=bob.id)
    assert kind == "null", "the required document was stored as SQL NULL"
    assert await api.customers.count_customers_by_metadata(documents=[None]) == 0
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
    # A CTE named after the table it writes reads the table's own columns
    noted = await api.invoices.set_invoice_notes(notes="call first", id=for_bob.id)
    assert (noted.number, noted.notes, noted.customer_name) == ("2026-0003", "call first", "Bob")
    assert (await api.invoices.set_invoice_notes(notes=None, id=for_bob.id)).notes is None
    # INSERT ... SELECT unnest(...) ON CONFLICT DO NOTHING returns only the rows it wrote
    added = await api.line_items.add_blank_lines(invoice_id=for_bob.id, positions=[1, 2])
    assert [line.position for line in added] == [1, 2]
    added = await api.line_items.add_blank_lines(invoice_id=for_bob.id, positions=[2, 3])
    assert [line.position for line in added] == [3]
    gone = "WITH gone AS (DELETE FROM line_items WHERE invoice_id = :id RETURNING 1) SELECT count(*) FROM gone"
    assert await sql(gone, id=for_bob.id) == 3
    # A partial update leaves what is None alone, and merges a JSON document into the stored one
    before = await api.customers.get_customer(id=ada.id)
    merged = await api.customers.merge_customer_metadata(metadata={"plan": "pro"}, email=None, id=ada.id)
    assert merged.metadata == {**metadata, "plan": "pro"}
    assert merged.email == before.email
    unchanged = await api.customers.merge_customer_metadata(metadata=None, email=None, id=ada.id)
    assert unchanged.metadata == merged.metadata
    assert await api.customers.set_customer_metadata(id=ada.id, document=metadata) == 1
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
    skipped = api.line_items.CreateLineItemsParams(
        invoice_id=first.id,
        position=0,
        description="skipped",
        quantity=Decimal(1),
        unit_price=Decimal(1),
        vat_rate=Decimal(0),
    )
    assert await api.line_items.create_line_items([*lines, skipped]) == 2
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
    # A NULLIF of equal arguments, and an aggregate of a group an outer join left without a row or whose rows its
    # FILTER keeps none of, are None, though sqlc says NOT NULL.
    dearest = await api.invoices.list_dearest_lines(customer_id=ada.id)
    assert [(row.number, row.notes, row.dearest) for row in dearest] == [
        ("2026-0001", AWKWARD, Decimal("100.00")),
        ("2026-0002", None, None),
    ]
    reduced = await api.line_items.sum_lines_below_vat_rate(vat_rate=Decimal("0.2100"))
    assert [(row.invoice_id, row.subtotal) for row in reduced] == [(first.id, Decimal("100.00"))]
    exempt = await api.line_items.sum_lines_below_vat_rate(vat_rate=Decimal("0.0000"))
    assert [(row.invoice_id, row.subtotal) for row in exempt] == [(first.id, None)]

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
    # The key of a ROLLUP is None in the row of them all, though sqlc says NOT NULL.
    rolled = await api.invoices.count_invoices_by_status_and_in_all()
    assert [(row.status, row.count) for row in rolled] == [
        (models.InvoiceStatus.DRAFT, 2),
        (models.InvoiceStatus.ISSUED, 1),
        (None, 3),
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
    # As is what a join in parentheses brings, which we do not read.
    paid_invoices = await api.payments.list_customer_paid_invoices()
    assert [(row.name, row.number, row.amount) for row in paid_invoices] == [
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
