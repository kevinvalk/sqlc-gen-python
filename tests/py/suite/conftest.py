"""Shared fixtures: where the expected output lives, and a real PostgreSQL.

The suite proves the invoicing golden cases (`tests/cases/invoicing_*/*.py`)
correct against a live database.

- `SQLC_CASES` overrides the cases directory: `verify.py` points it at the
  golden cases as generated (or a planted-bug copy of them).
- The database is `postgres.server()`'s: `SQLC_TEST_DATABASE_URL`, or a
  throwaway Docker container of the newest PostgreSQL.
"""

import importlib
import inspect
import json
import os
import sys
from collections.abc import Iterator
from dataclasses import dataclass
from functools import cached_property
from pathlib import Path
from types import ModuleType, SimpleNamespace
from typing import Any

import postgres
import pytest
import sqlalchemy

ROOT = Path(__file__).resolve().parents[3]
CASES = Path(os.environ.get("SQLC_CASES", ROOT / "tests/cases"))
SCHEMAS = ROOT / "tests/schemas/src"

sys.path.insert(0, str(CASES))
sys.dont_write_bytecode = True  # keep the golden cases free of __pycache__


@dataclass(frozen=True)
class Case:
    """One golden case directory."""

    dir: Path

    @cached_property
    def spec(self) -> dict[str, Any]:
        spec: dict[str, Any] = json.loads((self.dir / "case.json").read_text())
        return spec

    @property
    def name(self) -> str:
        return self.dir.name

    @property
    def schema(self) -> str:
        schema: str = self.spec["schema"]
        return schema

    @property
    def backend(self) -> str:
        backend: str = self.spec["options"]["backend"]
        return backend

    @property
    def is_async(self) -> bool:
        return bool(self.spec["options"]["async"])

    def module(self, stem: str) -> ModuleType:
        return importlib.import_module(f"{self.name}.{stem}")

    def query_modules(self) -> list[str]:
        """The module of each query file."""
        stems = {path.stem for path in self.dir.glob("*.py")}
        return sorted(stems - {"__init__", "models"})

    def functions(self, stem: str) -> dict[str, Any]:
        """The public functions of query file ``stem``: coroutines in an async
        case.
        """
        module = self.module(stem)
        return {
            name: fn
            for name, fn in inspect.getmembers(module, inspect.isfunction)
            if fn.__module__ == module.__name__ and not name.startswith("_")
        }


def cases(schema: str = "invoicing") -> list[Case]:
    """The cases of ``schema``: the suite is written for the invoicing and
    ledger fixtures. The other golden cases beside them are for the static
    gate and the runtime sweep.
    """
    found = (Case(path.parent) for path in sorted(CASES.glob("*/case.json")))
    return [case for case in found if case.schema == schema]


def queries(case: Case, conn: object, called: set[str]) -> Any:  # noqa: ANN401
    """The case's API on ``conn``: ``await api.<module>.<function>(...)`` runs
    a sync or an async case alike, each function checked by `deep.checked`, which records it in
    ``called``. Every module's names are there too (``api.<module>.<Row>``),
    and the package's ``models``.
    """
    from deep import checked  # noqa: PLC0415 -- the suite's own module

    def bind(function: Any) -> Any:  # noqa: ANN401
        function = checked(function, called)

        async def call(*args: object, **kwargs: object) -> object:
            result = function(conn, *args, **kwargs)
            return await result if inspect.isawaitable(result) else result

        return call

    modules = {}
    for stem in case.query_modules():
        functions = case.functions(stem)
        bound = {name: bind(fn) for name, fn in functions.items()}
        modules[stem] = SimpleNamespace(**{**vars(case.module(stem)), **bound})
    return SimpleNamespace(models=case.module("models"), **modules)


def functions(case: Case) -> set[str]:
    """Every public function of the case's API, as `deep.checked` names it."""
    return {
        f"{fn.__module__}.{fn.__qualname__}"
        for stem in case.query_modules()
        for fn in case.functions(stem).values()
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
    with postgres.database(
        server, (SCHEMAS / "invoicing/schema.sql").read_text()
    ) as url:
        yield url
