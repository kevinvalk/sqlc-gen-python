"""Every generated function of the ledger, run against SQLite on each of its four drivers.

As in `test_runtime.py`, every call is checked against its annotations and every function must be called.
"""

import asyncio
import dataclasses
import sqlite3
from typing import Any

import aiosqlite
import pytest
from examples import GoldenCase, statements
from postgres import aw
from suite.api import calls_every_function, cases, no_row, queries, sqlalchemy_connection, streams_lazily

SCHEMA = cases("ledger")[0].schema_sql.read_text()
STATEMENTS = statements(SCHEMA)
AWKWARD = "it's :x $1 ?2 %s"  # Characters a driver or a careless generator could misread

RUNS = [pytest.param(case, id=case.name) for case in cases("ledger")]


@pytest.mark.parametrize("case", RUNS)
def test_every_function_against_sqlite(case: GoldenCase) -> None:
    run = {
        ("sqlite3", False): _sqlite3,
        ("sqlite3", True): _aiosqlite,
        ("sqlalchemy", False): _sqlalchemy,
        ("sqlalchemy", True): _sqlalchemy,
    }[case.backend, case.is_async]
    with calls_every_function(case) as called:
        asyncio.run(run(case, called))


class _Dict(dict[str, Any]):
    """A row factory of the caller's, which generated code must not read rows through. It is a class, as that is all
    aiosqlite's stubs take for one.
    """

    def __init__(self, cursor: sqlite3.Cursor, row: tuple[Any, ...]) -> None:
        names = (column[0] for column in cursor.description)
        super().__init__(zip(names, row, strict=True))


async def _sqlite3(case: GoldenCase, called: set[str]) -> None:
    connection = sqlite3.connect(":memory:")
    connection.row_factory = _Dict
    try:
        connection.executescript(SCHEMA)
        await scenario(queries(case, connection, called))
    finally:
        connection.close()


async def _aiosqlite(case: GoldenCase, called: set[str]) -> None:
    async with aiosqlite.connect(":memory:") as connection:
        connection.row_factory = _Dict
        await connection.executescript(SCHEMA)
        await scenario(queries(case, connection, called))


async def _sqlalchemy(case: GoldenCase, called: set[str]) -> None:
    url = "sqlite+aiosqlite://" if case.is_async else "sqlite://"
    async with sqlalchemy_connection(url, asynchronous=case.is_async) as connection:
        for statement in STATEMENTS:
            await aw(connection.exec_driver_sql(statement))
        await scenario(queries(case, connection, called))


async def scenario(api: Any) -> None:  # noqa: PLR0915
    query = api.query
    cash = await query.open_account(name="Cash", opened_on="2026-11-13", settings={"a": 1})
    assert (cash.name, cash.archived, cash.opened_on) == ("Cash", False, "2026-11-13")
    assert cash.settings == {"a": 1}
    await no_row(api, query.open_account(name="Cash", opened_on="2026-11-13"))
    assert await query.get_account(id=cash.id) == cash
    await no_row(api, query.get_account(id=cash.id + 1))
    quoted = await query.get_quoted_text(id=cash.id)
    assert (quoted.triple_quotes, quoted.backslash, quoted.name) == ('"""', "\\ %s :x", "Cash")
    assert await query.get_account_by_name(name="Cash") == cash

    # A BOOLEAN is stored as 0 or 1, and read back as a bool.
    assert await query.is_archived(id=cash.id) is False
    assert await query.archive_account(id=cash.id) == 1
    assert await query.archive_account(id=cash.id) == 0
    assert await query.is_archived(id=cash.id) is True
    archived = dataclasses.replace(cash, archived=True)
    assert await query.list_accounts() == [archived]
    # A slice: several ids, and none.
    assert await query.list_accounts_by_ids(ids=[cash.id + 1, cash.id]) == [archived]
    assert await query.list_accounts_by_ids(ids=[]) == []
    # Of none, `NOT IN` keeps every row, where `NOT IN (NULL)` would keep none.
    assert await query.list_accounts_except(ids=[]) == [archived]
    assert await query.list_accounts_except(ids=[cash.id]) == []

    receipt = bytes(range(256))
    assert (
        await query.book(
            account_id=cash.id,
            amount_cents=1250,
            rate=0.5,
            memo=AWKWARD,
            receipt=receipt,
        )
        is None
    )
    second = await query.book_returning_id(account_id=cash.id, amount_cents=-250, memo=None)
    assert await query.balance(account_id=cash.id) == 1000
    entries = await query.list_entries(account_id=cash.id)
    assert [(e.amount_cents, e.rate, e.memo, e.receipt) for e in entries] == [
        (1250, 0.5, AWKWARD, receipt),
        (-250, None, None, None),
    ]
    assert (await query.compare_memo(id=second)).empty_memo is None
    assert (await query.compare_memo(id=entries[0].id)).empty_memo is False
    assert entries[1].id == second
    # A count of the rows a filter keeps, and one over a window.
    counted = await query.count_entries(account_id=cash.id)
    assert (counted.entries, counted.debits) == (2, 1)
    none = await query.count_entries(account_id=cash.id + 1)
    assert (none.entries, none.debits) == (0, 0)
    amounts = await query.list_amounts(account_id=cash.id)
    assert [(a.amount_cents, a.entries) for a in amounts] == [(1250, 2), (-250, 2)]
    assert await query.list_amounts(account_id=cash.id + 1) == []
    assert (await query.get_last_amount(id=cash.id)).last_amount_cents == -250
    # A slice's items, then `?2`: each bound in its own place.
    ids = [e.id for e in entries]
    assert await query.list_entry_ids(ids=[*ids, 0], account_id=cash.id) == ids
    assert await query.list_entry_ids(ids=ids, account_id=cash.id + 1) == []
    assert await query.list_entry_ids(ids=[], account_id=cash.id) == []
    # One parameter used twice is one argument (`?2`).
    assert await query.list_memos(pattern="it%", account_id=0) == [AWKWARD]
    assert await query.list_memos(pattern="%", account_id=cash.id) == [AWKWARD]
    assert await query.list_memos(pattern="%", account_id=cash.id + 1) == []

    joined = await query.list_entries_with_account()
    assert [(row.entry, row.account) for row in joined] == [(entry, archived) for entry in entries]
    # A LEFT JOIN's embed: the entries, then none at all.
    outer = await query.list_accounts_with_entries()
    assert [(row.account, row.entry) for row in outer] == [(archived, entry) for entry in entries]
    shares = await query.list_shares()
    assert [(s.name, s.amount_cents, s.total_cents) for s in shares] == [
        ("Cash", 1250, 1000),
        ("Cash", -250, 1000),
    ]
    assert (await query.delete_entries(account_id=cash.id)).rowcount == 2
    assert await query.balance(account_id=cash.id) == 0
    outer = await query.list_accounts_with_entries()
    assert [(row.account, row.entry) for row in outer] == [(archived, None)]
    # And so is its column, though sqlc says NOT NULL once a JOIN follows.
    shares = await query.list_shares()
    assert [(s.name, s.amount_cents, s.total_cents) for s in shares] == [("Cash", None, 0)]
    # A scalar subquery that finds no row is None, though sqlc says NOT NULL.
    last = await query.get_last_amount(id=cash.id)
    assert (last.name, last.last_amount_cents) == ("Cash", None)

    # A `:copyfrom`: the INSERT once per row, and never for none.
    rows = [
        query.BookManyParams(account_id=cash.id, amount_cents=300, memo=AWKWARD),
        query.BookManyParams(account_id=cash.id, amount_cents=200, memo=None),
    ]
    assert await query.book_many(rows=rows) == 2
    assert await query.book_many(rows=[]) == 0
    assert await query.balance(account_id=cash.id) == 500

    await streams_lazily(api, query)
