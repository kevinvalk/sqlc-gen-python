"""Prove the generated Python correct at runtime, and the proof sharp.

usage: verify.py [-h] [--mutants] [--check] GENERATED

GENERATED is what ``cargo test`` exported (``cargo xtask verify`` fills it). Each layer, a row of the summary at the
end, is described in ``docs/testing.md``. Every layer runs on copies in a temporary directory, against one
PostgreSQL (Docker, or ``SQLC_TEST_DATABASE_URL``) and one MySQL (Docker, or ``SQLC_TEST_MYSQL_URL``).
"""

from __future__ import annotations

import argparse
import os
import shutil
import signal
import subprocess
import sys
import tempfile
import textwrap
import time
from dataclasses import dataclass, replace
from functools import partial
from pathlib import Path
from typing import TYPE_CHECKING

import autoprop
import mysql
import postgres
import typemap
import typemap_postgres
from examples import ROOT, golden_cases
from gate import PYTHONS, Environment, sync
from typemap_probe import release

if TYPE_CHECKING:
    from collections.abc import Callable

    import sqlalchemy

    # Writes a generated document, and returns what that did and whether it fails the run.
    Write = Callable[[Path, str], tuple[str, bool]]

HERE = Path(__file__).resolve().parent
SUITE = HERE / "suite"
CASE = "invoicing_sqlalchemy"  # The case we plant a mutant in, unless it names another

# What catches a mutant: the static gate, the runtime suite's test (of PostgreSQL or of MySQL), the type-truth probe,
# or the runtime sweep.
GATE = "gate"
RUNTIME = "test_every_function_against_postgres"
MYSQL_RUNTIME = "test_every_function_against_mysql"
TYPEMAP = "typemap"
SWEEP = "autoprop"

# No layer's Python takes anything from the caller: it reads no `PYTHON*` variable (`-E`) or user site (`-s`). So it
# needs `-B` to keep tests/python clean of bytecode.
PYTHON_FLAGS = ("-B", "-E", "-s")


def golden(generated: Path, root: Path) -> Path:
    """Lay the generated golden cases out as ``root/<case>/``, with the options of their examples' ``sqlc.yaml``."""
    for case in golden_cases():
        shutil.copytree(generated / case.name, root / case.name)
    return root


def workspace(root: Path, cases: Path, mutant: Mutant | None = None) -> Path:
    shutil.copytree(cases, root)
    if mutant is not None:
        path = root / mutant.case / mutant.file
        source = ruff_format(mutant.plant(path.read_text()))
        path.write_text(source)

    # We make usage a typed package of its own, so the gate imports it too.
    (root / "usage").mkdir()
    shutil.copy(SUITE / "usage.py", root / "usage" / "__init__.py")
    (root / "usage" / "py.typed").touch()
    return root


def failure(command: list[str], root: Path, *, quiet: bool) -> str | None:
    """Run a layer's command on the cases at ``root``, against the servers ``main`` put in the environment, and return
    what it said when it failed.
    """
    process = subprocess.run(
        command,
        env=os.environ | {"SQLC_CASES": str(root)},
        cwd=HERE,
        capture_output=quiet,
        text=True,
        check=False,
    )
    return f"{process.stdout or ''}{process.stderr or ''}" if process.returncode != 0 else None


def gate(root: Path) -> list[str]:
    return [sys.executable, *PYTHON_FLAGS, str(HERE / "gate.py"), str(root)]


def pytest(*options: str, python: str = sys.executable) -> list[str]:
    return [python, *PYTHON_FLAGS, "-m", "pytest", "-q", *options, str(SUITE)]


def probe(root: Path, python: str = sys.executable) -> list[str]:
    return [python, *PYTHON_FLAGS, str(HERE / "typemap.py"), str(root)]


def ruff_format(source: str) -> str:
    """Sort imports and format, as the plugin's renderer does.

    The renderer formats a tree, which has no trailing commas, so we must not let a comma left by the original layout
    keep a shorter planted line exploded.
    """
    ruff = Path(sys.executable).with_name("ruff")
    no_magic = ["--config", "format.skip-magic-trailing-comma=true"]
    for arguments in (["check", "--select", "I", "--fix"], ["format", *no_magic]):
        source = subprocess.run(
            [ruff, *arguments, "--isolated", "--target-version", f"py{PYTHONS[0].replace('.', '')}", "-"],
            input=source,
            capture_output=True,
            text=True,
            check=True,
        ).stdout
    return source


@dataclass(frozen=True)
class Mutant:
    """A plausible generator bug: ``old`` becomes ``new`` in ``file`` of the golden case ``case``.

    ``caught_by`` names the layer that must catch it, and ``says`` is a part of what that layer said when it failed, so
    that something else failing there does not count.
    """

    caught_by: str
    says: str
    file: str
    old: str
    new: str
    case: str = CASE
    everywhere: bool = False  # Whether every `old` changes, as a generator bug would, and not only the first

    def plant(self, source: str) -> str:
        if self.old not in source:
            message = f"mutant no longer applies: {self.old!r} in {self.file}"
            raise ValueError(message)
        return source.replace(self.old, self.new, -1 if self.everywhere else 1)


MUTANTS = {
    "enum value left as str": Mutant(
        RUNTIME,
        ".status: str 'draft' is not <enum 'InvoiceStatus'>",
        "models.py",
        "status=InvoiceStatus(row[3])",
        "status=row[3]",
    ),
    # `list[Any]` type-checks as a list of anything, so only a value tells.
    "bare enums left as str": Mutant(
        RUNTIME,
        "list_invoice_statuses() ->[0]: str 'draft' is not <enum 'InvoiceStatus'>",
        "invoices.py",
        """return [
        models.InvoiceStatus(value)
        for value in connection.execute(_LIST_INVOICE_STATUSES).scalars()
    ]""",
        "return list(connection.execute(_LIST_INVOICE_STATUSES).scalars())",
    ),
    "NULL guard missing on a nullable enum": Mutant(
        RUNTIME,
        "None is not a valid BillingPaymentMethod",
        "models.py",
        "preferred_method=None if row[4] is None else BillingPaymentMethod(row[4])",
        "preferred_method=BillingPaymentMethod(row[4])",
    ),
    # PostgreSQL names the column `coalesce`, so a type given by name misses it.
    "enum arrays typed by name": Mutant(
        RUNTIME,
        "'{' is not a valid BillingPaymentMethod",
        "customers.py",
        """WHERE id = :id"
).columns(
    sqlalchemy.column(
        "accepted_methods",
        postgresql.ARRAY(sqlalchemy.Enum(*models.BillingPaymentMethod)),
    )
)""",
        """WHERE id = :id"
).columns(
    accepted_methods=postgresql.ARRAY(sqlalchemy.Enum(*models.BillingPaymentMethod))
)""",
    ),
    # `.first()` is `Row | None`, which `from_row` refuses.
    ":one takes the first row": Mutant(
        GATE,
        'is not assignable to "Sequence[Any]"',
        "customers.py",
        '{"name": name}).one()',
        '{"name": name}).first()',
    ),
    # `.scalar()` is `Any | None`, which the annotated local refuses.
    ":one takes the first value": Mutant(
        GATE,
        'is not assignable to declared type "UUID"',
        "customers.py",
        '{"email": email}\n    ).scalar_one()',
        '{"email": email}\n    ).scalar()',
    ),
    # Without `raise_on_none`, `.first()` type-checks as `one_or_none()` does but takes the first of several rows.
    ":one or none takes the first row": Mutant(
        RUNTIME,
        "Failed: DID NOT RAISE MultipleResultsFound",
        "customers.py",
        '{"name": name})\n    ).one_or_none()',
        '{"name": name})\n    ).first()',
        "invoicing_sqlalchemy_async",
    ),
    "JSON None written as JSON null": Mutant(
        RUNTIME,
        "AssertionError: metadata=None is stored as JSON null",
        "customers.py",
        "JSONB(none_as_null=True)",
        "JSONB()",
    ),
    "JSON parameter untyped": Mutant(
        RUNTIME,
        "cannot adapt type 'dict'",
        "customers.py",
        """    .bindparams(
        sqlalchemy.bindparam("metadata", type_=postgresql.JSONB(none_as_null=True))
    )
""",
        "",
    ),
    # Two swapped values of one type type-check and run, so at runtime only sqlc's SQL run directly tells.
    "columns swapped": Mutant(
        SWEEP,
        "but sqlc's SQL run directly returned",
        "models.py",
        "id=row[0],\n            customer_id=row[1],",
        "id=row[1],\n            customer_id=row[0],",
    ),
    # The drivers return money as text, and only the type-truth probe reads it.
    "money as Decimal": Mutant(
        TYPEMAP,
        "MISMATCH: value: str '$0.00' is not <class 'decimal.Decimal'>",
        "models.py",
        "    price: str\n",
        "    price: Decimal\n",
        case=typemap_postgres.PGTYPES,
    ),
    # sqlc reports what stands left of a RIGHT JOIN of several tables NOT NULL, yet a customer without payments has no
    # number!
    "a column left of a RIGHT JOIN as sqlc reports it": Mutant(
        RUNTIME,
        "list_customer_payments() ->[1].number: NoneType None is not <class 'str'>",
        "payments.py",
        "    number: str | None\n    amount: Decimal | None\n",
        "    number: str\n    amount: Decimal | None\n",
    ),
    # psycopg returns a geometric type as its text, asyncpg as an object.
    "psycopg point as a tuple": Mutant(
        TYPEMAP,
        ".c_point: '(0,0)' is not tuple[float, float]",
        "models.py",
        "    c_point: str | None\n",
        "    c_point: tuple[float, float] | None\n",
        case="pgtext_psycopg",
    ),
    # MySQL's TIME is a duration, which may be negative or over a day.
    "MySQL TIME as time": Mutant(
        TYPEMAP,
        ".c_time: timedelta datetime.timedelta(0) is not <class 'datetime.time'>",
        "models.py",
        "    c_time: dt.timedelta | None\n",
        "    c_time: dt.time | None\n",
        case="mysqltypes_pymysql",
    ),
    # SQLite has no boolean, it stores and returns an integer.
    "SQLite BOOLEAN read unconverted": Mutant(
        TYPEMAP,
        ".c_boolean: int 1 is not <class 'bool'>",
        "models.py",
        "c_boolean=None if row[10] is None else bool(row[10])",
        "c_boolean=row[10]",
        case="sqlitetypes_sqlite3",
    ),
    "timestamptz annotated as date": Mutant(
        RUNTIME,
        "record_payment() ->.received_at: datetime",
        "models.py",
        "    received_at: dt.datetime\n",
        "    received_at: dt.date\n",
    ),
    "count annotated as str": Mutant(
        GATE,
        'expected "int" but received "str"',
        "invoices.py",
        "customer_id: UUID) -> int:",
        "customer_id: UUID) -> str:",
    ),
    "row count returned as bool": Mutant(
        RUNTIME,
        "update_customer_email() ->: bool True is not <class 'int'>",
        "customers.py",
        '"id": id}\n    ).rowcount',
        '"id": id}\n    ).rowcount > 0',
    ),
    ":many returns a lazy generator": Mutant(
        GATE,
        'Generator[LineItem, None, None]", expected "list[LineItem]"',
        "line_items.py",
        """    return [
        models.LineItem.from_row(row)
        for row in connection.execute(_LIST_LINE_ITEMS, {"invoice_id": invoice_id})
    ]""",
        """    return (
        models.LineItem.from_row(row)
        for row in connection.execute(_LIST_LINE_ITEMS, {"invoice_id": invoice_id})
    )""",
    ),
    "empty :copyfrom executed": Mutant(
        RUNTIME,
        "A value is required for bind parameter 'invoice_id'",
        "line_items.py",
        "    if params:\n        connection.execute(_CREATE_LINE_ITEMS, params)",
        "    connection.execute(_CREATE_LINE_ITEMS, params)",
    ),
    # psycopg returns an enum array as its text, which only the enum parses.
    "enum array text left unparsed": Mutant(
        RUNTIME,
        ".accepted_methods: str is not a list",
        "models.py",
        "accepted_methods=BillingPaymentMethod.from_array(row[5])",
        "accepted_methods=row[5]",
        case="invoicing_models",
    ),
    # A statement's backslashes are Python escapes unless doubled: a `\n` would reach PostgreSQL as a line break.
    "backslashes of a statement left undoubled": Mutant(
        RUNTIME,
        "' $2\\n'",
        "customers.py",
        r"E'\\\\ $2\\n'",
        r"E'\\ $2\n'",
        case="invoicing_psycopg",
    ),
    # An empty slice is a subquery of no rows, as `x NOT IN (NULL)` is NULL and keeps no row.
    "empty slice as NULL": Mutant(
        MYSQL_RUNTIME,
        "assert [] == [",
        "query.py",
        """            _LIST_CUSTOMERS_EXCEPT.replace(
                "/*SLICE:ids*/%s",
                ", ".join(["%s"] * len(ids)) or "SELECT NULL FROM DUAL WHERE FALSE",""",
        """            _LIST_CUSTOMERS_EXCEPT.replace(
                "/*SLICE:ids*/%s",
                ", ".join(["%s"] * len(ids)) or "NULL",""",
        case="billing_pymysql",
        everywhere=True,  # The function and its stream, or they would read different rows
    ),
    # PyMySQL formats the statement with `%`, so a literal `%` must be `%%`.
    "percent left undoubled for PyMySQL": Mutant(
        MYSQL_RUNTIME,
        "not enough arguments for format string",
        "query.py",
        "NOT LIKE '%%:test%%'",
        "NOT LIKE '%:test%'",
        case="billing_pymysql",
    ),
    # asyncmy's stubs leave what a statement runs through untyped, and strict pyright rejects a call of that. Once they
    # type it this mutant is no longer caught, and we remove the bridge (`AGENTS.md`, "Workarounds").
    "asyncmy's execute called without the bridge": Mutant(
        GATE,
        'Type of "execute" is partially unknown',
        "query.py",
        "await _typed(cur).execute(_GET_CUSTOMER, (id,))",
        "await cur.execute(_GET_CUSTOMER, (id,))",
        case="billing_pymysql_async",
    ),
    # The suite's connection makes cursors of dicts, so only an explicit cursor class reads rows by index.
    "asyncmy rows read through the connection's cursor class": Mutant(
        MYSQL_RUNTIME,
        "KeyError: 0",
        "query.py",
        """async with connection.cursor(asyncmy.cursors.Cursor) as cur:
        await _typed(cur).execute(_GET_CUSTOMER, (id,))""",
        """async with connection.cursor() as cur:
        await _typed(cur).execute(_GET_CUSTOMER, (id,))""",
        case="billing_pymysql_async",
    ),
    # psycopg's `execute` reads every row, so a stream must read from a cursor PostgreSQL holds.
    "psycopg stream read through a buffered cursor": Mutant(
        RUNTIME,
        "the stream read every row before it yielded one",
        "countdown.py",
        """    with psycopg.RawServerCursor(
        connection, "stream_countdown", row_factory=tuple_row
    ) as cur:""",
        "    with psycopg.RawCursor(connection, row_factory=tuple_row) as cur:",
        case="invoicing_psycopg",
    ),
    # A MySQL driver's default cursor reads every row on `execute`, its unbuffered one as it is iterated.
    "PyMySQL stream read through a buffered cursor": Mutant(
        MYSQL_RUNTIME,
        "the stream read every row before it yielded one",
        "query.py",
        """    with connection.cursor(pymysql.cursors.SSCursor) as cur:
        cur.execute(_COUNTDOWN)""",
        """    with connection.cursor(pymysql.cursors.Cursor) as cur:
        cur.execute(_COUNTDOWN)""",
        case="billing_pymysql",
    ),
    "unused import": Mutant(
        GATE,
        "F401",
        "payments.py",
        "import datetime as dt\n",
        "import datetime as dt\nimport enum\n",
    ),
}


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("generated", type=Path, help="what `cargo test` exported")
    parser.add_argument("--mutants", action="store_true", help="also plant bugs that must be caught")
    parser.add_argument("--check", action="store_true", help="fail if a document would change")
    args = parser.parse_args()
    generated = args.generated.resolve()
    write = partial(document, check=args.check)
    started = time.monotonic()
    signal.signal(signal.SIGTERM, signal.default_int_handler)  # So a termination still removes the servers

    with (
        postgres.server() as server,
        mysql.server() as mysql_server,
        tempfile.TemporaryDirectory() as temporary,
    ):
        # One server of each, for every layer and every Python that runs the suite.
        os.environ[postgres.ENV] = server.render_as_string(hide_password=False)
        os.environ[mysql.ENV] = mysql_server.render_as_string(hide_password=False)

        directory = Path(temporary)
        cases = golden(generated, directory / "golden")
        control = workspace(directory / "control", cases)
        layers: list[Callable[[], Row]] = [
            partial(static_and_pytest, control),
            partial(types, control, server, mysql_server, write),
            partial(sweep, control, generated, server, write),
            partial(newer, control),
            *([partial(mutants, directory, cases, generated, server)] if args.mutants else []),
        ]
        setup = time.monotonic() - started
        summary = [timed(layer) for layer in layers]

    print(f"\n{'layer':<12} {'verdict':<8} {'time':>6}  detail")
    print(f"{'setup':<12} {'':<8} {setup:>5.0f}s  servers and workspaces")
    for (layer, failed, detail), seconds in summary:
        lines = detail.replace("\n", "\n" + " " * 30)
        print(f"{layer:<12} {'FAILED' if failed else 'ok':<8} {seconds:>5.0f}s  {lines}")
    print(f"{'total':<12} {'':<8} {time.monotonic() - started:>5.0f}s")
    return int(any(failed for (_, failed, _), _ in summary))


Row = tuple[str, bool, str]


def timed(layer: Callable[[], Row]) -> tuple[Row, float]:
    started = time.monotonic()
    return layer(), time.monotonic() - started


def static_and_pytest(control: Path) -> Row:
    print("gate and pytest:", flush=True)
    failed = [
        layer
        for layer, command in [(GATE, gate(control)), ("pytest", pytest())]
        if failure(command, control, quiet=False) is not None
    ]
    cases = sum(1 for path in control.iterdir() if path.name != "usage")
    detail = f"failed: {', '.join(failed)}" if len(failed) > 0 else f"{cases} cases"
    return "gate+pytest", len(failed) > 0, detail


def types(control: Path, server: sqlalchemy.URL, mysql_server: sqlalchemy.URL, write: Write) -> Row:
    print("\ntype-truth probe:", flush=True)
    sys.path.insert(0, str(control))
    sections = typemap.probe(server, mysql_server)
    mismatches = [mismatch for section in sections for mismatch in section.mismatches]
    for mismatch in mismatches:
        print(f"  {mismatch}")

    # A document of mismatches is no record of what the drivers do, so we do not write it.
    written, stale = (
        ("not written", False)
        if len(mismatches) > 0
        else write(ROOT / "docs/typemap.md", typemap.render(sections, newer_versions()))
    )
    probed = ", ".join(f"{len(s.probes)} {s.engine} types on {len(s.drivers)} drivers" for s in sections)
    edges = sum(len(by_driver) for section in sections for p in section.probes for by_driver in p.notes.values())
    detail = (
        f"{probed}, {len(mismatches)} mismatches\n{edges} edges that raise or are lossy, in docs/typemap.md ({written})"
    )
    return TYPEMAP, len(mismatches) > 0 or stale, detail


def sweep(control: Path, generated: Path, server: sqlalchemy.URL, write: Write) -> Row:
    print("\nruntime sweep:", flush=True)
    reports = autoprop.sweep(server, autoprop.targets(control, generated), [control, generated])
    failed, totals, record = autoprop.summarize(reports)

    # A failed sweep leaves the record as it is, as each function it lists is one the sweep must go on exercising.
    written, stale = ("not written", False) if failed else write(autoprop.EXERCISED, record)
    totals[1] += f", in {autoprop.EXERCISED.name} ({written})"
    return SWEEP, failed or stale, "\n".join(totals)


def newer_versions() -> str:
    return ", ".join(
        f"Python {version} with sqlalchemy {release(Environment(version).installed('sqlalchemy'))}"
        for version in PYTHONS[1:]
    )


def newer(control: Path) -> Row:
    # We run pytest and the type-truth probe again on each newer Python, which installs SQLAlchemy 2.1, but only for
    # their verdicts: the document records the oldest Python. The drivers are the same on every Python, so the endtoend
    # sweep runs once.
    failed = []
    for version in PYTHONS[1:]:
        print(f"\nPython {version}:", flush=True)
        python = str(sync(Environment(version)).bin("python"))
        for layer, command in [
            ("pytest", pytest(python=python)),
            ("typemap", probe(control, python)),
        ]:
            if failure(command, control, quiet=False) is not None:
                failed.append(f"{layer} on {version}")
    detail = ", ".join(failed) if len(failed) > 0 else f"pytest and typemap on {', '.join(PYTHONS[1:])}"
    return "newer", len(failed) > 0, detail


def document(path: Path, text: str, *, check: bool) -> tuple[str, bool]:
    if path.is_file() and path.read_text() == text:
        return "unchanged", False
    if check:
        return f"OUT OF DATE: run `cargo xtask verify` to update {path.name}", True
    path.write_text(text)
    return "updated", False


def mutants(directory: Path, cases: Path, generated: Path, server: sqlalchemy.URL) -> Row:
    print("\nmutants:", flush=True)
    wrong: list[str] = []
    for i, (name, mutant) in enumerate(MUTANTS.items()):
        started = time.monotonic()
        root = workspace(directory / f"mutant{i}", cases, mutant)
        said = caught(mutant, root, generated, server)
        seconds = time.monotonic() - started
        if said is not None and mutant.says in said:
            print(f"  {seconds:>4.0f}s caught by {mutant.caught_by}: {name}", flush=True)
            continue

        wrong.append(name)
        why = "ESCAPED" if said is None else f"caught, not saying {mutant.says!r}"
        print(f"  {seconds:>4.0f}s {why}, by {mutant.caught_by}: {name}", flush=True)
        print(textwrap.indent((said or "").strip()[-2000:], "    | "), flush=True)
    detail = f"{len(MUTANTS) - len(wrong)}/{len(MUTANTS)} planted bugs caught as meant"
    return "mutants", len(wrong) > 0, detail


def caught(mutant: Mutant, root: Path, generated: Path, server: sqlalchemy.URL) -> str | None:
    """What the layer named for ``mutant`` said when it failed on the cases at ``root``, or ``None`` when it passed."""
    if mutant.caught_by == SWEEP:
        targets = autoprop.targets(root, generated)
        swept = [replace(target, shrinks=False) for target in targets if target.package == mutant.case]
        reports = autoprop.sweep(server, swept, [root])
        failures = [report.failure for report in reports if report.failure != ""]
        return "\n".join(failures) if len(failures) > 0 else None

    command = {
        GATE: gate(root),
        RUNTIME: pytest("-x", "-k", RUNTIME),
        MYSQL_RUNTIME: pytest("-x", "-k", MYSQL_RUNTIME),
        TYPEMAP: probe(root),
    }[mutant.caught_by]
    return failure(command, root, quiet=True)


if __name__ == "__main__":
    sys.exit(main())
