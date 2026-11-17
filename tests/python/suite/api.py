import contextlib
import importlib
import inspect
import os
import pkgutil
import sys
import typing
from collections.abc import AsyncIterator, Awaitable, Iterator
from pathlib import Path
from types import ModuleType, SimpleNamespace
from typing import Any

import psycopg
import pytest
import sqlalchemy
from examples import GoldenCase, golden_cases
from postgres import aw
from sqlalchemy.ext.asyncio import create_async_engine
from suite.deep import checked, is_stream, same, streamed

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

    def bind(function: Any, stream: Any) -> Any:
        # A function with more parameters than `max_params` takes one record of them, so we build it from the
        # scenario's keywords.
        parameter = inspect.signature(function).parameters.get("params")
        record = None
        if parameter is not None and parameter.kind is parameter.POSITIONAL_OR_KEYWORD:
            record = typing.get_type_hints(function)["params"]
        function = checked(function, called)
        stream = None if stream is None else streamed(stream, called)

        async def call(*args: object, **kwargs: object) -> object:
            if record is not None:
                args, kwargs = (record(**kwargs),), {}
            result = await aw(function(connection, *args, **kwargs))

            # Every scenario of a `:many` that only reads proves its stream reads the very same rows, in order. asyncmy
            # reads an unbuffered cursor's rows as text even where it prepares statements, and MySQL writes a `FLOAT`
            # as text to six digits.
            if stream is not None:
                async with transaction(connection):
                    rows = await aw(stream(connection, *args, **kwargs))
                assert same(rows, result, digits=6), f"{stream.__name__} read {rows!r}, not {result!r}"
            return result

        return call

    modules = {}
    for stem in query_modules(case):
        public = public_functions(case, stem)
        bound = {
            name: opened(function, connection, called)
            if is_stream(function)
            else bind(function, public.get(f"stream_{name}"))
            for name, function in public.items()
        }
        modules[stem] = SimpleNamespace(**{**vars(module(case, stem)), **bound})
    api = SimpleNamespace(models=module(case, "models"), **modules)

    api.rollback = lambda: aw(typing.cast("Any", connection).rollback())

    # What a `:one` raises: SQLAlchemy's errors, or those of the package's models.
    api.raises = case.options.get("raise_on_none") is True
    if case.backend == "sqlalchemy":
        api.NoRow = sqlalchemy.exc.NoResultFound
        api.MultipleRows = sqlalchemy.exc.MultipleResultsFound
    else:
        api.NoRow = getattr(api.models, "NoRowError", None)
        api.MultipleRows = api.models.MultipleRowsError
    return api


@contextlib.asynccontextmanager
async def transaction(connection: object) -> AsyncIterator[None]:
    """A transaction on an autocommit psycopg connection, as PostgreSQL only opens the cursor a stream reads in one."""
    if isinstance(connection, psycopg.AsyncConnection) and connection.autocommit:
        async with connection.transaction():
            yield
    elif isinstance(connection, psycopg.Connection) and connection.autocommit:
        with connection.transaction():
            yield
    else:
        yield


# What each database says as the countdown fails, through any driver
FAILED = "division by zero|malformed JSON|Invalid JSON text"


async def streams_lazily(api: Any, module: Any) -> None:
    """The countdown fails part of the way. Its function reads every row before it returns, so it never returns one,
    while its stream reads each row as it is iterated, and so the rows before. It runs last, as each failure rolls the
    transaction back.
    """
    with pytest.raises(Exception, match=FAILED):
        await module.countdown()
    await api.rollback()

    read: list[int] = []
    with pytest.raises(Exception, match=FAILED):
        await read_into(module.stream_countdown(), read)
    await api.rollback()
    assert len(read) > 0, "the stream read every row before it yielded one"


async def read_into(stream: object, read: list[int]) -> None:
    """Read a stream's rows into ``read`` one at a time, so those read before it fails are there."""
    if isinstance(stream, contextlib.AbstractAsyncContextManager):
        async with stream as rows:
            async for row in typing.cast("AsyncIterator[int]", rows):
                read.append(row)  # noqa: PERF401 -- a comprehension would lose the rows when the stream fails
    else:
        with typing.cast("contextlib.AbstractContextManager[Iterator[int]]", stream) as rows:
            for row in rows:
                read.append(row)  # noqa: PERF402 -- the same


def opened(stream: Any, connection: object, called: set[str]) -> Any:
    """A stream on ``connection``, to enter as it is, as the countdown proves it reads each row as it is iterated."""

    def call(*args: object, **kwargs: object) -> object:
        called.add(f"{stream.__module__}.{stream.__qualname__}")
        return stream(connection, *args, **kwargs)

    return call


async def no_row(api: Any, call: Awaitable[object]) -> None:
    """A `:one` without a row returns `None`, or raises with `raise_on_none`."""
    if api.raises:
        with pytest.raises(api.NoRow):
            await call
    else:
        assert await call is None


@contextlib.contextmanager
def calls_every_function(case: GoldenCase) -> Iterator[set[str]]:
    """The set each call of the case's API (``queries``) records its function in, which must hold every public function
    of the case once the block is done.
    """
    called: set[str] = set()
    yield called
    public = functions(case)
    assert called == public, f"never called: {sorted(public - called)}"


@contextlib.asynccontextmanager
async def sqlalchemy_connection(url: str | sqlalchemy.URL, *, asynchronous: bool) -> AsyncIterator[Any]:
    """A connection of an engine of its own, sync or async, rolled back and disposed of after."""
    engine: Any = create_async_engine(url) if asynchronous else sqlalchemy.create_engine(url)
    try:
        connection = await aw(engine.connect())
        try:
            yield connection
            await aw(connection.rollback())
        finally:
            await aw(connection.close())
    finally:
        await aw(engine.dispose())


def functions(case: GoldenCase) -> set[str]:
    """Every public function of the case's API, named as `deep.checked` names it."""
    return {
        f"{function.__module__}.{function.__qualname__}"
        for stem in query_modules(case)
        for function in public_functions(case, stem).values()
    }
