"""A throwaway MySQL server in Docker, and databases on it.

The runtime proof of the MySQL golden cases uses ``IMAGE`` or the server ``SQLC_TEST_MYSQL_URL`` names (a SQLAlchemy URL
such as ``mysql+pymysql://root:pw@127.0.0.1:3306/``). We never skip silently: without Docker and without that variable,
the program ends with an error.
"""

from __future__ import annotations

import os
import secrets
import time
from contextlib import contextmanager
from typing import TYPE_CHECKING, Any

import asyncmy
import pymysql
import sqlalchemy

from containers import container
from examples import statements

if TYPE_CHECKING:
    from collections.abc import Awaitable, Iterator

READY_SECONDS = 120
IMAGE = "mysql:8.4"  # The long-term support release
ENV = "SQLC_TEST_MYSQL_URL"
READY_STREAK = 2  # Connections in a row that make the server ready (see `wait_for`)
PASSWORD = "sqlc"  # noqa: S105 -- a throwaway container's


@contextmanager
def server() -> Iterator[sqlalchemy.URL]:
    """Yield a server's ``mysql+pymysql`` URL without a database, valid for as long as the context lasts: the one
    ``ENV`` names when it is set, else a container of ``IMAGE`` that we remove with its volumes afterwards.
    """
    if given := os.environ.get(ENV):
        yield sqlalchemy.make_url(given).set(drivername="mysql+pymysql", database=None)
        return

    arguments = [
        f"--env=MYSQL_ROOT_PASSWORD={PASSWORD}",
        IMAGE,
        *["--skip-log-bin", "--innodb-flush-log-at-trx-commit=0"],
    ]
    with container("MySQL", ENV, 3306, arguments) as port:
        url = sqlalchemy.URL.create(
            "mysql+pymysql",
            username="root",
            password=PASSWORD,
            host="127.0.0.1",
            port=port,
        )
        wait_for(url)
        yield url


def connect(url: sqlalchemy.URL) -> pymysql.Connection[pymysql.cursors.Cursor]:
    return pymysql.connect(
        host=url.host or "127.0.0.1",
        port=url.port or 3306,
        user=url.username or "root",
        password=url.password or "",
        database=url.database,
    )


def connect_async(url: sqlalchemy.URL, **options: Any) -> Awaitable[asyncmy.Connection]:
    return asyncmy.connect(
        host=url.host,
        port=url.port,
        user=url.username,
        password=url.password,
        database=url.database,
        **options,
    )


@contextmanager
def database(server: sqlalchemy.URL, schema: str) -> Iterator[sqlalchemy.URL]:
    """Yield the URL of a fresh database holding ``schema``, dropped afterwards."""
    name = f"sqlcdx_{secrets.token_hex(6)}"
    admin = connect(server)
    try:
        with admin.cursor() as cursor:
            cursor.execute(f"CREATE DATABASE {name}")

        url = server.set(database=name)
        try:
            connection = connect(url)
            try:
                with connection.cursor() as cursor:
                    for statement in statements(schema):
                        cursor.execute(statement)
                connection.commit()
            finally:
                connection.close()
            yield url
        finally:
            with admin.cursor() as cursor:
                cursor.execute(f"DROP DATABASE {name}")
    finally:
        admin.close()


def version(url: sqlalchemy.URL) -> str:
    connection = connect(url)
    try:
        with connection.cursor() as cursor:
            cursor.execute("SELECT VERSION()")
            return str(cursor.fetchall()[0][0])
    finally:
        connection.close()


def wait_for(url: sqlalchemy.URL) -> None:
    # The image's init phase runs a temporary server without networking and then restarts it, so a connection must
    # succeed twice in a row, a second apart.
    deadline = time.monotonic() + READY_SECONDS
    streak = 0
    while streak < READY_STREAK:
        if time.monotonic() > deadline:
            message = f"MySQL at {url} not ready in {READY_SECONDS}s"
            raise TimeoutError(message)
        try:
            connect(url).close()
            streak += 1
        except pymysql.err.OperationalError:
            streak = 0
        time.sleep(1)
