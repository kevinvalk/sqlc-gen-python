"""Shared fixtures: a real PostgreSQL, and no silent skip.

The suite proves the golden cases of the examples (`examples/*/<case>/`)
correct against a live database; `api.py` loads them. The database is
`postgres.server()`'s: `SQLC_TEST_DATABASE_URL`, or a throwaway Docker
container of the newest PostgreSQL.
"""

from collections.abc import Awaitable, Iterator
from typing import Any

import postgres
import pytest
import sqlalchemy
from examples import golden_cases


async def no_row(api: Any, call: Awaitable[object]) -> None:  # noqa: ANN401
    """A `:one` without a row: `None`, or with `raise_on_none`, the error."""
    if api.raises:
        with pytest.raises(api.NoRow):
            await call
    else:
        assert await call is None


@pytest.fixture(scope="session")
def server() -> Iterator[sqlalchemy.URL]:
    try:
        with postgres.server() as url:
            yield url
    except SystemExit as error:  # end the session once, not fail every test
        pytest.exit(str(error), returncode=1)


def pytest_sessionfinish(session: pytest.Session) -> None:
    """Fail a run that skipped anything, or expected a failure: there is no
    silent skip.
    """
    reporter = session.config.pluginmanager.get_plugin("terminalreporter")
    assert reporter is not None, "the verdict needs the terminal reporter"
    unproven = [k for k in ("skipped", "xfailed", "xpassed") if reporter.stats.get(k)]
    if unproven and session.exitstatus == pytest.ExitCode.OK:
        reporter.write("\n")  # end the progress line
        reporter.write_line(f"FAILED: a test was {unproven[0]}", red=True)
        session.exitstatus = pytest.ExitCode.TESTS_FAILED


@pytest.fixture(scope="session")
def database(server: sqlalchemy.URL) -> Iterator[sqlalchemy.URL]:
    """A database holding the invoicing schema, dropped afterwards."""
    schema = next(c for c in golden_cases() if c.example == "invoicing").schema_sql
    with postgres.database(server, schema.read_text()) as url:
        yield url
