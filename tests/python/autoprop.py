"""The runtime sweep: run what the generator emits for PostgreSQL against PostgreSQL.

Each example seeds every table in foreign-key order and then calls every generated function once (writers first, deletes
last), in a transaction it rolls back, checking every argument and result against its annotation (``deep.check``). A
call returned ``rows``, came back ``empty`` (``None``, ``NoResultFound`` or the models' ``NoRowError`` for a ``:one``),
``ran`` (``:exec`` and ``:copyfrom``), or was ``inconclusive`` because the database refused the drawn data or the SQL
itself. Anything else fails the package.

A function is "exercised" once a call of it checked everything it can return: rows, or for a command that returns none,
any call that was not inconclusive. Each ``:one``, ``:many`` and ``:execrows`` call is also compared with sqlc's own
text run directly through psycopg's ``RawCursor`` (which takes ``$N`` as written), so the rewriting and the reading are
held to a reference that shares none of the generator's code. We run that twice and compare nothing when the two runs
differ (a sequence, ``random()``).

This module is the sweep as a whole. One package's sweep is ``autoprop_package``, a call of one function
``autoprop_calls``, seeding a table ``autoprop_seeds``, the values an example has seen ``autoprop_pool``, and which
errors are a refusal ``autoprop_refusals``.
"""

from __future__ import annotations

import hashlib
import json
import logging
import multiprocessing
import os
import re
import sys
from collections import Counter
from concurrent.futures import ProcessPoolExecutor, as_completed
from pathlib import Path
from typing import TYPE_CHECKING

import psycopg

from autoprop_package import Report, Target, run
from examples import ROOT, golden_cases
from postgres import conninfo

if TYPE_CHECKING:
    import sqlalchemy

    from autoprop_calls import Json

HERE = Path(__file__).resolve().parent
KNOWN = HERE / "known-failures.tsv"
# IMPORTANT: Every function the sweep exercised. One it no longer exercises fails the sweep until its line is deleted
# by hand, so the sweep never checks less without someone saying why. A sweep is the same from run to run (see `sweep`),
# but for the calls a statement timeout cuts short.
EXERCISED = HERE / "exercised.tsv"
CACHE = ROOT / "target/sweep"  # A report per package, by `cache_key`


def targets(golden: Path, generated: Path) -> list[Target]:
    """Every package to sweep: the golden cases laid out under ``golden`` (``<case>/``), and the endtoend outputs
    ``cargo test`` exported to ``generated``, as ``packages.tsv`` lists them.
    """
    found = []
    for case in golden_cases():
        if case.is_models_only or not is_swept(case.request, case.options):
            continue

        # A case that was not exported would otherwise go unswept.
        if not (golden / case.name).is_dir():
            message = f"{golden / case.name}: the golden case was not exported"
            raise FileNotFoundError(message)
        asynchronous, raw = bool(case.options["async"]), case.backend == "psycopg"
        found.append(
            Target(
                case.name,
                case.models,
                case.request,
                case.schema_sql,
                (),
                asynchronous,
                raw,
                raise_on_none=bool(case.options.get("raise_on_none")),
            )
        )

    profiles = json.loads((ROOT / "tests/sqlc/endtoend/profiles.json").read_text())
    # Several requests may share one package, so we keep the first request and its options per package.
    modes: dict[str, tuple[Path, Json]] = {}
    for (name, profile), package in endtoend_packages(generated).items():
        path = ROOT / "tests/sqlc/endtoend" / name
        if is_swept(path, profiles[profile]):
            modes.setdefault(package, (path, profiles[profile]))

    failures = known_failures()
    for package, (path, options) in modes.items():
        known = tuple(text for case, text in failures if case in path.parents)
        asynchronous, raw = bool(options["async"]), options["backend"] == "psycopg"
        found.append(
            Target(
                package,
                f"{package}.models",
                path,
                schema_of(path),
                known,
                asynchronous,
                raw,
                raise_on_none=bool(options.get("raise_on_none")),
            )
        )
    return found


def is_swept(request: Path, options: Json) -> bool:
    # A type left `Any` or `object` checks nothing at runtime, so a profile that accepts them is the static gate's alone
    engine = json.loads(request.read_text())["settings"]["engine"]
    return engine == "postgresql" and options["backend"] in {"sqlalchemy", "psycopg"} and "unknown_type" not in options


def endtoend_packages(generated: Path) -> dict[tuple[str, str], str]:
    """The package ``cargo test`` exported each endtoend request and profile as: one per distinct output, so several
    share one.
    """
    rows = [line.split("\t") for line in (generated / "packages.tsv").read_text().splitlines()[1:]]
    missing = sorted({package for *_, package in rows if not (generated / package).is_dir()})
    if len(missing) > 0:
        message = f"packages.tsv names packages cargo test did not export: {missing[:3]}"
        raise FileNotFoundError(message)
    return {(request, profile): package for request, profile, package in rows}


def known_failures() -> list[tuple[Path, str]]:
    rows = [line.split("\t") for line in KNOWN.read_text().splitlines()]
    endtoend = ROOT / "tests/sqlc/endtoend"
    return [(endtoend / row[0], row[1]) for row in rows if row[0] != "" and not row[0].startswith("#")]


def schema_of(request: Path) -> Path:
    """The schema script ``capture.py`` stored beside a request."""
    package = re.fullmatch(r"request(?:\.managed-db)?(\.\d+)?\.json", request.name)
    if package is None:
        message = f"not a captured request: {request}"
        raise ValueError(message)
    return request.with_name(f"schema{package[1] or ''}.sql")


def sweep(server: sqlalchemy.URL, found: list[Target], roots: list[Path]) -> list[Report]:
    """Sweep every target whose report is not cached, a process per CPU, each in a database of its own."""
    # `rejected` plans with EXPLAIN (GENERIC_PLAN), which is new in PostgreSQL 16.
    with psycopg.connect(conninfo(server)) as connection:
        version = connection.info.server_version
    if version < 160000:  # noqa: PLR2004
        message = "the runtime sweep needs PostgreSQL 16 or newer"
        raise RuntimeError(message)

    # A package's sweep reads nothing but what its key hashes, so for an unchanged key it would only repeat its report.
    tools = hashlib.sha256(f"{version} {sys.version}".encode())
    for path in [HERE / "uv.lock", *sorted(HERE.glob("*.py")), *sorted(HERE.glob("suite/*.py"))]:
        tools.update(path.read_bytes())
    keys = {target: cache_key(target, roots, tools.digest()) for target in found}
    cached = {target: cached_report(target, CACHE / f"{key}.json") for target, key in keys.items()}
    fresh = [target for target, report in cached.items() if report is None]
    print(f"  {len(found) - len(fresh)} of {len(found)} packages unchanged since their last sweep", flush=True)

    # Hypothesis also draws the literals of every module its process imported, so a package swept after others in one
    # process draws differently. A process of its own keeps each package's sweep the same from run to run, forked from
    # a server that imported the libraries once.
    context = multiprocessing.get_context("forkserver")
    context.set_forkserver_preload(["autoprop_package"])
    workers = min(os.cpu_count() or 1, 8)
    swept: dict[Target, Report] = {}
    CACHE.mkdir(parents=True, exist_ok=True)
    with ProcessPoolExecutor(workers, context, initializer=_worker, initargs=(roots,), max_tasks_per_child=1) as pool:
        # We store each report as it comes, so that a sweep cut short keeps what it did. And only what passed, so that
        # a failure runs again, as a server that went away would fail a package too.
        for future in as_completed([pool.submit(run, server, target) for target in fresh]):
            report = future.result()
            swept[report.target] = report
            if report.failure == "":
                stored = {"calls": report.calls, "returning": sorted(report.returning), "known": report.known}
                (CACHE / f"{keys[report.target]}.json").write_text(json.dumps(stored | {"same": report.same}))
    return [cached[target] or swept[target] for target in found]


def cache_key(target: Target, roots: list[Path], tools: bytes) -> str:
    """Hash what ``target``'s sweep reads besides ``tools``: its request and schema, and every module of its package and
    of the package of its models, which ``models.module`` may name.
    """
    digest = hashlib.sha256(tools + repr(target).encode())
    packages = sorted({target.package, target.models.partition(".")[0]})
    modules = [path for name in packages for root in roots for path in sorted((root / name).glob("*.py"))]
    for path in [target.request, target.schema, *modules]:
        digest.update(path.name.encode())
        digest.update(path.read_bytes())
    return digest.hexdigest()


def cached_report(target: Target, path: Path) -> Report | None:
    if not path.is_file():
        return None
    stored = json.loads(path.read_text())
    return Report(
        target,
        calls={name: Counter(outcomes) for name, outcomes in stored["calls"].items()},
        returning=set(stored["returning"]),
        known=Counter(stored["known"]),
        same=stored["same"],
    )


def summarize(reports: list[Report]) -> tuple[bool, list[str], str]:
    """Print each package that failed or could not run, each known failure that no longer happens and each function no
    longer exercised, and return whether that fails the sweep, with its totals and the new ``EXERCISED``.
    """
    for report in reports:
        if report.failure != "":
            print(f"  FAILED {report.target.package}\n    {report.failure}")

    happened = {known for report in reports for known in report.known}
    stale = [(case, text) for case, text in known_failures() if text not in happened]
    for case, text in stale:
        where = case.relative_to(ROOT)
        print(f"  known failure no longer happens; remove it: {where}: {text}")

    exercised = sorted(f"{report.target.package}\t{name}" for report in reports for name in report.exercised)
    recorded = EXERCISED.read_text().splitlines()[1:] if EXERCISED.is_file() else []
    lost = sorted(set(recorded) - set(exercised))
    for line in lost:
        package, name = line.split("\t")
        print(f"  no longer exercised; delete it from {EXERCISED.name} once you know why: {package} {name}")

    functions = sum(len(report.calls) for report in reports)
    failed = sum(1 for report in reports if report.failure != "")
    same = sum(report.same for report in reports)
    totals = [
        f"{len(reports)} APIs: {failed} failed, {len(stale)} known failures stale",
        f"{len(exercised)}/{functions} functions exercised, {len(lost)} no longer",
        f"{same} calls returned what sqlc's SQL run directly did",
    ]
    record = "".join(f"{line}\n" for line in ["package\tfunction", *exercised])
    # A package that could not run fails the sweep too, and so does a sweep that compared nothing: a schema PostgreSQL
    # rejects proves nothing about its package, and a new image or extension could make it reject all of them.
    return failed > 0 or len(stale) > 0 or same <= 0 or len(lost) > 0, totals, record


def _worker(roots: list[Path]) -> None:
    sys.path[:0] = [str(root) for root in roots]
    sys.dont_write_bytecode = True

    # A call the database refuses inside a generated `connection.pipeline()` leaves the pipeline aborted, so psycopg's
    # sync on leaving it fails too, and psycopg logs that second error while it raises the first, which the sweep
    # handles. Any other psycopg warning still prints.
    logging.getLogger("psycopg").addFilter(lambda record: not record.msg.startswith("error ignored terminating"))
