"""The runtime sweep: run what the generator emits for PostgreSQL against PostgreSQL.

``docs/development.md`` describes it under Proof. Each example seeds every table in foreign-key order and
then calls every generated function once (writers first, deletes last), in a transaction it rolls back, checking every
argument and result against its annotation (``deep.check``). A call returned ``rows``, came back ``empty`` (``None``,
``NoResultFound`` or the models' ``NoRowError`` for a ``:one``), ``ran`` (``:exec`` and ``:copyfrom``), or was
``inconclusive`` because the database refused the drawn data or the SQL itself. Anything else fails the package.

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

import json
import logging
import os
import re
import sys
from concurrent.futures import ProcessPoolExecutor
from functools import partial
from pathlib import Path
from typing import TYPE_CHECKING

import psycopg

from autoprop_package import Target, run
from examples import ROOT, golden_cases
from postgres import conninfo

if TYPE_CHECKING:
    import sqlalchemy

    from autoprop_calls import Json
    from autoprop_package import Report

# IMPORTANT: The share of generated functions that must be exercised. We measured 92% when an empty result stopped
# counting, less a margin for the calls a statement timeout makes inconclusive on a slower machine. Raise it as the
# sweep improves, NEVER lower it to pass! Which functions are exercised varies a little between runs (one in about a
# thousand flipped between two runs), so a committed list of them would fail at random.
FLOOR = 0.91

KNOWN = Path(__file__).with_name("known-failures.tsv")


def targets(golden: Path, generated: Path) -> list[Target]:
    """Every package to sweep: the golden cases laid out under ``golden`` (``<case>/``), and the corpus outputs
    ``cargo test`` exported to ``generated``, as ``packages.tsv`` lists them.
    """
    found = []
    for case in golden_cases():
        if not is_swept(case.request, case.options):
            continue

        # A case that was not exported would otherwise go unswept.
        if not (golden / case.name).is_dir():
            message = f"{golden / case.name}: the golden case was not exported"
            raise FileNotFoundError(message)
        asynchronous, raw = bool(case.options["async"]), case.backend == "psycopg"
        found.append(Target(case.name, case.request, case.schema_sql, (), asynchronous, raw))

    profiles = json.loads((ROOT / "tests/sqlc/endtoend/profiles.json").read_text())
    # Several requests may share one package, so we keep the first request and its options per package.
    modes: dict[str, tuple[Path, Json]] = {}
    for (name, profile), package in corpus_packages(generated).items():
        path = ROOT / "tests/sqlc/endtoend" / name
        if is_swept(path, profiles[profile]):
            modes.setdefault(package, (path, profiles[profile]))

    failures = known_failures()
    for package, (path, options) in modes.items():
        known = tuple(text for case, text in failures if case in path.parents)
        asynchronous, raw = bool(options["async"]), options["backend"] == "psycopg"
        found.append(Target(package, path, schema_of(path), known, asynchronous, raw))
    return found


def is_swept(request: Path, options: Json) -> bool:
    engine = json.loads(request.read_text())["settings"]["engine"]
    return engine == "postgresql" and options["backend"] in {"sqlalchemy", "psycopg"}


def corpus_packages(generated: Path) -> dict[tuple[str, str], str]:
    """The package ``cargo test`` exported each corpus request and profile as: one per distinct output, so several
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
    corpus = ROOT / "tests/sqlc/endtoend"
    return [(corpus / row[0], row[1]) for row in rows if row[0] != "" and not row[0].startswith("#")]


def schema_of(request: Path) -> Path:
    """The schema script ``capture.py`` stored beside a request."""
    package = re.fullmatch(r"request(?:\.managed-db)?(\.\d+)?\.json", request.name)
    if package is None:
        message = f"not a captured request: {request}"
        raise ValueError(message)
    return request.with_name(f"schema{package[1] or ''}.sql")


def sweep(server: sqlalchemy.URL, found: list[Target], roots: list[Path]) -> list[Report]:
    """Sweep every target, a process per CPU, each in a database of its own."""
    # `rejected` plans with EXPLAIN (GENERIC_PLAN), which is new in PostgreSQL 16.
    with psycopg.connect(conninfo(server)) as connection:
        if connection.info.server_version < 160000:  # noqa: PLR2004
            message = "the runtime sweep needs PostgreSQL 16 or newer"
            raise RuntimeError(message)

    workers = min(os.cpu_count() or 1, 8)
    with ProcessPoolExecutor(workers, initializer=_worker, initargs=(roots,)) as pool:
        return list(pool.map(partial(run, server), found))


def summarize(reports: list[Report]) -> tuple[bool, list[str]]:
    """Print each package that failed or could not run and each known failure that no longer happens, and return
    whether that fails the sweep, with its totals.
    """
    for report in reports:
        if report.failure != "":
            print(f"  FAILED {report.target.package}\n    {report.failure}")

    happened = {known for report in reports for known in report.known}
    stale = [(case, text) for case, text in known_failures() if text not in happened]
    for case, text in stale:
        where = case.relative_to(ROOT)
        print(f"  known failure no longer happens; remove it: {where}: {text}")

    functions = sum(len(report.calls) for report in reports)
    exercised = sum(len(report.exercised) for report in reports)
    share = exercised / functions if functions > 0 else 0.0
    failed = sum(1 for report in reports if report.failure != "")
    same = sum(report.same for report in reports)
    totals = [
        f"{len(reports)} APIs: {failed} failed, {len(stale)} known failures stale",
        f"{exercised}/{functions} functions exercised ({share:.0%}, floor {FLOOR:.0%})",
        f"{same} calls returned what sqlc's SQL run directly did",
    ]
    # A package that could not run fails the sweep too, and so does a sweep that compared nothing: a schema PostgreSQL
    # rejects proves nothing about its package, and a new image or extension could make it reject all of them.
    return failed > 0 or len(stale) > 0 or same <= 0 or share < FLOOR, totals


def _worker(roots: list[Path]) -> None:
    sys.path[:0] = [str(root) for root in roots]
    sys.dont_write_bytecode = True

    # A call the database refuses inside a generated `connection.pipeline()` leaves the pipeline aborted, so psycopg's
    # sync on leaving it fails too, and psycopg logs that second error while it raises the first, which the sweep
    # handles. Any other psycopg warning still prints.
    logging.getLogger("psycopg").addFilter(lambda record: not record.msg.startswith("error ignored terminating"))
