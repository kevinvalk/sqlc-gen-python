"""Throwaway PostgreSQL servers in Docker, and databases on them.

The runtime proof uses ``NEWEST`` or the server ``SQLC_TEST_DATABASE_URL`` names, and the capture passes the image
sqlc's test suite runs with its own variable. We never skip silently: without Docker and without such a variable, the
program ends with an error.
"""

from __future__ import annotations

import inspect
import os
import secrets
import time
from contextlib import contextmanager
from typing import TYPE_CHECKING, Any

import psycopg
import sqlalchemy

from containers import container

if TYPE_CHECKING:
    from collections.abc import Iterator

READY_SECONDS = 60
ENV = "SQLC_TEST_DATABASE_URL"
BACKEND_MEMORY = 2**30  # The data memory in bytes each server process may use
NEWEST = "postgres:18"


@contextmanager
def server(image: str = NEWEST, variable: str = ENV) -> Iterator[sqlalchemy.URL]:
    """Yield a server's psycopg URL, valid for as long as the context lasts: the one the environment ``variable`` names
    when it is set, else a container of ``image``.
    """
    if given := os.environ.get(variable):
        yield sqlalchemy.make_url(given).set(drivername="postgresql+psycopg")
        return

    arguments = [
        "--env=POSTGRES_PASSWORD=postgres",
        # A statement that needs more memory fails with SQLSTATE 53200 instead of the kernel killing the server, as the
        # runtime sweep draws values such as an array index of 10**8.
        f"--ulimit=data={BACKEND_MEMORY}",
        image,
        *["-c", "fsync=off", "-c", "synchronous_commit=off"],
        *["-c", "max_connections=200"],
    ]
    with container("PostgreSQL", variable, 5432, arguments) as port:
        url = sqlalchemy.make_url(f"postgresql+psycopg://postgres:postgres@127.0.0.1:{port}/postgres")
        wait_for(url)
        yield url


class SchemaRejectedError(Exception):
    """PostgreSQL does not accept a schema that sqlc did."""


@contextmanager
def database(server: sqlalchemy.URL, schema: str) -> Iterator[sqlalchemy.URL]:
    """Yield the URL of a fresh database holding ``schema``, dropped afterwards."""
    name = f"sqlcdx_{secrets.token_hex(6)}"
    with psycopg.connect(conninfo(server), autocommit=True) as admin:
        admin.execute(f"CREATE DATABASE {name}")

        # A statement or lock that hangs fails instead, so that a proof cannot stall.
        for setting in ("statement_timeout = '5s'", "lock_timeout = '1s'"):
            admin.execute(f"ALTER DATABASE {name} SET {setting}")

        url = server.set(database=name)
        try:
            try:
                with psycopg.connect(conninfo(url)) as connection:
                    connection.execute(schema)  # Without parameters, psycopg runs it as a verbatim script
            except psycopg.Error as error:
                message = f"schema: {' '.join(str(error).split())[:100]}"
                raise SchemaRejectedError(message) from error
            yield url
        finally:
            admin.execute(f"DROP DATABASE {name} WITH (FORCE)")


def conninfo(url: sqlalchemy.URL) -> str:
    return url.set(drivername="postgresql").render_as_string(hide_password=False)


async def aw(value: Any) -> Any:
    """Await ``value`` when SQLAlchemy's async API returned it, so that one probe drives both APIs."""
    return await value if inspect.isawaitable(value) else value


def version(url: sqlalchemy.URL) -> str:
    engine = sqlalchemy.create_engine(url)
    try:
        with engine.connect() as connection:
            found = connection.exec_driver_sql("SHOW server_version").scalar_one()
            return str(found).split()[0]
    finally:
        engine.dispose()


def wait_for(url: sqlalchemy.URL) -> None:
    # The image's init phase only listens on a Unix socket, so the first TCP connection that succeeds is the real
    # server.
    engine = sqlalchemy.create_engine(url)
    deadline = time.monotonic() + READY_SECONDS
    try:
        while not _connects(engine):
            if time.monotonic() > deadline:
                message = f"PostgreSQL at {url} not ready in {READY_SECONDS}s"
                raise TimeoutError(message)
            time.sleep(0.2)
    finally:
        engine.dispose()


def _connects(engine: sqlalchemy.Engine) -> bool:
    try:
        with engine.connect():
            return True
    except sqlalchemy.exc.OperationalError:
        return False
