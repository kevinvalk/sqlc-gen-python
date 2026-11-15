"""Prove the generated Python correct at runtime, and the proof sharp.

usage: verify.py [-h] [--mutants] [--check] GENERATED

GENERATED is what ``cargo test`` exported (``cargo xtask verify`` fills it).
Each layer, a row of the summary at the end, is described in
``docs/python-output.md`` §Proof: the static gate, pytest, the type-truth
probe (writing ``docs/typemap.md``), the runtime sweep, pytest and the probe
again on newer Pythons, and with ``--mutants`` planted bugs. Every layer runs
on copies in a temporary directory, against one PostgreSQL (Docker, or
``SQLC_TEST_DATABASE_URL``) and one MySQL (Docker, or ``SQLC_TEST_MYSQL_URL``).
With ``--check``, a document that would change fails the run instead of being
written.
"""

from __future__ import annotations

import argparse
import os
import shutil
import subprocess
import sys
import tempfile
import textwrap
from dataclasses import dataclass
from functools import partial
from pathlib import Path
from typing import TYPE_CHECKING

import autoprop
import mysql
import postgres
import typemap
import typemap_postgres
from examples import ROOT
from gate import PYTHONS, Env, sync
from typemap_probe import release

if TYPE_CHECKING:
    from collections.abc import Callable

    import sqlalchemy

    # Writes a generated document: what that did, and whether it fails the run.
    Write = Callable[[Path, str], tuple[str, bool]]

HERE = Path(__file__).resolve().parent
SUITE = HERE / "suite"
# The case mutants are planted in unless they name another.
CASE = "invoicing_sqlalchemy"
# What catches a mutant: the static gate, the runtime suite's test (of
# PostgreSQL or of MySQL), the type-truth probe, or the runtime sweep.
GATE = "gate"
RUNTIME = "test_every_function_against_postgres"
MYSQL_RUNTIME = "test_every_function_against_mysql"
TYPEMAP = "typemap"
SWEEP = "autoprop"


# Every layer's Python is the caller's in nothing: it reads no `PYTHON*`
# variable (`-E`) or user site (`-s`), and so needs `-B` to keep tests/python
# clean of bytecode.
PYTHON_FLAGS = ("-B", "-E", "-s")


def golden(generated: Path, root: Path) -> Path:
    """Lay the generated golden cases out as ``root/<case>/``, each with its
    generated files; their options are their examples' ``sqlc.yaml``'s.
    """
    for package in sorted(generated.glob("cases__*/")):
        shutil.copytree(package, root / package.name.removeprefix("cases__"))
    return root


def workspace(root: Path, cases: Path, mutant: Mutant | None = None) -> Path:
    """Copy the golden ``cases`` and ``usage.py`` into ``root``, planting
    ``mutant``.
    """
    shutil.copytree(cases, root)
    if mutant:
        path = root / mutant.case / mutant.file
        source = ruff_format(mutant.plant(path.read_text()))
        path.write_text(source)
    # A typed package of its own, so the gate imports it too.
    (root / "usage").mkdir()
    shutil.copy(SUITE / "usage.py", root / "usage" / "__init__.py")
    (root / "usage" / "py.typed").touch()
    return root


def failure(command: list[str], root: Path, *, quiet: bool) -> str | None:
    """Run ``command``, a layer, on the cases at ``root`` and the servers
    ``main`` put in the environment: what it said if it failed, which with
    ``quiet`` is not printed, else ``None``.
    """
    done = subprocess.run(
        command,
        env=os.environ | {"SQLC_CASES": str(root)},
        cwd=HERE,
        capture_output=quiet,
        text=True,
        check=False,
    )
    return f"{done.stdout or ''}{done.stderr or ''}" if done.returncode else None


def gate(root: Path) -> list[str]:
    """The static gate over ``root``."""
    return [sys.executable, *PYTHON_FLAGS, str(HERE / "gate.py"), str(root)]


def pytest(*options: str, python: str = sys.executable) -> list[str]:
    """The runtime suite, with ``options``, on ``python``."""
    return [python, *PYTHON_FLAGS, "-m", "pytest", "-q", *options, str(SUITE)]


def probe(root: Path, python: str = sys.executable) -> list[str]:
    """The type-truth probe of the cases at ``root``, on ``python``: only its
    verdict.
    """
    return [python, *PYTHON_FLAGS, str(HERE / "typemap.py"), str(root)]


def ruff_format(source: str) -> str:
    """Sort imports and format, as the plugin's renderer does.

    The renderer formats a tree, which has no trailing commas, so a comma left
    by the original layout must not keep a shorter planted line exploded.
    """
    ruff = Path(sys.executable).with_name("ruff")
    no_magic = ["--config", "format.skip-magic-trailing-comma=true"]
    for args in (["check", "--select", "I", "--fix"], ["format", *no_magic]):
        source = subprocess.run(
            [ruff, *args, "--isolated", "--target-version", "py310", "-"],
            input=source,
            capture_output=True,
            text=True,
            check=True,
        ).stdout
    return source


@dataclass(frozen=True)
class Mutant:
    """A plausible generator bug: ``old`` becomes ``new`` in ``file`` of the
    golden case ``case``.

    ``caught_by`` is the layer that must catch it: ``GATE``, ``RUNTIME``,
    ``MYSQL_RUNTIME``, ``TYPEMAP`` or ``SWEEP``; and what it ``says`` is a part
    of what that layer said when it failed, so that something else failing
    there does not count.
    """

    caught_by: str
    says: str
    file: str
    old: str
    new: str
    case: str = CASE

    def plant(self, source: str) -> str:
        """Plant the bug in ``source``, the text of ``file``."""
        if self.old not in source:
            message = f"mutant no longer applies: {self.old!r} in {self.file}"
            raise ValueError(message)
        return source.replace(self.old, self.new, 1)


# Plausible generator bugs, and what must catch each.
MUTANTS = {
    "enum value left as str": Mutant(
        RUNTIME,
        ".status: str 'draft' is not <enum 'InvoiceStatus'>",
        "models.py",
        "status=InvoiceStatus(row[3])",
        "status=row[3]",
    ),
    # `list[Any]` type-checks as a list of anything: only a value tells.
    "bare enums left as str": Mutant(
        RUNTIME,
        "list_invoice_statuses() ->[0]: str 'draft' is not <enum 'InvoiceStatus'>",
        "invoices.py",
        """return [
        models.InvoiceStatus(value)
        for value in conn.execute(_LIST_INVOICE_STATUSES).scalars()
    ]""",
        "return list(conn.execute(_LIST_INVOICE_STATUSES).scalars())",
    ),
    "NULL guard missing on a nullable enum": Mutant(
        RUNTIME,
        "None is not a valid BillingPaymentMethod",
        "models.py",
        "preferred_method=None if row[4] is None else BillingPaymentMethod(row[4])",
        "preferred_method=BillingPaymentMethod(row[4])",
    ),
    "enum array left to the driver": Mutant(
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
)""",
    ),
    # PostgreSQL names the column `coalesce`: by name, the type misses it.
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
        '{"email": email}).scalar_one()',
        '{"email": email}).scalar()',
    ),
    # Without `raise_on_none`, `.first()` type-checks as `one_or_none()`
    # does, and takes the first of several rows.
    ":one or none takes the first row": Mutant(
        RUNTIME,
        "Failed: DID NOT RAISE MultipleResultsFound",
        "customers.py",
        '{"name": name})).one_or_none()',
        '{"name": name})).first()',
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
    # Two values of one type swapped type-check and run; at runtime, only
    # sqlc's SQL run directly tells.
    "columns swapped": Mutant(
        SWEEP,
        "but sqlc's SQL run directly returned",
        "models.py",
        "id=row[0],\n            customer_id=row[1],",
        "id=row[1],\n            customer_id=row[0],",
    ),
    "numeric as float": Mutant(
        RUNTIME,
        ".amount: Decimal Decimal('169.47') is not <class 'float'>",
        "models.py",
        "amount: Decimal",
        "amount: float",
    ),
    # The drivers return money as text; only the type-truth probe reads it.
    "money as Decimal": Mutant(
        TYPEMAP,
        "MISMATCH: value: str '$0.00' is not <class 'decimal.Decimal'>",
        "models.py",
        "    price: str\n",
        "    price: Decimal\n",
        case=typemap_postgres.PGTYPES,
    ),
    # sqlc reports what stands left of a RIGHT JOIN of several tables NOT
    # NULL: a customer without payments has no number.
    "a column left of a RIGHT JOIN as sqlc reports it": Mutant(
        RUNTIME,
        "list_customer_payments() ->[1].number: NoneType None is not <class 'str'>",
        "payments.py",
        "    number: str | None\n    amount: Decimal | None\n",
        "    number: str\n    amount: Decimal | None\n",
    ),
    # psycopg returns a geometric type as its text; asyncpg, an object.
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
    # SQLite has no boolean: it stores, and returns, an integer.
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
        '"id": id}).rowcount',
        '"id": id}).rowcount > 0',
    ),
    ":many returns a lazy generator": Mutant(
        GATE,
        'Generator[LineItem, None, None]", expected "list[LineItem]"',
        "line_items.py",
        """    return [
        models.LineItem.from_row(row)
        for row in conn.execute(_LIST_LINE_ITEMS, {"invoice_id": invoice_id})
    ]""",
        """    return (
        models.LineItem.from_row(row)
        for row in conn.execute(_LIST_LINE_ITEMS, {"invoice_id": invoice_id})
    )""",
    ),
    "empty :copyfrom executed": Mutant(
        RUNTIME,
        "A value is required for bind parameter 'invoice_id'",
        "line_items.py",
        "    if params:\n        conn.execute(_CREATE_LINE_ITEMS, params)",
        "    conn.execute(_CREATE_LINE_ITEMS, params)",
    ),
    "enum str() differs across Python versions": Mutant(
        RUNTIME,
        "'InvoiceStatus.PAID' != 'paid'",
        "models.py",
        '    VOID = "void"\n\n'
        "    def __str__(self) -> str:\n"
        "        return str.__str__(self)\n",
        '    VOID = "void"\n',
    ),
    # psycopg returns an enum array as its text, which only the enum parses.
    "enum array text left unparsed": Mutant(
        RUNTIME,
        ".accepted_methods: str is not a list",
        "models.py",
        "accepted_methods=BillingPaymentMethod.from_array(row[5])",
        "accepted_methods=row[5]",
        case="invoicing_psycopg",
    ),
    # PyMySQL formats the statement with `%`: a literal `%` must be `%%`.
    "percent left undoubled for PyMySQL": Mutant(
        MYSQL_RUNTIME,
        "not enough arguments for format string",
        "query.py",
        "NOT LIKE '%%:test%%'",
        "NOT LIKE '%:test%'",
        case="billing_pymysql",
    ),
    # asyncmy's stubs leave what a statement runs through untyped, which
    # strict pyright rejects a call of. Once they type it, this is no longer
    # caught: the bridge is then to be removed (`AGENTS.md`, "Workarounds").
    "asyncmy's execute called without the bridge": Mutant(
        GATE,
        'Type of "execute" is partially unknown',
        "query.py",
        "await _typed(cur).execute(_GET_CUSTOMER, (id,))",
        "await cur.execute(_GET_CUSTOMER, (id,))",
        case="billing_pymysql_async",
    ),
    # The suite's connection makes cursors of dicts.
    "asyncmy rows read through the connection's cursor class": Mutant(
        MYSQL_RUNTIME,
        "KeyError: 0",
        "query.py",
        """async with conn.cursor(asyncmy.cursors.Cursor) as cur:
        await _typed(cur).execute(_GET_CUSTOMER, (id,))""",
        """async with conn.cursor() as cur:
        await _typed(cur).execute(_GET_CUSTOMER, (id,))""",
        case="billing_pymysql_async",
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
    """Run every layer, print what failed and a summary; 1 if anything failed."""
    parser = argparse.ArgumentParser()
    parser.add_argument("generated", type=Path, help="what `cargo test` exported")
    parser.add_argument(
        "--mutants", action="store_true", help="also plant bugs that must be caught"
    )
    parser.add_argument(
        "--check", action="store_true", help="fail if a document would change"
    )
    args = parser.parse_args()
    generated = args.generated.resolve()
    write = partial(document, check=args.check)
    with (
        postgres.server() as server,
        mysql.server() as mysql_server,
        tempfile.TemporaryDirectory() as tmpdir,
    ):
        # One server of each for every layer and Python that runs the suite.
        os.environ[postgres.ENV] = server.render_as_string(hide_password=False)
        os.environ[mysql.ENV] = mysql_server.render_as_string(hide_password=False)
        tmp = Path(tmpdir)
        cases = golden(generated, tmp / "golden")
        control = workspace(tmp / "control", cases)
        summary = [
            static_and_pytest(control),
            types(control, server, mysql_server, write),
            sweep(control, generated, server),
            newer(control),
        ]
        if args.mutants:
            summary.append(mutants(tmp, cases, generated, server))
    print(f"\n{'layer':<12} {'verdict':<8} detail")
    for layer, failed, detail in summary:
        lines = detail.replace("\n", "\n" + " " * 22)
        print(f"{layer:<12} {'FAILED' if failed else 'ok':<8} {lines}")
    return int(any(failed for _, failed, _ in summary))


Row = tuple[str, bool, str]


def static_and_pytest(control: Path) -> Row:
    """The gate and the runtime suite, over the cases as generated."""
    print("gate and pytest:", flush=True)
    fails = [
        layer
        for layer, command in [(GATE, gate(control)), ("pytest", pytest())]
        if failure(command, control, quiet=False) is not None
    ]
    cases = sum(1 for p in control.iterdir() if p.name != "usage")
    detail = f"failed: {', '.join(fails)}" if fails else f"{cases} cases"
    return "gate+pytest", bool(fails), detail


def types(
    control: Path, server: sqlalchemy.URL, mysql_server: sqlalchemy.URL, write: Write
) -> Row:
    """The type-truth probe of every engine, written to ``docs/typemap.md``."""
    print("\ntype-truth probe:", flush=True)
    sys.path.insert(0, str(control))
    sections = typemap.probe(server, mysql_server)
    mismatches = [m for section in sections for m in section.mismatches]
    for mismatch in mismatches:
        print(f"  {mismatch}")
    # A document of mismatches is no record of what the drivers do.
    written, stale = (
        ("not written", False)
        if mismatches
        else write(ROOT / "docs/typemap.md", typemap.render(sections, newer_versions()))
    )
    probed = ", ".join(
        f"{len(s.probes)} {s.engine} types on {len(s.drivers)} drivers"
        for s in sections
    )
    edges = sum(
        len(by_driver)
        for section in sections
        for p in section.probes
        for by_driver in p.notes.values()
    )
    detail = (
        f"{probed}, {len(mismatches)} mismatches"
        f"\n{edges} edges that raise or are lossy, in docs/typemap.md"
        f" ({written})"
    )
    return TYPEMAP, bool(mismatches) or stale, detail


def sweep(control: Path, generated: Path, server: sqlalchemy.URL) -> Row:
    """The runtime sweep over every generated PostgreSQL package."""
    print("\nruntime sweep:", flush=True)
    reports = autoprop.sweep(
        server, autoprop.targets(control, generated), [control, generated]
    )
    fails, totals = autoprop.summarize(reports)
    return SWEEP, fails, "\n".join(totals)


def newer_versions() -> str:
    """Each newer Python, and the SQLAlchemy it installs."""
    return ", ".join(
        f"Python {v} with sqlalchemy {release(Env(v).installed('sqlalchemy'))}"
        for v in PYTHONS[1:]
    )


def newer(control: Path) -> Row:
    """pytest and the type-truth probe again on each newer Python, which
    installs SQLAlchemy 2.1. Only the verdicts: the document records the
    oldest Python. The drivers are the same on every Python, so the corpus
    sweep runs once.
    """
    failed = []
    for version in PYTHONS[1:]:
        print(f"\nPython {version}:", flush=True)
        python = str(sync(Env(version)).bin("python"))
        for layer, command in [
            ("pytest", pytest(python=python)),
            ("typemap", probe(control, python)),
        ]:
            if failure(command, control, quiet=False) is not None:
                failed.append(f"{layer} on {version}")
    detail = ", ".join(failed) or f"pytest and typemap on {', '.join(PYTHONS[1:])}"
    return "newer", bool(failed), detail


def document(path: Path, text: str, *, check: bool) -> tuple[str, bool]:
    """Write a generated document, or with ``check`` only compare it; say
    what that did, and whether it fails the run.
    """
    if path.is_file() and path.read_text() == text:
        return "unchanged", False
    if check:
        return f"OUT OF DATE: run `cargo xtask verify` to update {path.name}", True
    path.write_text(text)
    return "updated", False


def mutants(tmp: Path, cases: Path, generated: Path, server: sqlalchemy.URL) -> Row:
    """Plant every mutant; each must be caught by the layer named for it."""
    print("\nmutants:", flush=True)
    wrong: list[str] = []
    for i, (name, mutant) in enumerate(MUTANTS.items()):
        root = workspace(tmp / f"mutant{i}", cases, mutant)
        said = caught(mutant, root, generated, server)
        if said is not None and mutant.says in said:
            print(f"  caught by {mutant.caught_by}: {name}", flush=True)
            continue
        wrong.append(name)
        why = "ESCAPED" if said is None else f"caught, not saying {mutant.says!r}"
        print(f"  {why}, by {mutant.caught_by}: {name}", flush=True)
        print(textwrap.indent((said or "").strip()[-2000:], "    | "), flush=True)
    detail = f"{len(MUTANTS) - len(wrong)}/{len(MUTANTS)} planted bugs caught as meant"
    return "mutants", bool(wrong), detail


def caught(
    mutant: Mutant, root: Path, generated: Path, server: sqlalchemy.URL
) -> str | None:
    """What the layer named for ``mutant`` said, failing on the cases at
    ``root`` it is planted in; ``None`` if it passed.
    """
    if mutant.caught_by == SWEEP:
        targets = autoprop.targets(root, generated)
        reports = autoprop.sweep(
            server, [t for t in targets if t.package == mutant.case], [root]
        )
        return "\n".join(r.failure for r in reports if r.failure) or None
    command = {
        GATE: gate(root),
        RUNTIME: pytest("-x", "-k", RUNTIME),
        MYSQL_RUNTIME: pytest("-x", "-k", MYSQL_RUNTIME),
        TYPEMAP: probe(root),
    }[mutant.caught_by]
    return failure(command, root, quiet=True)


if __name__ == "__main__":
    sys.exit(main())
