"""What application code sees: checked by the static gate, never executed.

Each `assert_type` pins a public type. Each `# type: ignore[...]` line is a
mistake that must stay a type error: both checkers run with unused-ignore
reporting, so if the generated API ever stops rejecting it, this file fails.
`known_holes` holds the mistakes no annotation can reject.
"""

import contextlib
import datetime as dt
from decimal import Decimal
from typing import Any
from uuid import UUID

import psycopg
import sqlalchemy
from invoicing_psycopg import NoRowError
from invoicing_psycopg import invoices as psycopg_invoices
from invoicing_psycopg import models as psycopg_models
from invoicing_psycopg_async import invoices as psycopg_invoices_async
from invoicing_sqlalchemy import customers, invoices, line_items, models, payments
from invoicing_sqlalchemy_async import invoices as invoices_async
from invoicing_sqlalchemy_async import models as models_async
from sqlalchemy.ext.asyncio import AsyncConnection
from typing_extensions import assert_type


def sync_api(conn: sqlalchemy.Connection, some: UUID) -> None:
    # A `:one` returns its row, and raises `NoResultFound` without one.
    invoice = invoices.get_invoice(conn, id=some)
    assert_type(invoice, models.Invoice)
    assert_type(invoice.status, models.InvoiceStatus)
    assert_type(invoice.due_on, dt.date | None)
    assert_type(invoices.get_invoice_status(conn, id=some), models.InvoiceStatus)
    # A nullable single column is bare: `None` is its NULL.
    assert_type(invoices.get_invoice_due_date(conn, id=some), dt.date | None)
    assert_type(invoices.list_invoice_numbers(conn, customer_id=some), list[str])
    assert_type(invoices.count_invoices(conn, customer_id=some), int)
    assert_type(invoices.issue_invoice(conn, issued_on=None, id=some), int)
    assert_type(invoices.void_invoice(conn, id=some), sqlalchemy.CursorResult[Any])
    totals = invoices.get_invoice_totals(conn, id=some)
    assert_type(totals.subtotal, Decimal)
    joined = invoices.get_invoice_with_customer(conn, id=some)
    assert_type(joined.customer.accepted_methods, list[models.BillingPaymentMethod])
    assert_type(joined.customer.metadata, object | None)
    assert_type(
        customers.list_vip_customers(conn),
        list[customers.ListVipCustomersRow],
    )
    assert_type(line_items.create_line_items(conn, []), None)
    payment = payments.record_payment(
        conn,
        invoice_id=some,
        method=models.BillingPaymentMethod.CARD,
        amount=Decimal("1.00"),
        from_=None,
        received_at=dt.datetime.now(dt.timezone.utc),
    )
    assert_type(payment, models.BillingPayment)


async def async_api(conn: AsyncConnection, some: UUID) -> None:
    assert_type(await invoices_async.get_invoice(conn, id=some), models_async.Invoice)
    assert_type(await invoices_async.count_invoices(conn, customer_id=some), int)


def psycopg_api(conn: psycopg.Connection[Any], some: UUID) -> None:
    # A `:one` returns its row, and raises the package's `NoRowError` without one.
    with contextlib.suppress(NoRowError):
        assert_type(psycopg_invoices.get_invoice(conn, id=some), psycopg_models.Invoice)
    assert_type(psycopg_invoices.get_invoice_due_date(conn, id=some), dt.date | None)
    assert_type(
        psycopg_invoices.void_invoice(conn, id=some),
        psycopg.RawCursor[tuple[Any, ...]],
    )


async def psycopg_async_api(conn: psycopg.AsyncConnection[Any], some: UUID) -> None:
    numbers = await psycopg_invoices_async.list_invoice_numbers(conn, customer_id=some)
    assert_type(numbers, list[str])


def mistakes(
    conn: sqlalchemy.Connection, some: UUID, now: dt.datetime, invoice: models.Invoice
) -> None:
    card = models.BillingPaymentMethod.CARD
    invoices.get_invoice(conn, some)  # type: ignore[call-arg]
    invoices.search_invoices(conn, customer_id=some, status=None)  # type: ignore[call-arg]
    invoices.list_invoices_by_status(conn, statuses=["draft"])  # type: ignore[list-item]
    payments.record_payment(
        conn,  # type: ignore[call-arg]
        invoice_id=some,
        method=card,
        amount=Decimal(1),
        received_at=now,
    )
    payments.record_payment(
        conn,
        invoice_id=some,
        method=card,
        amount=1.5,  # type: ignore[arg-type]
        from_=None,
        received_at=now,
    )
    # A record field omitted is an error, never a NULL written.
    models.Customer(  # type: ignore[call-arg]
        id=some,
        name="a",
        tags=[],
        preferred_method=None,
        accepted_methods=[],
        metadata=None,
        created_at=now,
    )
    line_items.CreateLineItemsParams(  # type: ignore[call-arg]
        invoice_id=some,
        position=1,
        description="a",
        quantity=Decimal(1),
        unit_price=Decimal(1),
    )
    invoice.status = models.InvoiceStatus.PAID  # type: ignore[misc]
    models.Medium(some, some, "a", b"", now)  # type: ignore[call-arg]
    invoices.list_invoices_by_ids(conn, ids=(some,))  # type: ignore[arg-type]


def known_holes(
    conn: sqlalchemy.Connection, some: UUID, now: dt.datetime, customer: models.Customer
) -> None:
    # None is a type error, though each goes wrong at runtime (docs/
    # python-output.md, Known limitations). If one ever becomes one, move it
    # to `mistakes`.
    # A `datetime` is a `date`; the drivers store different days.
    invoices.issue_invoice(conn, issued_on=now, id=some)
    # A `bool` is an `int`; asyncpg writes 1, psycopg raises.
    line_items.CreateLineItemsParams(
        invoice_id=some,
        position=True,
        description="a",
        quantity=Decimal(1),
        unit_price=Decimal(1),
        vat_rate=Decimal(0),
    )
    # A JSON parameter takes any object: every driver raises on a Decimal.
    customers.create_customer(
        conn,
        name="a",
        email=None,
        tags=[],
        preferred_method=None,
        accepted_methods=[],
        metadata={"total": Decimal(1)},
    )
    # A frozen record is hashable to the type checkers, but its list is not.
    hash(customer)
