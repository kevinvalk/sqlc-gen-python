"""Throwaway PostgreSQL servers in Docker, and databases on them.

The runtime proof uses ``NEWEST``, or the server ``SQLC_TEST_DATABASE_URL``
names; the capture passes the image sqlc's test suite runs, and its own
variable. There is no silent skip: no Docker and no such variable ends the
program with an error.

``database`` makes a throwaway database holding a schema, for every layer of
the runtime proof.
"""

from __future__ import annotations

import inspect
import os
import secrets
import shutil
import subprocess
import sys
import time
from contextlib import contextmanager
from typing import TYPE_CHECKING, Any

import psycopg
import sqlalchemy

if TYPE_CHECKING:
    from collections.abc import Iterator

READY_SECONDS = 60
# The data memory each server process may use, in bytes.
BACKEND_MEMORY = 2**30
# The runtime proof's server: the newest PostgreSQL.
NEWEST = "postgres:18"


@contextmanager
def server(
    image: str = NEWEST, env: str = "SQLC_TEST_DATABASE_URL"
) -> Iterator[sqlalchemy.URL]:
    """Yield a server's psycopg URL, valid for as long as the context lasts:
    ``env``'s, if that variable is set, or else a container of ``image``.
    """
    if given := os.environ.get(env):
        yield sqlalchemy.make_url(given).set(drivername="postgresql+psycopg")
        return
    docker = shutil.which("docker")
    if (
        docker is None
        or subprocess.run([docker, "info"], capture_output=True, check=False).returncode
    ):
        sys.exit(f"a PostgreSQL server is needed: start docker, or set {env}")

    name = f"sqlcdx-{secrets.token_hex(4)}"
    subprocess.run(
        [
            docker,
            "run",
            "--detach",
            "--rm",
            "--publish=127.0.0.1::5432",
            f"--name={name}",
            "--env=POSTGRES_PASSWORD=postgres",
            # A statement that would need more memory fails with SQLSTATE
            # 53200 instead of the kernel killing the server: the runtime
            # sweep draws values such as an array index of 10**8.
            f"--ulimit=data={BACKEND_MEMORY}",
            image,
            "-c",
            "fsync=off",
            "-c",
            "synchronous_commit=off",
            "-c",
            "max_connections=200",
        ],
        check=True,
        capture_output=True,
    )
    try:
        published = subprocess.run(
            [docker, "port", name, "5432/tcp"],
            check=True,
            capture_output=True,
            text=True,
        ).stdout
        port = published.splitlines()[0].rsplit(":", 1)[1]
        url = sqlalchemy.make_url(
            f"postgresql+psycopg://postgres:postgres@127.0.0.1:{port}/postgres"
        )
        wait_for(url)
        yield url
    finally:
        subprocess.run(
            [docker, "rm", "--force", "--volumes", name],
            capture_output=True,
            check=False,
        )


class SchemaRejectedError(Exception):
    """PostgreSQL does not accept a schema sqlc did."""


@contextmanager
def database(server: sqlalchemy.URL, schema: str) -> Iterator[sqlalchemy.URL]:
    """Yield the URL of a fresh database holding ``schema``, dropped afterwards.

    A statement or lock that hangs fails instead, so a proof cannot stall.
    """
    name = f"sqlcdx_{secrets.token_hex(6)}"
    with psycopg.connect(conninfo(server), autocommit=True) as admin:
        admin.execute(f"CREATE DATABASE {name}")
        for setting in ("statement_timeout = '5s'", "lock_timeout = '1s'"):
            admin.execute(f"ALTER DATABASE {name} SET {setting}")
        url = server.set(database=name)
        try:
            try:
                with psycopg.connect(conninfo(url)) as conn:
                    conn.execute(schema)  # without parameters: a verbatim script
            except psycopg.Error as error:
                message = f"schema: {' '.join(str(error).split())[:100]}"
                raise SchemaRejectedError(message) from error
            yield url
        finally:
            admin.execute(f"DROP DATABASE {name} WITH (FORCE)")


def conninfo(url: sqlalchemy.URL) -> str:
    """The libpq URL of a SQLAlchemy URL, for psycopg itself."""
    return url.set(drivername="postgresql").render_as_string(hide_password=False)


async def aw(value: Any) -> Any:  # noqa: ANN401
    """Await ``value`` if SQLAlchemy's async API returned it; the sync API's as is."""
    return await value if inspect.isawaitable(value) else value


def version(url: sqlalchemy.URL) -> str:
    """Return the server's version, such as ``16.15``."""
    engine = sqlalchemy.create_engine(url)
    try:
        with engine.connect() as conn:
            found = conn.exec_driver_sql("SHOW server_version").scalar_one()
            return str(found).split()[0]
    finally:
        engine.dispose()


def wait_for(url: sqlalchemy.URL) -> None:
    """Block until ``url`` accepts connections, for at most ``READY_SECONDS``.

    The image's init phase listens on a Unix socket only, so the first TCP
    connection that succeeds is the real server.
    """
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
