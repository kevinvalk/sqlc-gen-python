from collections.abc import Callable, Iterator
from contextlib import AbstractContextManager

import mysql
import postgres
import pytest
import sqlalchemy
from examples import golden_cases


def _running(
    server: Callable[[], AbstractContextManager[sqlalchemy.URL]],
) -> Iterator[sqlalchemy.URL]:
    try:
        with server() as url:
            yield url
    except SystemExit as error:  # We end the session once instead of failing every test
        pytest.exit(str(error), returncode=1)


@pytest.fixture(scope="session")
def server() -> Iterator[sqlalchemy.URL]:
    yield from _running(postgres.server)


@pytest.fixture(scope="session")
def mysql_server() -> Iterator[sqlalchemy.URL]:
    yield from _running(mysql.server)


def pytest_sessionfinish(session: pytest.Session) -> None:
    # A proof that skipped a test, or expected one to fail, did not prove it, so we fail the run.
    reporter = session.config.pluginmanager.get_plugin("terminalreporter")
    assert reporter is not None, "the verdict needs the terminal reporter"
    unproven = [outcome for outcome in ("skipped", "xfailed", "xpassed") if reporter.stats.get(outcome)]
    if len(unproven) > 0 and session.exitstatus == pytest.ExitCode.OK:
        reporter.write("\n")  # Ends the progress line
        reporter.write_line(f"FAILED: a test was {unproven[0]}", red=True)
        session.exitstatus = pytest.ExitCode.TESTS_FAILED


@pytest.fixture(scope="session")
def database(server: sqlalchemy.URL) -> Iterator[sqlalchemy.URL]:
    schema = next(case for case in golden_cases() if case.example == "invoicing").schema_sql
    # We skip position zero so a bulk insert has to count what PostgreSQL wrote.
    trigger = """
        CREATE FUNCTION skip_line() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RETURN NULL; END $$;
        CREATE TRIGGER skip_line BEFORE INSERT ON line_items
        FOR EACH ROW WHEN (NEW.position = 0) EXECUTE FUNCTION skip_line();
    """
    with postgres.database(server, schema.read_text() + trigger) as url:
        yield url
