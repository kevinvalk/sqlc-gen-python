"""The static gate: is the generated Python correct, typed and idiomatic?

usage: gate.py ROOT...

Each ROOT holds one package per generated output (``ROOT/<package>/*.py``),
and may hold modules beside them that use those packages, such as the
runtime suite's ``usage.py``. ``cargo xtask check`` fills one from ``cargo test``;
``verify.py`` passes the golden cases as generated. Under the lint contract
in this project's ``pyproject.toml``, everything must pass:

- ``ruff format --check``, and ``ruff check`` deaf to ``# noqa`` (the
  generator may not silence its own findings);
- pyright strict, ``mypy --strict``, and a real import of every package module
  with warnings as errors, under each of ``PYTHONS``: the oldest supported
  Python and the newest stable one;
- both type checkers again, importing every package as installed, as a
  dependency of the application is: only through the Python path, where they
  read a package only if it is marked typed (PEP 561). The generated package
  lives inside the user's own distribution, whose root carries the marker, so
  the installed copy is given one.

Each Python has its own environment in ``target/py``, synced from ``uv.lock``,
so the type checkers see what that Python installs: SQLAlchemy 2.0 on 3.10,
2.1 later. Findings are collated per package into one report, and the exit
status is 1 if there are any, or if a root holds no package to check.
"""

from __future__ import annotations

import importlib.metadata
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
from collections import Counter, defaultdict
from concurrent.futures import ThreadPoolExecutor
from dataclasses import dataclass, replace
from functools import partial
from pathlib import Path
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from collections.abc import Callable

# The oldest supported Python, and the newest stable one.
PYTHONS = ("3.10", "3.15")

PROJECT = Path(__file__).resolve().parent
CONFIG = PROJECT / "pyproject.toml"
RUFF_OPTIONS = ("--output-format=json", "--no-cache", f"--config={CONFIG}")

# Run inside an environment: import every module below a directory with
# warnings as errors, printing a JSON line for each that fails.
IMPORT_ALL = """
import importlib, json, pathlib, sys, traceback, warnings
warnings.simplefilter("error")
root = pathlib.Path(sys.argv[1])
sys.path.insert(0, str(root))
for path in sorted(root.glob("*/**/*.py")):
    module = ".".join(path.relative_to(root).with_suffix("").parts)
    try:
        importlib.import_module(module.removesuffix(".__init__"))
    except BaseException as error:
        message = traceback.format_exception_only(error)[-1]
        print(json.dumps({"path": str(path), "message": message}))
"""


@dataclass(frozen=True)
class Env:
    """The environment for one Python version."""

    python: str

    @property
    def path(self) -> Path:
        """Where the environment lives."""
        return PROJECT.parents[1] / "target" / "py" / self.python

    def bin(self, name: str) -> str:
        """Locate an executable installed in the environment."""
        return str(self.path / "bin" / name)

    def installed(self, package: str) -> str:
        """The version of ``package`` installed in the environment."""
        site = [str(path) for path in self.path.glob("lib/python*/site-packages")]
        (found,) = importlib.metadata.distributions(name=package, path=site)
        return found.version


@dataclass(frozen=True)
class Diagnostic:
    """One complaint about one file."""

    path: Path
    where: str
    message: str


@dataclass(frozen=True)
class Run:
    """What one tool said, and how long it took to say it."""

    tool: str
    seconds: float
    diagnostics: list[Diagnostic]
    # What a tool that failed without a diagnostic printed instead.
    error: str


def main() -> int:
    """Run every check over the roots and report the findings."""
    roots = [Path(arg).resolve() for arg in sys.argv[1:]]
    if not roots:
        sys.exit("usage: gate.py ROOT...")
    for root in roots:
        if not any(root.glob("*/**/*.py")):
            sys.exit(f"gate: {root} holds no package to check")
    files = sum(1 for root in roots for _ in root.glob("**/*.py"))
    packages = [p for root in roots for p in sorted(root.iterdir()) if p.is_dir()]
    envs = [sync(Env(python)) for python in PYTHONS]
    ruff = envs[0].bin("ruff")

    jobs: list[Callable[[], Run]] = [
        partial(ruff_format, ruff, roots),
        partial(ruff_check, ruff, roots),
    ]
    for env in envs:
        jobs += [partial(pyright, env, roots, files), partial(mypy, env, roots)]
        jobs += [
            partial(as_installed, tool, env, roots, packages)
            for tool in ("pyright", "mypy")
        ]
        jobs += [partial(import_all, env, root) for root in roots]
    with ThreadPoolExecutor(len(jobs)) as pool:
        runs = [future.result() for future in [pool.submit(job) for job in jobs]]

    return report(roots, len(packages), runs)


def sync(env: Env) -> Env:
    """Create or update an environment from the lock file."""
    subprocess.run(
        [
            "uv",
            "sync",
            "--quiet",
            "--locked",
            "--managed-python",
            f"--python={env.python}",
        ],
        cwd=PROJECT,
        env=os.environ | {"UV_PROJECT_ENVIRONMENT": str(env.path)},
        check=True,
    )
    return env


def ruff_format(ruff: str, roots: list[Path]) -> Run:
    """Files must already be formatted as ruff formats them."""
    command = [ruff, "format", "--check", *RUFF_OPTIONS, *map(str, roots)]
    return run("ruff format", command, parse_ruff)


def ruff_check(ruff: str, roots: list[Path]) -> Run:
    """Every lint rule the contract selects."""
    command = [ruff, "check", "--ignore-noqa", *RUFF_OPTIONS, *map(str, roots)]
    return run("ruff check", command, parse_ruff)


def parse_ruff(out: str) -> list[Diagnostic]:
    """Read ruff's JSON output, which ``format`` and ``check`` share."""
    return [
        Diagnostic(
            Path(d["filename"]),
            f"{d['location']['row']}:{d['location']['column']}",
            f"{d['code']} {d['message']}",
        )
        for d in json.loads(out or "[]")
    ]


def pyright_command(env: Env, config: Path) -> list[str]:
    """Pyright for ``env``'s Python, under ``config``."""
    return [
        *[env.bin("pyright"), "--outputjson", f"--project={config}"],
        *[f"--pythonversion={env.python}", f"--pythonpath={env.bin('python')}"],
    ]


def mypy_command(env: Env, cache: Path | str) -> list[str]:
    """``mypy --strict`` for ``env``'s Python, under the contract."""
    return [
        *[env.bin("python"), "-m", "mypy", f"--config-file={CONFIG}", "--output=json"],
        *[f"--python-version={env.python}", f"--cache-dir={cache}"],
    ]


def pyright_findings(out: str) -> tuple[int, list[tuple[str, int, int, str]]]:
    """Pyright's report: files analysed, and each finding's file, 0-based
    line and column, and message.
    """
    report = json.loads(out or "{}")
    return report.get("summary", {}).get("filesAnalyzed", 0), [
        (
            d["file"],
            d["range"]["start"]["line"],
            d["range"]["start"]["character"],
            f"{d['message']} ({d.get('rule', d['severity'])})",
        )
        for d in report.get("generalDiagnostics", [])
    ]


def mypy_findings(out: str) -> list[tuple[str, int, int, str]]:
    """Mypy's findings, notes aside: file, 0-based line and column, message."""
    lines = [json.loads(line) for line in out.splitlines() if line.startswith("{")]
    return [
        (d["file"], d["line"] - 1, d["column"], f"{d['message']} [{d['code']}]")
        for d in lines
        if d["severity"] != "note"
    ]


def located(findings: list[tuple[str, int, int, str]]) -> list[Diagnostic]:
    """Findings at their 1-based line and column."""
    return [
        Diagnostic(Path(file).resolve(), f"{line + 1}:{column + 1}", message)
        for file, line, column, message in findings
    ]


def pyright(env: Env, roots: list[Path], files: int) -> Run:
    """Pyright strict: the stricter type checker, and the one editors run.

    Its command line cannot add import roots, so a configuration extending the
    contract does, like ``MYPYPATH`` for mypy. It must analyse all ``files``.
    """
    analyzed: list[int] = []

    def parse(out: str) -> list[Diagnostic]:
        count, findings = pyright_findings(out)
        analyzed.append(count)
        return located(findings)

    with tempfile.TemporaryDirectory() as tmp:
        config = Path(tmp, "pyrightconfig.json")
        paths = [str(root) for root in roots]
        config.write_text(json.dumps({"extends": str(CONFIG), "extraPaths": paths}))
        command = [*pyright_command(env, config), *paths]
        result = run(f"pyright {env.python}", command, parse)
    if analyzed != [files] and not result.error:
        error = f"analysed {analyzed[0]} of {files} files"
        return replace(result, error=error)
    return result


def mypy(env: Env, roots: list[Path]) -> Run:
    """``mypy --strict``: what most users of the generated code run."""
    # A module is named after its path below its root.
    command = [*mypy_command(env, env.path / ".mypy_cache"), "--explicit-package-bases"]
    return run(
        f"mypy {env.python}",
        [*command, *map(str, roots)],
        lambda out: located(mypy_findings(out)),
        MYPYPATH=os.pathsep.join(map(str, roots)),
    )


def as_installed(tool: str, env: Env, roots: list[Path], packages: list[Path]) -> Run:
    """``tool``, pyright or mypy, over an import of every package as installed:
    a copy marked typed, as the distribution it lives in is, found on
    ``PYTHONPATH``, which both read through the interpreter, as they read
    site-packages. A finding is the package's, at its ``__init__.py``.
    """

    def parse(out: str) -> list[Diagnostic]:
        found = pyright_findings(out)[1] if tool == "pyright" else mypy_findings(out)
        # Line N of the importer imports package N.
        return [Diagnostic(packages[n] / "__init__.py", "", m) for _, n, _, m in found]

    with tempfile.TemporaryDirectory() as tmp:
        site = Path(tmp, "site")
        for package in packages:
            # Only its sources: the import job may be writing bytecode beside them.
            shutil.copytree(
                package,
                site / package.name,
                ignore=shutil.ignore_patterns("__pycache__"),
            )
            (site / package.name / "py.typed").touch()
        importer = Path(tmp, "importer.py")
        # `as`, so that pyright does not report the import as unused.
        importer.write_text("".join(f"import {p.name} as {p.name}\n" for p in packages))
        config = Path(tmp, "pyrightconfig.json")
        config.write_text(json.dumps({"extends": str(CONFIG)}))
        command = (
            pyright_command(env, config)
            if tool == "pyright"
            else mypy_command(env, tmp)
        )
        return run(
            f"{tool} {env.python} installed",
            [*command, str(importer)],
            parse,
            PYTHONPATH=os.pathsep.join(map(str, [site, *roots])),
        )


def import_all(env: Env, root: Path) -> Run:
    """Import every module: annotations and class bodies run on import."""

    def parse(out: str) -> list[Diagnostic]:
        lines = map(json.loads, out.splitlines())
        return [Diagnostic(Path(d["path"]), "", d["message"]) for d in lines]

    command = [env.bin("python"), "-I", "-c", IMPORT_ALL, str(root)]
    return run(f"import {env.python}", command, parse)


def run(
    tool: str,
    command: list[str],
    parse: Callable[[str], list[Diagnostic]],
    **env: str,
) -> Run:
    """Run a tool to completion and parse what it reports."""
    start = time.monotonic()
    result = subprocess.run(
        command,
        capture_output=True,
        text=True,
        check=False,
        env=os.environ | env,
    )
    seconds = time.monotonic() - start
    diagnostics = parse(result.stdout)
    failed = result.returncode != 0 and not diagnostics
    error = (
        result.stderr or result.stdout or f"exit status {result.returncode}"
    ).strip()
    return Run(tool, seconds, diagnostics, error if failed else "")


def report(roots: list[Path], packages: int, runs: list[Run]) -> int:
    """Print the findings per package, then per rule; return the exit status."""
    by_package: defaultdict[str, list[tuple[str, str, str]]] = defaultdict(list)
    for run in runs:
        for d in run.diagnostics:
            path = d.path.resolve()
            # A diagnostic may point outside the roots, e.g. into a stub.
            root = next((root for root in roots if path.is_relative_to(root)), None)
            package, *file = path.relative_to(root).parts if root else (str(path),)
            where = "/".join(file) + (f":{d.where}" if d.where else "")
            by_package[package].append((run.tool, where, " ".join(d.message.split())))

    for package, findings in sorted(by_package.items()):
        print(package)
        for tool, where, message in sorted(findings):
            print(f"  {tool:<12} {where:<18} {message}")
    rules = Counter(
        (tool, rule(message))
        for findings in by_package.values()
        for tool, _, message in findings
    )
    if rules:
        print("\nby tool and rule:")
        for (tool, name), count in rules.most_common():
            print(f"  {count:>5}  {tool:<12} {name}")
    errors = [run for run in runs if run.error]
    for run in errors:
        print(f"\n{run.tool} failed:\n{run.error}")

    timings = ", ".join(f"{run.tool} {run.seconds:.1f}s" for run in runs)
    print(f"\ngate: {len(by_package)} of {packages} packages failed ({timings})")
    return 1 if by_package or errors else 0


def rule(message: str) -> str:
    """Name the rule a message cites: a ruff code, a ``(rule)`` or ``[code]``."""
    for opening, closing in ("()", "[]"):
        if message.endswith(closing) and opening in message:
            return message[message.rindex(opening) :]
    return message.split(" ", 1)[0]


if __name__ == "__main__":
    sys.exit(main())
