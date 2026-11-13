"""Shared fixtures: where the expected output lives, and a real PostgreSQL.

The suite proves the golden cases of the examples (`examples/*/<case>/`)
correct against a live database.

- `SQLC_CASES` overrides where the cases are: a directory of `<case>/`, as
  `verify.py` lays out the golden cases as generated (or a planted-bug copy of
  them). Their options are always their `sqlc.yaml`'s.
- The database is `postgres.server()`'s: `SQLC_TEST_DATABASE_URL`, or a
  throwaway Docker container of the newest PostgreSQL.
"""

import importlib
import inspect
import os
import pkgutil
import sys
import typing
from collections.abc import Awaitable, Iterator
from pathlib import Path
from types import ModuleType, SimpleNamespace
from typing import Any

import postgres
import pytest
import sqlalchemy
from examples import GoldenCase, golden_cases

ROOT = Path(__file__).resolve().parents[3]
CASES = Path(os.environ["SQLC_CASES"]) if "SQLC_CASES" in os.environ else None

# Each case is a package named for it: in `CASES`, or beside its example's SQL.
for path in [CASES] if CASES else sorted({c.dir.parent for c in golden_cases()}):
    sys.path.insert(0, str(path))
sys.dont_write_bytecode = True  # keep the golden cases free of __pycache__


def cases(example: str = "invoicing") -> list[GoldenCase]:
    """The cases of ``example``: the suite is written for the invoicing,
    ledger, billing and custom fixtures. The other golden cases beside them
    are for the static gate and the runtime sweep.
    """
    return [case for case in golden_cases() if case.example == example]


def module(case: GoldenCase, stem: str) -> ModuleType:
    """The case's module ``stem``."""
    return importlib.import_module(f"{case.name}.{stem}")


def query_modules(case: GoldenCase) -> list[str]:
    """The module of each query file."""
    package = importlib.import_module(case.name)
    stems = {info.name for info in pkgutil.iter_modules(package.__path__)}
    return sorted(stems - {"models"})


def public_functions(case: GoldenCase, stem: str) -> dict[str, Any]:
    """The public functions of query file ``stem``: coroutines in an async
    case.
    """
    found = module(case, stem)
    return {
        name: fn
        for name, fn in inspect.getmembers(found, inspect.isfunction)
        if fn.__module__ == found.__name__ and not name.startswith("_")
    }


def queries(case: GoldenCase, conn: object, called: set[str]) -> Any:  # noqa: ANN401
    """The case's API on ``conn``: ``await api.<module>.<function>(...)`` runs
    a sync or an async case alike, each function checked by `deep.checked`, which records it in
    ``called``. Every module's names are there too (``api.<module>.<Row>``),
    and the package's ``models``.
    """
    from deep import checked  # noqa: PLC0415 -- the suite's own module

    def bind(function: Any) -> Any:  # noqa: ANN401
        # A function of more parameters than `max_params` takes one record
        # of them: the scenario's keywords make it.
        parameter = inspect.signature(function).parameters.get("params")
        record = None
        if parameter is not None and parameter.kind is parameter.POSITIONAL_OR_KEYWORD:
            record = typing.get_type_hints(function)["params"]
        function = checked(function, called)

        async def call(*args: object, **kwargs: object) -> object:
            if record is not None:
                args, kwargs = (record(**kwargs),), {}
            result = function(conn, *args, **kwargs)
            return await result if inspect.isawaitable(result) else result

        return call

    modules = {}
    for stem in query_modules(case):
        bound = {name: bind(fn) for name, fn in public_functions(case, stem).items()}
        modules[stem] = SimpleNamespace(**{**vars(module(case, stem)), **bound})
    api = SimpleNamespace(models=module(case, "models"), **modules)
    # What a `:one` raises: SQLAlchemy's errors, or the package's own.
    api.raises = case.options.get("raise_on_none") is True
    if case.backend == "sqlalchemy":
        api.NoRow = sqlalchemy.exc.NoResultFound
        api.MultipleRows = sqlalchemy.exc.MultipleResultsFound
    else:
        package = importlib.import_module(case.name)
        api.NoRow = getattr(package, "NoRowError", None)
        api.MultipleRows = package.MultipleRowsError
    return api


async def no_row(api: Any, call: Awaitable[object]) -> None:  # noqa: ANN401
    """A `:one` without a row: `None`, or with `raise_on_none`, the error."""
    if api.raises:
        with pytest.raises(api.NoRow):
            await call
    else:
        assert await call is None


def functions(case: GoldenCase) -> set[str]:
    """Every public function of the case's API, as `deep.checked` names it."""
    return {
        f"{fn.__module__}.{fn.__qualname__}"
        for stem in query_modules(case)
        for fn in public_functions(case, stem).values()
    }


@pytest.fixture(scope="session")
def server() -> Iterator[sqlalchemy.URL]:
    try:
        with postgres.server() as url:
            yield url
    except SystemExit as error:  # end the session once, not fail every test
        pytest.exit(str(error), returncode=1)


def pytest_sessionfinish(session: pytest.Session) -> None:
    """Fail a run that skipped anything: there is no silent skip."""
    reporter = session.config.pluginmanager.get_plugin("terminalreporter")
    assert reporter is not None, "the verdict needs the terminal reporter"
    if reporter.stats.get("skipped") and session.exitstatus == pytest.ExitCode.OK:
        reporter.write("\n")  # end the progress line
        reporter.write_line("FAILED: a test was skipped", red=True)
        session.exitstatus = pytest.ExitCode.TESTS_FAILED


@pytest.fixture(scope="session")
def database(server: sqlalchemy.URL) -> Iterator[sqlalchemy.URL]:
    """A database holding the invoicing schema, dropped afterwards."""
    schema = next(c for c in golden_cases() if c.example == "invoicing").schema_sql
    with postgres.database(server, schema.read_text()) as url:
        yield url
