"""Every generated function of the ledger, run against SQLite: no server needed.

One scenario drives the ledger API, through `ledger_sqlite3` on `sqlite3` and
`ledger_sqlite3_async` on `aiosqlite`, and through `ledger_sqlalchemy` and
`ledger_sqlalchemy_async` on SQLAlchemy's `sqlite` and `sqlite+aiosqlite`. As in `test_runtime.py`, every
call is checked against its annotations, and every function must be called.
"""

import asyncio
import dataclasses
import sqlite3
from typing import Any

import aiosqlite
import pytest
import sqlalchemy
from examples import GoldenCase
from sqlalchemy.ext.asyncio import create_async_engine

from conftest import cases, functions, no_row, queries

SCHEMA = cases("ledger")[0].schema_sql.read_text()
STATEMENTS = [s for s in SCHEMA.split(";") if s.strip()]
# Characters a driver or a careless generator could misread.
AWKWARD = "it's :x $1 ?2 %s"

RUNS = [pytest.param(case, id=case.name) for case in cases("ledger")]


@pytest.mark.parametrize("case", RUNS)
def test_every_function_against_sqlite(case: GoldenCase) -> None:
    called: set[str] = set()
    run = {
        ("sqlite3", False): _sqlite3,
        ("sqlite3", True): _aiosqlite,
        ("sqlalchemy", False): _sqlalchemy_sync,
        ("sqlalchemy", True): _sqlalchemy_async,
    }[case.backend, case.is_async]
    asyncio.run(run(case, called))
    public = functions(case)
    assert called == public, f"never called: {sorted(public - called)}"


def _dicts(cursor: sqlite3.Cursor, row: tuple[Any, ...]) -> dict[str, Any]:
    """A row factory of the caller's, which generated code must not read."""
    return {
        column[0]: value for column, value in zip(cursor.description, row, strict=True)
    }


async def _sqlite3(case: GoldenCase, called: set[str]) -> None:
    conn = sqlite3.connect(":memory:")
    conn.row_factory = _dicts
    try:
        conn.executescript(SCHEMA)
        await scenario(_api(case, conn, called))
    finally:
        conn.close()


async def _aiosqlite(case: GoldenCase, called: set[str]) -> None:
    async with aiosqlite.connect(":memory:") as conn:
        conn.row_factory = _dicts
        await conn.executescript(SCHEMA)
        await scenario(_api(case, conn, called))


async def _sqlalchemy_sync(case: GoldenCase, called: set[str]) -> None:
    engine = sqlalchemy.create_engine("sqlite://")
    try:
        with engine.connect() as conn:
            for statement in STATEMENTS:
                conn.exec_driver_sql(statement)
            await scenario(_api(case, conn, called))
    finally:
        engine.dispose()


async def _sqlalchemy_async(case: GoldenCase, called: set[str]) -> None:
    engine = create_async_engine("sqlite+aiosqlite://")
    try:
        async with engine.connect() as conn:
            for statement in STATEMENTS:
                await conn.exec_driver_sql(statement)
            await scenario(_api(case, conn, called))
    finally:
        await engine.dispose()


def _api(case: GoldenCase, conn: object, called: set[str]) -> Any:  # noqa: ANN401
    return queries(case, conn, called)


async def scenario(api: Any) -> None:  # noqa: ANN401
    q = api.query
    cash = await q.open_account(
        name="Cash", opened_on="2026-11-13", settings='{"a": 1}'
    )
    assert (cash.name, cash.archived, cash.opened_on) == ("Cash", False, "2026-11-13")
    assert cash.settings == '{"a": 1}'
    assert await q.get_account(id=cash.id) == cash
    await no_row(api, q.get_account(id=cash.id + 1))
    assert await q.get_account_by_name(name="Cash") == cash

    # A BOOLEAN is stored as 0 or 1, and read back as a bool.
    assert await q.is_archived(id=cash.id) is False
    assert await q.archive_account(id=cash.id) == 1
    assert await q.archive_account(id=cash.id) == 0
    assert await q.is_archived(id=cash.id) is True
    archived = dataclasses.replace(cash, archived=True)
    assert await q.list_accounts() == [archived]
    # A slice: several ids, and none.
    assert await q.list_accounts_by_ids(ids=[cash.id + 1, cash.id]) == [archived]
    assert await q.list_accounts_by_ids(ids=[]) == []

    receipt = bytes(range(256))
    assert (
        await q.book(
            account_id=cash.id,
            amount_cents=1250,
            rate=0.5,
            memo=AWKWARD,
            receipt=receipt,
        )
        is None
    )
    second = await q.book_returning_id(account_id=cash.id, amount_cents=-250, memo=None)
    assert await q.balance(account_id=cash.id) == 1000
    entries = await q.list_entries(account_id=cash.id)
    assert [(e.amount_cents, e.rate, e.memo, e.receipt) for e in entries] == [
        (1250, 0.5, AWKWARD, receipt),
        (-250, None, None, None),
    ]
    assert entries[1].id == second
    assert (await q.get_last_amount(id=cash.id)).last_amount_cents == -250
    # A slice's items, then `?2`: each bound in its own place.
    ids = [e.id for e in entries]
    assert await q.list_entry_ids(ids=[*ids, 0], account_id=cash.id) == ids
    assert await q.list_entry_ids(ids=ids, account_id=cash.id + 1) == []
    assert await q.list_entry_ids(ids=[], account_id=cash.id) == []
    # One parameter used twice is one argument (`?2`).
    assert await q.list_memos(pattern="it%", account_id=0) == [AWKWARD]
    assert await q.list_memos(pattern="%", account_id=cash.id) == [AWKWARD]
    assert await q.list_memos(pattern="%", account_id=cash.id + 1) == []

    joined = await q.list_entries_with_account()
    assert [(row.entry, row.account) for row in joined] == [
        (entry, archived) for entry in entries
    ]
    # A LEFT JOIN's embed: the entries, then none at all.
    outer = await q.list_accounts_with_entries()
    assert [(row.account, row.entry) for row in outer] == [
        (archived, entry) for entry in entries
    ]
    assert (await q.delete_entries(account_id=cash.id)).rowcount == 2
    assert await q.balance(account_id=cash.id) == 0
    outer = await q.list_accounts_with_entries()
    assert [(row.account, row.entry) for row in outer] == [(archived, None)]
    # A scalar subquery that finds no row is None, though sqlc says NOT NULL.
    last = await q.get_last_amount(id=cash.id)
    assert (last.name, last.last_amount_cents) == ("Cash", None)
