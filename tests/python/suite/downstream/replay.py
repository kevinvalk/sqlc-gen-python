"""Run the pinned downstream SQL through the WASM plugin and check its Python on a real database."""

from __future__ import annotations

import argparse
import asyncio
import csv
import fnmatch
import hashlib
import importlib
import inspect
import json
import os
import posixpath
import re
import shutil
import sqlite3
import subprocess
import sys
import tempfile
import tomllib
import types
from contextlib import AsyncExitStack, asynccontextmanager, contextmanager, nullcontext
from pathlib import Path
from typing import TYPE_CHECKING, Any

import aiosqlite
import mysql
import postgres
import psycopg
import sqlalchemy
import yaml
from sqlalchemy.ext.asyncio import create_async_engine
from suite.deep import check
from suite.downstream import scenarios

ROOT = Path(__file__).resolve().parents[4]

if TYPE_CHECKING:
    from collections.abc import AsyncIterator, Callable, Iterator

    from sqlalchemy.engine import URL, Engine

WORK = ROOT / "target/downstream"
LEDGER = ROOT / "docs/audits/external-sqlc-projects.tsv"
DRIVER = {"sqlite": "sqlite3", "postgresql": "psycopg", "mysql": "pymysql"}


def git(repository: Path, *args: str) -> bytes:
    return subprocess.check_output(["git", "-C", repository, *args])


def checkout(row: dict[str, str], sources: Path | None) -> Path:
    name, pin = row["repository"], row["revision"]
    if sources:
        repository = sources / name
        git(repository, "cat-file", "-e", pin + "^{commit}")
        return repository
    repository = WORK / "sources" / name / pin
    if not repository.exists():
        repository.parent.mkdir(parents=True, exist_ok=True)
        # Publish only a complete pin so MySQL and MariaDB can start their first runs together.
        with tempfile.TemporaryDirectory(dir=repository.parent) as temp:
            stage = Path(temp) / "source"
            stage.mkdir()
            git(stage, "init", "--quiet", "--bare")
            git(stage, "remote", "add", "origin", f"https://github.com/{name}.git")
            git(stage, "config", "remote.origin.promisor", "true")
            git(stage, "config", "remote.origin.partialclonefilter", "blob:none")
            git(stage, "fetch", "--quiet", "--depth=1", "--filter=blob:none", "origin", pin)
            try:
                stage.rename(repository)
            except OSError:
                if not repository.is_dir():
                    raise
    return repository


def generate(row: dict[str, str], repo: Path, work: Path, sqlc: Path, wasm: Path) -> tuple[Path, str]:
    pin = row["revision"]
    path = Path(row["configuration"])
    config = yaml.safe_load(git(repo, "show", f"{pin}:{path.as_posix()}"))
    original = (config.get("sql") or config["packages"])[int(row["entry"]) - 1]
    engine = row["engine"]
    assert original["engine"] == engine
    paths = [p.decode() for p in git(repo, "ls-tree", "-rz", "--name-only", pin).split(b"\0") if p.endswith(b".sql")]
    entry: dict[str, object] = {"engine": engine}
    schema = []
    for field in ("schema", "queries"):
        inputs = original[field]
        files = []
        for ref in [inputs] if isinstance(inputs, str) else inputs:
            pattern = posixpath.normpath(posixpath.join(path.parent.as_posix(), ref))

            matches = sorted(p for p in paths if fnmatch.fnmatchcase(p, pattern) or p.startswith(pattern + "/"))
            if not matches:
                msg = f"No pinned SQL matches {pattern}"
                raise ValueError(msg)
            files.extend(matches)
        files = list(dict.fromkeys(files))
        entry[field] = files
        for filename in files:
            dest = work / filename
            dest.parent.mkdir(parents=True, exist_ok=True)
            data = git(repo, "show", f"{pin}:{filename}")
            if row["scenario"] == "rankings" and filename.endswith("/analytics.sql"):
                before = b"    tasks_created,\n    rn\nFROM ("
                assert data.count(before) == 1
                data = data.replace(
                    before,
                    b"    tasks_created,\n    CAST(rn AS UNSIGNED) AS rn\nFROM (",
                )
            dest.write_bytes(data)
            if field != "schema" or filename.endswith(".down.sql"):
                continue
            ddl = data.decode()
            if re.search(r"(?m)^-- name:", ddl):
                continue
            ddl = re.split(r"(?im)^\s*--\s*(?:\+goose\s+Down|migrate:down|\+migrate\s+Down)\b", ddl)[0]
            # Each profile already owns a database. Deployment-level USE must never redirect it.
            ddl = re.sub(r"(?im)^\s*(?:CREATE DATABASE|USE)\s+[^;]+;", "", ddl)
            if row["scenario"] == "blog":
                # The published dump restores session variables whose values it never saves.
                ddl = re.sub(r"(?im)^/\*!\d+ SET \w+=@OLD_\w+ \*/;\s*$", "", ddl)
            schema.append(ddl)
    profiles = work / "output"
    entry["codegen"] = [
        {
            "plugin": "python",
            "out": f"output/{backend}_{mode}",
            "options": {"backend": backend, "async": mode == "async"},
        }
        for backend in [DRIVER[engine], "sqlalchemy"]
        for mode in ["sync", "async"]
    ]
    plugin = {
        "name": "python",
        "wasm": {
            "url": wasm.as_uri(),
            "sha256": hashlib.sha256(wasm.read_bytes()).hexdigest(),
        },
    }
    (work / "sqlc.json").write_text(json.dumps({"version": "2", "plugins": [plugin], "sql": [entry]}))
    subprocess.run(
        [sqlc, "generate", "-f", "sqlc.json"],
        cwd=work,
        check=True,
        env={**os.environ, "SQLCCACHE": str(WORK / "sqlc-cache")},
    )
    return profiles, "\n".join(schema)


def functions(package: Path) -> dict[str, Callable[..., Any]]:
    # Two projects may both generate query.py and models.py in the same process.
    name = "downstream_" + hashlib.sha256(str(package).encode()).hexdigest()[:16]
    namespace = types.ModuleType(name)
    namespace.__path__ = [str(package)]
    sys.modules[name] = namespace
    result: dict[str, Callable[..., Any]] = {}
    for p in package.glob("*.py"):
        if p.name == "models.py":
            continue
        module = importlib.import_module(f"{name}.{p.stem}")
        for fn in vars(module).values():
            if inspect.isfunction(fn) and fn.__module__ == module.__name__ and not fn.__name__.startswith("_"):
                if fn.__name__ in result:
                    msg = f"Ambiguous function {fn.__name__}"
                    raise ValueError(msg)
                result[fn.__name__] = fn
    return result


class Queries:
    def __init__(self, connection: Any, reference: Engine, package: Path) -> None:
        self.connection = connection
        self.reference = reference
        self.functions = functions(package)
        self.calls: list[str] = []
        self.in_transaction = False

    def sql(self, statement: str, params: tuple[object, ...] = ()) -> list[tuple[Any, ...]]:
        with self.reference.begin() as conn:
            result = conn.exec_driver_sql(statement, params)
            return [tuple(row) for row in result] if result.returns_rows else []

    @asynccontextmanager
    async def rollback(self) -> AsyncIterator[None]:
        self.in_transaction = True
        try:
            yield
        finally:
            await postgres.aw(self.connection.rollback())
            self.in_transaction = False

    async def call(self, name: str, /, **arguments: Any) -> Any:
        fn = self.functions[name]
        for field, value in arguments.items():
            check(value, fn.__annotations__[field], f"{name}.{field}")
        value = await postgres.aw(fn(self.connection, **arguments))
        hint = fn.__annotations__["return"]
        check(value, type(None) if hint is None else hint, name)
        if hasattr(value, "rowcount") and hasattr(value, "close"):
            assert value.rowcount >= 0
            await postgres.aw(value.close())
        if stream := self.functions.get("stream_" + name):
            async with AsyncExitStack() as stack:
                manager = stream(self.connection, **arguments)
                if hasattr(manager, "__aenter__"):
                    items = await stack.enter_async_context(manager)
                    streamed = [item async for item in items]
                else:
                    streamed = list(stack.enter_context(manager))
                check(streamed, hint, "stream_" + name)
                assert streamed == value, name
            self.calls.append("stream_" + name)
        if not self.in_transaction:
            await postgres.aw(self.connection.commit())
        self.calls.append(name)
        return value


@contextmanager
def sqlite_database(schema: str) -> Iterator[URL]:
    with tempfile.TemporaryDirectory() as temp:
        path = Path(temp) / "audit.sqlite"
        connection = sqlite3.connect(path)
        try:
            connection.executescript(schema)
        finally:
            connection.close()
        yield sqlalchemy.URL.create("sqlite", database=str(path))


async def exercise(row: dict[str, str], url: URL, profile: Path) -> list[str]:
    backend, mode = profile.name.split("_")
    engine = row["engine"]
    sync = {
        "mysql": "mysql+pymysql",
        "sqlite": "sqlite",
        "postgresql": "postgresql+psycopg",
    }
    asynchronous = {
        "mysql": "mysql+asyncmy",
        "sqlite": "sqlite+aiosqlite",
        "postgresql": "postgresql+asyncpg",
    }
    async with AsyncExitStack() as stack:
        reference = sqlalchemy.create_engine(url.set(drivername=sync[engine]))
        stack.callback(reference.dispose)
        conn: Any
        if backend == "sqlalchemy":
            if mode == "async":
                db = create_async_engine(url.set(drivername=asynchronous[engine]))
                stack.push_async_callback(db.dispose)
                conn = await stack.enter_async_context(db.connect())
            else:
                conn = stack.enter_context(reference.connect())
        elif backend == "sqlite3":
            assert url.database is not None
            if mode == "async":
                conn = await stack.enter_async_context(aiosqlite.connect(url.database))
            else:
                conn = sqlite3.connect(url.database)
                stack.callback(conn.close)
        elif backend == "pymysql":
            conn = await mysql.connect_async(url) if mode == "async" else mysql.connect(url)
            stack.callback(conn.close)
        elif mode == "async":
            conn = await stack.enter_async_context(await psycopg.AsyncConnection.connect(postgres.conninfo(url)))
        else:
            conn = stack.enter_context(psycopg.connect(postgres.conninfo(url)))
        queries = Queries(conn, reference, profile)
        await getattr(scenarios, row["scenario"])(queries)
        return queries.calls


def main() -> None:
    pinned = tomllib.loads((ROOT / ".prototools").read_text())["sqlc"]
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--server", choices=("mysql", "mariadb", "sqlite", "postgresql"), required=True)
    parser.add_argument("--project", help="limit the run to one owner/repository")
    parser.add_argument(
        "--sources",
        type=Path,
        help="local Git checkouts in OWNER/REPOSITORY directories",
    )
    captured = ROOT / f"target/capture/v{pinned}/bin/sqlc"
    parser.add_argument(
        "--sqlc", type=Path, default=captured if captured.is_file() else Path(shutil.which("sqlc") or captured)
    )
    parser.add_argument(
        "--wasm",
        type=Path,
        default=ROOT / "target/wasm32-wasip1/release/sqlc-gen-python.wasm",
    )
    args = parser.parse_args()
    if not __debug__:
        parser.error("run Python with assertions enabled")
    if not args.wasm.is_file() or not args.sqlc.is_file():
        parser.error("build the WASM with cargo wasm and supply a sqlc executable with --sqlc")
    engine = "mysql" if args.server == "mariadb" else args.server
    with LEDGER.open() as f:
        rows = [
            r
            for r in csv.DictReader(f, delimiter="\t")
            if r["engine"] == engine and r["scenario"] and (not args.project or r["repository"] == args.project)
        ]
    if not rows:
        parser.error("no recorded workload matches that project and server")
    if args.server == "mariadb":
        mysql.IMAGE = "mariadb:11.4"
    runtime = mysql if engine == "mysql" else postgres
    WORK.mkdir(parents=True, exist_ok=True)
    with nullcontext(None) if engine == "sqlite" else runtime.server() as server:
        version = sqlite3.sqlite_version if engine == "sqlite" else runtime.version(server)
        if engine == "mysql" and (("mariadb" in version.lower()) != (args.server == "mariadb")):
            parser.error(f"expected {args.server}, connected to {version}")
        print(f"{args.server} {version}", flush=True)
        for row in rows:
            repo = checkout(row, args.sources)
            with tempfile.TemporaryDirectory(dir=WORK) as temp:
                profiles, ddl = generate(row, repo, Path(temp), args.sqlc.resolve(), args.wasm.resolve())
                for profile in sorted(profiles.iterdir()):
                    with sqlite_database(ddl) if engine == "sqlite" else runtime.database(server, ddl) as url:
                        calls = asyncio.run(exercise(row, url, profile))
                    print(
                        row["repository"],
                        row["scenario"],
                        profile.name,
                        len(calls),
                        "calls passed",
                        flush=True,
                    )


if __name__ == "__main__":
    main()
