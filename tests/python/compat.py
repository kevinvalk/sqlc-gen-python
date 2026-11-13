"""Which sqlc features the generated Python supports: ``docs/compat.md``.

Nothing in the table is written by hand. Every query of every golden fixture
and corpus request is an instance of the features ``FEATURES`` detects in it,
from what sqlc put in the request (its ``cmd``, ``embed_table``,
``is_sqlc_slice``, types and names), in the column of its engine and the
backend its options name (a backend that does not run the engine is no
column). Each instance gets the verdict of what happened to it:

- ``runtime``: the runtime sweep (``autoprop.py``) exercised its function,
  or its coroutine: a call checked everything it returns
  (rows, for a query that returns rows). The sweep runs PostgreSQL on
  SQLAlchemy only; SQLite and MySQL have runtime tests of their golden
  fixtures, not of the corpus, so their columns say no more than ``static``. A cell says so when at least one instance was, since how
  many are varies from run to run;
- ``static``: it generated, so it passes the static gate ``cargo xtask check``
  runs over every output, but no call of it completed;
- ``error``: generating failed with a message naming its query or a table it
  touches: the clean error of something unsupported;
- ``FAILING``: the sweep failed the package it is in.

An error that names neither (a table the query does not touch, say) says
nothing about the query's features, so it is no instance. A cell shows the
strongest verdict among the instances that generated, ``FAILING`` first, then
how many of all its instances are an ``error`` and the number of their
commonest message; ``-`` if the corpus has none. Only the error counts are
counts: how many calls of a function the sweep completes varies a little from
run to run, and the document must not. Every request for an engine without
columns fails with one error, which a line says.
"""

from __future__ import annotations

import json
import keyword
import re
from collections import Counter, defaultdict
from pathlib import Path
from typing import TYPE_CHECKING, Any

import autoprop
from examples import golden_cases

if TYPE_CHECKING:
    from collections.abc import Callable, Iterator

ROOT = Path(__file__).resolve().parents[2]
CORPUS = ROOT / "tests/sqlc/endtoend"
ENGINES = {"postgresql": "PostgreSQL", "sqlite": "SQLite", "mysql": "MySQL"}
BACKENDS = {
    "sqlalchemy": "SQLAlchemy",
    "psycopg": "psycopg",
    "sqlite3": "sqlite3",
    "pymysql": "PyMySQL",
}
# Each engine, and the backends that run it.
COLUMNS = (
    "PostgreSQL SQLAlchemy",
    "PostgreSQL psycopg",
    "SQLite SQLAlchemy",
    "SQLite sqlite3",
    "MySQL SQLAlchemy",
    "MySQL PyMySQL",
)
VERDICTS = ("FAILING", "runtime", "static", "error")
LEGEND = {
    "runtime": "at least one function using it was called against PostgreSQL by"
    " the runtime sweep, every argument and result checked against its annotation",
    "static": "generated, so held to the static gate by `cargo xtask check`, but"
    " no call of it was exercised: none returned rows to check, or none ran",
    "error": "unsupported: generation fails, with the numbered message below",
    "FAILING": "the runtime sweep fails a package using it",
    "-": "not in the corpus",
}

Json = dict[str, Any]
# A table cell: how many of its instances got each verdict, by message.
Cell = dict[str, Counter[str]]


class Query:
    """A query, its columns and parameters resolved against its catalog."""

    def __init__(self, query: Json, catalog: Json) -> None:
        self.query = query
        self.default: str = catalog["default_schema"]
        self.params: list[Json] = [p["column"] for p in query.get("params") or []]
        self.columns: list[Json] = [*query["columns"], *self.params]
        touched = [c["table"] for c in self.columns] + [query["insert_into_table"]]
        self.tables: list[Json] = [t for t in touched if t]
        self.kinds: dict[tuple[str, str], str] = {
            (schema["name"], t["name"]): kind
            for schema in catalog["schemas"]
            for kind, key in (("enum", "enums"), ("composite", "composite_types"))
            for t in schema.get(key) or []
        }

    def any(self, test: Callable[[Json], bool], *, params: bool = False) -> bool:
        """Whether a column or parameter, or with ``params`` only a parameter,
        passes ``test``.
        """
        return any(map(test, self.params if params else self.columns))

    def kind(self, column: Json) -> str | None:
        """``enum`` or ``composite`` if a column has such a type."""
        name = (column["type"]["schema"] or self.default, column["type"]["name"])
        return self.kinds.get(name)

    def schemas(self) -> set[str]:
        """The schemas of every table and type the query touches, but the
        default one.
        """
        found = {t["schema"] for t in self.tables}
        found |= {c["type"]["schema"] for c in self.columns}
        return {s for s in found if s} - {self.default, "pg_catalog"}

    def verdict(
        self, outcome: str, reports: list[autoprop.Report]
    ) -> tuple[str, str] | None:
        """What happened to the query, with the message if it is ``error``;
        ``None`` if an error it has no part in kept it from generating.
        """
        name, file = self.query["name"], Path(self.query["filename"])
        if outcome.startswith("error:"):
            message = outcome.removeprefix("error:")
            ours = [f'{file.name}: query "{name}": ']
            ours += [f'table "{t["name"]}": ' for t in self.tables]
            prefix = next((p for p in ours if message.startswith(p)), None)
            return ("error", message.removeprefix(prefix)) if prefix else None
        if any(report.failure for report in reports):
            return "FAILING", ""
        exercised = any(
            report.queries[function] == (file.stem, name)
            for report in reports
            for function in report.exercised
        )
        return ("runtime" if exercised else "static"), ""


def is_array(column: Json) -> bool:
    """Whether sqlc reports a column as an array."""
    return bool(column["is_array"] or column["array_dims"])


def is_json(column: Json) -> bool:
    """Whether a column is JSON, in any engine."""
    name = column["type"]["name"].lower().removeprefix("pg_catalog.")
    return name in {"json", "jsonb"}


def cmd(name: str) -> tuple[str, Callable[[Query], bool]]:
    """The row of one sqlc command."""
    return f"`{name}`", lambda q: q.query["cmd"] == name


COMMANDS = (
    *(":one", ":many", ":exec", ":execrows", ":execresult", ":execlastid"),
    *(":copyfrom", ":batchexec", ":batchmany", ":batchone"),
)
# Each row: a feature, and whether a query uses it.
FEATURES: tuple[tuple[str, Callable[[Query], bool]], ...] = (
    ("any query", lambda _: True),
    *map(cmd, COMMANDS),
    ("`sqlc.embed`", lambda q: any(c["embed_table"] for c in q.query["columns"])),
    ("`sqlc.slice`", lambda q: q.any(lambda c: c["is_sqlc_slice"], params=True)),
    (
        "named parameters (`@p`, `sqlc.arg`)",
        lambda q: q.any(lambda c: c["is_named_param"], params=True),
    ),
    (
        "nullable named parameters (`sqlc.narg`)",
        lambda q: q.any(
            lambda c: c["is_named_param"] and not c["not_null"], params=True
        ),
    ),
    ("enums", lambda q: q.any(lambda c: q.kind(c) == "enum" and not is_array(c))),
    ("enum arrays", lambda q: q.any(lambda c: q.kind(c) == "enum" and is_array(c))),
    ("composite types", lambda q: q.any(lambda c: q.kind(c) == "composite")),
    (
        "arrays of other types",
        lambda q: q.any(lambda c: is_array(c) and q.kind(c) != "enum"),
    ),
    ("JSON", lambda q: q.any(is_json)),
    (
        "Python keywords as names",
        lambda q: q.any(lambda c: keyword.iskeyword(c["name"])),
    ),
    ("tables or types outside the default schema", lambda q: bool(q.schemas())),
)


def render(reports: list[autoprop.Report], generated: Path, golden: Path) -> str:
    """``docs/compat.md``, from the sweep's ``reports``, the corpus ledger
    and packages ``cargo test`` wrote to ``generated``, and the golden cases
    laid out under ``golden``.
    """
    cells: dict[tuple[str, str], Cell] = defaultdict(lambda: defaultdict(Counter))
    queries = 0
    # Each other engine's requests, by outcome.
    others: Counter[str] = Counter()
    for request, backend, outcome, swept in requests(reports, generated, golden):
        engine = ENGINES.get(request["settings"]["engine"])
        if engine is None:
            others[outcome.removeprefix("error:")] += 1
            continue
        column = f"{engine} {BACKENDS[backend]}"
        if column not in COLUMNS:
            continue
        # One output may be another engine's too: only PostgreSQL is swept.
        ran = swept if engine == "PostgreSQL" else []
        for query in (Query(q, request["catalog"]) for q in request["queries"]):
            found = query.verdict(outcome, ran)
            if found is None:
                continue
            queries += 1
            for feature, uses in FEATURES:
                if uses(query):
                    verdict, message = found
                    cells[feature, column][verdict][message] += 1
    return document(cells, queries, others)


def requests(
    reports: list[autoprop.Report], generated: Path, golden: Path
) -> Iterator[tuple[Json, str, str, list[autoprop.Report]]]:
    """Every request generated, its backend, its ledger outcome, and the
    sweep's reports on the package it generated, if swept.
    """
    swept: dict[str, list[autoprop.Report]] = defaultdict(list)
    for report in reports:
        swept[report.target.package].append(report)
    for case in golden_cases():
        if (golden / case.name).is_dir():
            request = json.loads(case.request.read_text())
            yield request, case.options["backend"], "ok", swept[case.name]

    profiles = json.loads((CORPUS / "profiles.json").read_text())
    packages = autoprop.corpus_packages(generated)
    ledger = (generated / "ledger.tsv").read_text().splitlines()[1:]
    for name, profile, outcome, _ in (line.split("\t") for line in ledger):
        request = json.loads((CORPUS / name).read_text())
        package = packages.get((name, profile))
        reports = swept[package] if package else []
        yield request, profiles[profile]["backend"], outcome, reports


def document(
    cells: dict[tuple[str, str], Cell], queries: int, others: Counter[str]
) -> str:
    """The table, its legend, the error messages it numbers, and what became
    of the ``others`` engines' requests.
    """
    notes: dict[str, int] = {}

    def cell(feature: str, column: str) -> str:
        found = cells.get((feature, column), {})
        best = next((v for v in VERDICTS if v in found and v != "error"), None)
        if "error" not in found:
            return best or "-"
        ((message, _),) = found["error"].most_common(1)
        number = notes.setdefault(message, len(notes) + 1)
        if best is None:
            return f"error ({number})"
        errors = found["error"].total()
        total = sum(by_message.total() for by_message in found.values())
        return f"{best}, error in {errors} of {total} ({number})"

    rows = [
        f"| {feature} | {' | '.join(cell(feature, c) for c in COLUMNS)} |"
        for feature, _ in FEATURES
    ]
    unsent("rename", "overrides")
    rows.append("| `overrides`, `rename` |" + " error |" * len(COLUMNS))
    source = (CORPUS / "SOURCE.toml").read_text()
    sqlc = re.search(r'^sqlc = "(.+)"$', source, re.MULTILINE)
    lines = [
        "# sqlc features",
        "",
        "Generated by `cargo xtask verify` (`tests/python/compat.py`); do not edit.",
        (
            f"Computed over {queries} queries, once per option profile: the golden"
            f" fixtures and sqlc {sqlc and sqlc[1]}'s end-to-end corpus. A column"
            " is an engine and a backend that runs it."
        ),
        "",
        "| Feature | " + " | ".join(COLUMNS) + " |",
        "|---|" + "---|" * len(COLUMNS),
        *rows,
        "",
        *(f"- **{verdict}**: {meaning}" for verdict, meaning in LEGEND.items()),
        "",
        (
            "A cell is the strongest verdict among the queries using the feature"
            " that generated, then how many of them all fail with an error."
        ),
        "",
        *(
            f"Other engines: {n} requests, once per option profile, fail with `{o}`."
            for o, n in others.items()
        ),
        "",
        (
            "`overrides` and `rename` are sqlc-gen-go configuration: sqlc does not"
            " send them to a plugin (`protos/sqlc.proto`), and as plugin options"
            " they are unknown keys, an error (`cargo xtask capture --check` runs it)."
        ),
        "",
        "Errors:",
        "",
        *(f"{n}. {message}" for message, n in notes.items()),
    ]
    return "\n".join(lines).rstrip() + "\n"


def unsent(*fields: str) -> None:
    """Raise unless sqlc's ``Settings`` message, what a plugin gets of the
    config, has none of ``fields``.
    """
    proto = (ROOT / "protos/sqlc.proto").read_text()
    match = re.search(r"^message Settings \{(.*?)^\}", proto, re.MULTILINE | re.DOTALL)
    if match is None or any(re.search(rf"\b{f}\s*=", match[1]) for f in fields):
        message = f"sqlc now sends {', '.join(fields)} to plugins; support them"
        raise AssertionError(message)
