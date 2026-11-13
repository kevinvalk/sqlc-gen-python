"""A throwaway MySQL server in Docker, and databases on it.

The runtime proof of the MySQL golden cases uses ``IMAGE``, or the server
``SQLC_TEST_MYSQL_URL`` names (a SQLAlchemy URL such as
``mysql+pymysql://root:pw@127.0.0.1:3306/``). There is no silent skip: no
Docker and no such variable ends the program with an error.
"""

from __future__ import annotations

import os
import secrets
import shutil
import subprocess
import sys
import time
from contextlib import contextmanager
from typing import TYPE_CHECKING

import pymysql
import sqlalchemy

if TYPE_CHECKING:
    from collections.abc import Iterator

READY_SECONDS = 120
# The long-term support release.
IMAGE = "mysql:8.4"
ENV = "SQLC_TEST_MYSQL_URL"
# Connections in a row that make the server ready (see `wait_for`).
READY_STREAK = 2
PASSWORD = "sqlc"  # noqa: S105 -- a throwaway container's


@contextmanager
def server() -> Iterator[sqlalchemy.URL]:
    """Yield a server's ``mysql+pymysql`` URL, without a database, valid for as
    long as the context lasts: ``ENV``'s, if set, or else a container of
    ``IMAGE``, removed with its volumes afterwards.
    """
    if given := os.environ.get(ENV):
        yield sqlalchemy.make_url(given).set(drivername="mysql+pymysql", database=None)
        return
    docker = shutil.which("docker")
    if (
        docker is None
        or subprocess.run([docker, "info"], capture_output=True, check=False).returncode
    ):
        sys.exit(f"a MySQL server is needed: start docker, or set {ENV}")

    name = f"sqlcdx-mysql-{secrets.token_hex(4)}"
    subprocess.run(
        [
            docker,
            "run",
            "--detach",
            "--rm",
            "--publish=127.0.0.1::3306",
            f"--name={name}",
            f"--env=MYSQL_ROOT_PASSWORD={PASSWORD}",
            IMAGE,
            "--skip-log-bin",
            "--innodb-flush-log-at-trx-commit=0",
        ],
        check=True,
        capture_output=True,
    )
    try:
        published = subprocess.run(
            [docker, "port", name, "3306/tcp"],
            check=True,
            capture_output=True,
            text=True,
        ).stdout
        port = int(published.splitlines()[0].rsplit(":", 1)[1])
        url = sqlalchemy.URL.create(
            "mysql+pymysql",
            username="root",
            password=PASSWORD,
            host="127.0.0.1",
            port=port,
        )
        wait_for(url)
        yield url
    finally:
        subprocess.run(
            [docker, "rm", "--force", "--volumes", name],
            capture_output=True,
            check=False,
        )


def connect(url: sqlalchemy.URL) -> pymysql.Connection[pymysql.cursors.Cursor]:
    """A PyMySQL connection to ``url``."""
    return pymysql.connect(
        host=url.host or "127.0.0.1",
        port=url.port or 3306,
        user=url.username or "root",
        password=url.password or "",
        database=url.database,
    )


@contextmanager
def database(server: sqlalchemy.URL, schema: str) -> Iterator[sqlalchemy.URL]:
    """Yield the URL of a fresh database holding ``schema``, dropped afterwards."""
    name = f"sqlcdx_{secrets.token_hex(6)}"
    admin = connect(server)
    try:
        with admin.cursor() as cur:
            cur.execute(f"CREATE DATABASE {name}")
        url = server.set(database=name)
        try:
            conn = connect(url)
            try:
                with conn.cursor() as cur:
                    for statement in statements(schema):
                        cur.execute(statement)
                conn.commit()
            finally:
                conn.close()
            yield url
        finally:
            with admin.cursor() as cur:
                cur.execute(f"DROP DATABASE {name}")
    finally:
        admin.close()


def statements(script: str) -> list[str]:
    """The statements of a schema script, whose comments are whole `--`
    lines, and which holds no other `;` than between statements.
    """
    code = "\n".join(
        line for line in script.splitlines() if not line.lstrip().startswith("--")
    )
    return [s for s in code.split(";") if s.strip()]


def wait_for(url: sqlalchemy.URL) -> None:
    """Block until ``url`` accepts connections, for at most ``READY_SECONDS``.

    The image's init phase runs a temporary server without networking, then
    restarts it, so a connection must succeed twice in a row, a second apart.
    """
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
