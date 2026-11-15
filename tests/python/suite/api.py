import importlib
import inspect
import os
import pkgutil
import sys
import typing
from collections.abc import Awaitable
from pathlib import Path
from types import ModuleType, SimpleNamespace
from typing import Any

import pytest
import sqlalchemy
from examples import GoldenCase, golden_cases
from suite.deep import checked

# NOTE: `verify.py` points `SQLC_CASES` at a directory of `<case>/` it laid out (the golden cases as generated, or a
# copy with a planted bug). A case's options still come from its `sqlc.yaml` either way.
CASES = Path(os.environ["SQLC_CASES"]) if "SQLC_CASES" in os.environ else None

# Each case is a package named after it, in `CASES` or beside its example's SQL.
for path in [CASES] if CASES is not None else sorted({case.dir.parent for case in golden_cases()}):
    sys.path.insert(0, str(path))
sys.dont_write_bytecode = True  # We keep the golden cases free of __pycache__


def cases(example: str = "invoicing") -> list[GoldenCase]:
    """The suite's scenarios are written for the invoicing, ledger, billing and custom examples, and the type-truth
    probe reads the types examples. The other golden cases are only for the static gate and the runtime sweep.
    """
    return [case for case in golden_cases() if case.example == example]


def module(case: GoldenCase, stem: str) -> ModuleType:
    return importlib.import_module(f"{case.name}.{stem}")


def query_modules(case: GoldenCase) -> list[str]:
    package = importlib.import_module(case.name)
    stems = {info.name for info in pkgutil.iter_modules(package.__path__)}
    return sorted(stems - {"models"})


def public_functions(case: GoldenCase, stem: str) -> dict[str, Any]:
    query_module = module(case, stem)
    return {
        name: function
        for name, function in inspect.getmembers(query_module, inspect.isfunction)
        if function.__module__ == query_module.__name__ and not name.startswith("_")
    }


def queries(case: GoldenCase, connection: object, called: set[str]) -> Any:
    """The case's API bound to ``connection``, so ``await api.<module>.<function>(...)`` runs a sync and an async case
    alike. Each function is checked by `deep.checked`, which records it in ``called``, and every module's other names
    (``api.<module>.<Row>``) and the package's ``models`` are there too.
    """

    def bind(function: Any) -> Any:
        # A function with more parameters than `max_params` takes one record of them, so we build it from the
        # scenario's keywords.
        parameter = inspect.signature(function).parameters.get("params")
        record = None
        if parameter is not None and parameter.kind is parameter.POSITIONAL_OR_KEYWORD:
            record = typing.get_type_hints(function)["params"]
        function = checked(function, called)

        async def call(*args: object, **kwargs: object) -> object:
            if record is not None:
                args, kwargs = (record(**kwargs),), {}
            result = function(connection, *args, **kwargs)
            return await result if inspect.isawaitable(result) else result

        return call

    modules = {}
    for stem in query_modules(case):
        bound = {name: bind(function) for name, function in public_functions(case, stem).items()}
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


async def no_row(api: Any, call: Awaitable[object]) -> None:
    """A `:one` without a row returns `None`, or raises with `raise_on_none`."""
    if api.raises:
        with pytest.raises(api.NoRow):
            await call
    else:
        assert await call is None


def functions(case: GoldenCase) -> set[str]:
    """Every public function of the case's API, named as `deep.checked` names it."""
    return {
        f"{function.__module__}.{function.__qualname__}"
        for stem in query_modules(case)
        for function in public_functions(case, stem).values()
    }
