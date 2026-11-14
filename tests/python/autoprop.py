"""The runtime sweep: run what the generator emits against PostgreSQL.

For every PostgreSQL package generated on SQLAlchemy or psycopg from a golden
case or a corpus request, the request's schema is applied to a fresh database,
the package is imported, and Hypothesis draws examples for its functions (its
coroutines, in an async package: on asyncpg, or psycopg's own on psycopg).
A psycopg package's functions take the psycopg connection under SQLAlchemy's,
in its transaction. Each example, in a transaction it rolls back:

1. seeds every table in foreign-key order, taking referenced values from rows
   seeded before, and reads each row back through its model, which must
   inhabit its annotations exactly (``deep.check``);
2. calls every generated function once, writers first and deletes last, with
   arguments drawn for its annotations and sqlc's parameter types, often
   reusing values seen earlier so that lookups find rows. Every argument and
   result is checked against its annotation (``deep.checked``).

A call returned ``rows``, came back ``empty`` (for a ``:one``, returned
``None``, or raised ``NoResultFound`` or the package's ``NoRowError``), ``ran`` (``:exec`` and
``:copyfrom`` return nothing), or was ``inconclusive``: the database refused
the drawn data (an integrity, data or cardinality error, a timeout), or it
rejects the SQL itself. Anything else fails the package: a value that does not
match its annotation, a crash, a parameter the driver cannot bind, a module
whose functions do not match its queries.

A function is **exercised** once a call of it checked everything it can
return: one that returned ``rows``, or, for a command that returns no rows
(``:exec``, ``:execrows``, ``:execresult``, ``:copyfrom``), any call that was
not inconclusive. An empty result checked no value against its annotation. A
function never exercised is a warning; the share exercised across all
packages must stay at least ``FLOOR``. Which functions are exercised varies a
little between runs (one in about a thousand flipped between two runs), so a
committed list of them would fail at random.

Each call of a function for ``:one``, ``:many`` or ``:execrows`` is also
compared with sqlc's own text run directly, with the same arguments, through
psycopg's ``RawCursor``, which takes ``$N`` as written: the rows (as a
multiset) or the row count must be the same. That holds the rewriting and the
reading of every result to a reference that shares none of the generator's
code. The direct run happens twice, each rolled back; if the two differ (a
sequence, ``random()``), nothing is compared. A coroutine runs the same SQL as
its function.

This module is the sweep as a whole: which packages it runs, a process each,
and what their reports add up to. One package's sweep is ``autoprop_package``;
a call of one function, ``autoprop_calls``; seeding a table,
``autoprop_seeds``; the values an example has seen, ``autoprop_pool``; and
which errors are a refusal, ``autoprop_refusals``.
"""

from __future__ import annotations

import json
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

# The share of generated functions that must be exercised (see above): 92%
# when an empty result stopped counting, less a margin for the calls a
# statement timeout makes inconclusive on a slower machine. Raise it as the
# sweep improves, never lower it to pass.
FLOOR = 0.91
# Failures the sweep must keep finding, and why they are not the generator's.
KNOWN = Path(__file__).with_name("known-failures.tsv")


def targets(golden: Path, generated: Path) -> list[Target]:
    """Every PostgreSQL package generated on SQLAlchemy or psycopg to sweep,
    sync or async: the golden cases laid out under ``golden`` (``<case>/``),
    and the corpus outputs ``cargo test`` exported to ``generated``, as
    ``packages.tsv`` lists them.
    """
    found = []
    for case in golden_cases():
        if not is_swept(case.request, case.options):
            continue
        if not (golden / case.name).is_dir():
            # A case that was not exported would otherwise go unswept.
            message = f"{golden / case.name}: the golden case was not exported"
            raise FileNotFoundError(message)
        asynchronous, raw = bool(case.options["async"]), case.backend == "psycopg"
        found.append(
            Target(case.name, case.request, case.schema_sql, (), asynchronous, raw)
        )
    profiles = json.loads((ROOT / "tests/sqlc/endtoend/profiles.json").read_text())
    # Each package to sweep, a request it came from, and its options.
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
    """Whether the sweep runs what ``request`` generates with ``options``:
    PostgreSQL, on SQLAlchemy or psycopg.
    """
    engine = json.loads(request.read_text())["settings"]["engine"]
    return engine == "postgresql" and options["backend"] in {"sqlalchemy", "psycopg"}


def corpus_packages(generated: Path) -> dict[tuple[str, str], str]:
    """The package ``cargo test`` exported each generated corpus request and
    profile as (``packages.tsv``): one per distinct output, so several share
    one. Raise if a package is missing.
    """
    rows = [
        line.split("\t")
        for line in (generated / "packages.tsv").read_text().splitlines()[1:]
    ]
    if missing := sorted({p for *_, p in rows if not (generated / p).is_dir()}):
        message = (
            f"packages.tsv names packages cargo test did not export: {missing[:3]}"
        )
        raise FileNotFoundError(message)
    return {(request, profile): package for request, profile, package in rows}


def known_failures() -> list[tuple[Path, str]]:
    """``KNOWN``'s entries: a corpus case, and what its failure says."""
    rows = [line.split("\t") for line in KNOWN.read_text().splitlines()]
    corpus = ROOT / "tests/sqlc/endtoend"
    return [(corpus / row[0], row[1]) for row in rows if row[0] and row[0][0] != "#"]


def schema_of(request: Path) -> Path:
    """The schema script ``capture.py`` stored beside a request."""
    package = re.fullmatch(r"request(?:\.managed-db)?(\.\d+)?\.json", request.name)
    if package is None:
        message = f"not a captured request: {request}"
        raise ValueError(message)
    return request.with_name(f"schema{package[1] or ''}.sql")


def sweep(
    server: sqlalchemy.URL, found: list[Target], roots: list[Path]
) -> list[Report]:
    """Sweep every target, a process per CPU, each in a database of its own."""
    with psycopg.connect(conninfo(server)) as conn:
        if conn.info.server_version < 160000:  # noqa: PLR2004
            # `rejected` plans with EXPLAIN (GENERIC_PLAN), which is new in 16.
            message = "the runtime sweep needs PostgreSQL 16 or newer"
            raise RuntimeError(message)
    workers = min(os.cpu_count() or 1, 8)
    with ProcessPoolExecutor(
        workers, initializer=_importable, initargs=(roots,)
    ) as pool:
        return list(pool.map(partial(run, server), found))


def summarize(reports: list[Report]) -> tuple[bool, list[str]]:
    """Print what the sweep found: each package that failed or could not run,
    each known failure that no longer happens, each function never exercised.
    Return whether that fails the sweep, and its totals.

    A package that could not run fails it too, and so does a sweep that called
    nothing: a schema PostgreSQL rejects proves nothing about its package, and
    a new image or extension could make it reject all of them.
    """
    for r in reports:
        if r.failure:
            print(f"  FAILED {r.target.package}\n    {r.failure}")
    happened = {k for r in reports for k in r.known}
    stale = [(case, text) for case, text in known_failures() if text not in happened]
    for case, text in stale:
        where = case.relative_to(ROOT)
        print(f"  known failure no longer happens; remove it: {where}: {text}")
    functions = sum(len(r.calls) for r in reports)
    exercised = sum(len(r.exercised) for r in reports)
    share = exercised / functions if functions else 0.0
    failed = sum(1 for r in reports if r.failure)
    same = sum(r.same for r in reports)
    totals = [
        f"{len(reports)} APIs: {failed} failed, {len(stale)} known failures stale",
        f"{exercised}/{functions} functions exercised ({share:.0%}, floor {FLOOR:.0%})",
        f"{same} calls returned what sqlc's SQL run directly did",
    ]
    return bool(failed or stale or not same) or share < FLOOR, totals


def _importable(roots: list[Path]) -> None:
    sys.path[:0] = [str(root) for root in roots]
    sys.dont_write_bytecode = True
