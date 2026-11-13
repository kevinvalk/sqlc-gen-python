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
  Python and the newest stable one.

Each Python has its own environment in ``target/py``, synced from ``uv.lock``,
so the type checkers see what that Python installs: SQLAlchemy 2.0 on 3.10,
2.1 later. Findings are collated per package into one report, and the exit
status is 1 if there are any, or if a root holds no package to check.
"""

from __future__ import annotations

import importlib.metadata
import json
import os
import subprocess
import sys
import tempfile
from collections import defaultdict
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
    """What one tool said."""

    tool: str
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
        command = [
            *[env.bin("pyright"), "--outputjson", f"--project={config}"],
            *[f"--pythonversion={env.python}", f"--pythonpath={env.bin('python')}"],
            *paths,
        ]
        result = run(f"pyright {env.python}", command, parse)
    if analyzed != [files] and not result.error:
        error = f"analysed {analyzed[0]} of {files} files"
        return replace(result, error=error)
    return result


def mypy(env: Env, roots: list[Path]) -> Run:
    """``mypy --strict``: what most users of the generated code run."""
    # A module is named after its path below its root.
    command = [
        *[env.bin("python"), "-m", "mypy", f"--config-file={CONFIG}", "--output=json"],
        *[f"--python-version={env.python}", f"--cache-dir={env.path / '.mypy_cache'}"],
        "--explicit-package-bases",
    ]
    return run(
        f"mypy {env.python}",
        [*command, *map(str, roots)],
        lambda out: located(mypy_findings(out)),
        MYPYPATH=os.pathsep.join(map(str, roots)),
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
    result = subprocess.run(
        command,
        capture_output=True,
        text=True,
        check=False,
        env=os.environ | env,
    )
    diagnostics = parse(result.stdout)
    failed = result.returncode != 0 and not diagnostics
    error = (
        result.stderr or result.stdout or f"exit status {result.returncode}"
    ).strip()
    return Run(tool, diagnostics, error if failed else "")


def report(roots: list[Path], packages: int, runs: list[Run]) -> int:
    """Print the findings per package; return the exit status."""
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
    errors = [run for run in runs if run.error]
    for run in errors:
        print(f"\n{run.tool} failed:\n{run.error}")

    print(f"\ngate: {len(by_package)} of {packages} packages failed")
    return 1 if by_package or errors else 0


if __name__ == "__main__":
    sys.exit(main())
