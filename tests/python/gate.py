"""The static gate: every generated file must pass ruff, pyright strict, mypy strict and a real import.

usage: gate.py ROOT...

Each ROOT holds one package per generated output (``ROOT/<package>/*.py``), and may hold modules beside them that use
those packages, such as the runtime suite's ``usage.py``. We check under each of ``PYTHONS`` in its own environment
synced from ``uv.lock``, so the type checkers see what that Python installs: SQLAlchemy 2.0 on 3.11, 2.1 later.
"""

from __future__ import annotations

import importlib.metadata
import json
import os
import subprocess
import sys
import tempfile
import tomllib
from collections import defaultdict
from concurrent.futures import ThreadPoolExecutor
from dataclasses import dataclass, replace
from functools import partial
from pathlib import Path
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from collections.abc import Callable

PROJECT = Path(__file__).resolve().parent
CONFIG = PROJECT / "pyproject.toml"

# The oldest supported Python, the project's `requires-python`, and the newest stable one.
PYTHONS = (tomllib.loads(CONFIG.read_text())["project"]["requires-python"].removeprefix(">="), "3.15")
RUFF_OPTIONS = ("--output-format=json", "--no-cache", f"--config={CONFIG}")

# We run this inside an environment, so it imports with warnings as errors and prints a JSON line per failing module.
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
class Environment:
    python: str

    @property
    def path(self) -> Path:
        return PROJECT.parents[1] / "target" / "py" / self.python

    def bin(self, name: str) -> str:
        return str(self.path / "bin" / name)

    def installed(self, package: str) -> str:
        site = [str(path) for path in self.path.glob("lib/python*/site-packages")]
        (found,) = importlib.metadata.distributions(name=package, path=site)
        return found.version


@dataclass(frozen=True)
class Diagnostic:
    path: Path
    where: str
    message: str


@dataclass(frozen=True)
class Run:
    tool: str
    diagnostics: list[Diagnostic]
    error: str  # What a tool that failed without a diagnostic printed instead


def main() -> int:
    roots = [Path(argument).resolve() for argument in sys.argv[1:]]
    if len(roots) <= 0:
        sys.exit("usage: gate.py ROOT...")
    for root in roots:
        if not any(root.glob("*/**/*.py")):
            sys.exit(f"gate: {root} holds no package to check")
    files = sum(1 for root in roots for _ in root.glob("**/*.py"))
    packages = [path for root in roots for path in sorted(root.iterdir()) if path.is_dir()]
    environments = [sync(Environment(python)) for python in PYTHONS]
    ruff = environments[0].bin("ruff")

    jobs: list[Callable[[], Run]] = [
        partial(ruff_format, ruff, roots),
        partial(ruff_check, ruff, roots),
    ]
    for environment in environments:
        jobs += [partial(pyright, environment, roots, files), partial(mypy, environment, roots)]
        jobs += [partial(import_all, environment, root) for root in roots]
    with ThreadPoolExecutor(len(jobs)) as pool:
        runs = [future.result() for future in [pool.submit(job) for job in jobs]]

    return report(roots, len(packages), runs)


def sync(environment: Environment) -> Environment:
    subprocess.run(
        [
            "uv",
            "sync",
            "--quiet",
            "--locked",
            "--managed-python",
            f"--python={environment.python}",
        ],
        cwd=PROJECT,
        env=os.environ | {"UV_PROJECT_ENVIRONMENT": str(environment.path)},
        check=True,
    )
    return environment


def ruff_format(ruff: str, roots: list[Path]) -> Run:
    command = [ruff, "format", "--check", *RUFF_OPTIONS, *map(str, roots)]
    return run("ruff format", command, parse_ruff)


def ruff_check(ruff: str, roots: list[Path]) -> Run:
    # NOTE: Generated code may hold exactly one directive, a file-wide noqa for S105 (the Rust invariants allow no
    # other), and ruff check obeys it.
    command = [ruff, "check", *RUFF_OPTIONS, *map(str, roots)]
    return run("ruff check", command, parse_ruff)


def parse_ruff(output: str) -> list[Diagnostic]:
    return [
        Diagnostic(
            Path(d["filename"]),
            f"{d['location']['row']}:{d['location']['column']}",
            f"{d['code']} {d['message']}",
        )
        for d in json.loads(output or "[]")
    ]


def parse_pyright(output: str) -> tuple[int, list[Diagnostic]]:
    report = json.loads(output or "{}")
    return report.get("summary", {}).get("filesAnalyzed", 0), [
        Diagnostic(
            Path(d["file"]).resolve(),
            # Pyright counts lines and columns from 0.
            f"{d['range']['start']['line'] + 1}:{d['range']['start']['character'] + 1}",
            f"{d['message']} ({d.get('rule', d['severity'])})",
        )
        for d in report.get("generalDiagnostics", [])
    ]


def parse_mypy(output: str) -> list[Diagnostic]:
    lines = [json.loads(line) for line in output.splitlines() if line.startswith("{")]
    return [
        Diagnostic(
            Path(d["file"]).resolve(),
            # Mypy counts lines from 1 and columns from 0.
            f"{d['line']}:{d['column'] + 1}",
            f"{d['message']} [{d['code']}]",
        )
        for d in lines
        if d["severity"] != "note"
    ]


def pyright(environment: Environment, roots: list[Path], files: int) -> Run:
    """Pyright strict, the stricter type checker and the one editors run, which must analyse all ``files``."""
    analyzed: list[int] = []

    def parse(output: str) -> list[Diagnostic]:
        count, findings = parse_pyright(output)
        analyzed.append(count)
        return findings

    # Pyright's command line cannot add import roots (what ``MYPYPATH`` does for mypy), so we write a configuration that
    # extends the contract and adds them.
    with tempfile.TemporaryDirectory() as directory:
        config = Path(directory, "pyrightconfig.json")
        paths = [str(root) for root in roots]
        config.write_text(json.dumps({"extends": str(CONFIG), "extraPaths": paths}))
        command = [
            *[environment.bin("pyright"), "--outputjson", f"--project={config}"],
            *[f"--pythonversion={environment.python}", f"--pythonpath={environment.bin('python')}"],
            *paths,
        ]
        result = run(f"pyright {environment.python}", command, parse)
    if analyzed != [files] and result.error == "":
        error = f"analysed {analyzed[0]} of {files} files"
        return replace(result, error=error)
    return result


def mypy(environment: Environment, roots: list[Path]) -> Run:
    # We name a module after its path below its root, hence the explicit package bases.
    command = [
        *[environment.bin("python"), "-m", "mypy", f"--config-file={CONFIG}", "--output=json"],
        *[f"--python-version={environment.python}", f"--cache-dir={environment.path / '.mypy_cache'}"],
        "--explicit-package-bases",
    ]
    return run(
        f"mypy {environment.python}",
        [*command, *map(str, roots)],
        parse_mypy,
        MYPYPATH=os.pathsep.join(map(str, roots)),
    )


def import_all(environment: Environment, root: Path) -> Run:
    """Import every module, as annotations and class bodies only run on import."""

    def parse(output: str) -> list[Diagnostic]:
        lines = map(json.loads, output.splitlines())
        return [Diagnostic(Path(d["path"]), "", d["message"]) for d in lines]

    command = [environment.bin("python"), "-I", "-c", IMPORT_ALL, str(root)]
    return run(f"import {environment.python}", command, parse)


def run(
    tool: str,
    command: list[str],
    parse: Callable[[str], list[Diagnostic]],
    **environment: str,
) -> Run:
    result = subprocess.run(
        command,
        capture_output=True,
        text=True,
        check=False,
        env=os.environ | environment,
    )
    diagnostics = parse(result.stdout)
    failed = result.returncode != 0 and len(diagnostics) <= 0
    error = (result.stderr or result.stdout or f"exit status {result.returncode}").strip()
    return Run(tool, diagnostics, error if failed else "")


def report(roots: list[Path], packages: int, runs: list[Run]) -> int:
    by_package: defaultdict[str, list[tuple[str, str, str]]] = defaultdict(list)
    for run in runs:
        for diagnostic in run.diagnostics:
            path = diagnostic.path.resolve()

            # A diagnostic may point outside the roots, into a stub for example.
            root = next((root for root in roots if path.is_relative_to(root)), None)
            package, *file = path.relative_to(root).parts if root is not None else (str(path),)
            where = "/".join(file) + (f":{diagnostic.where}" if diagnostic.where != "" else "")
            by_package[package].append((run.tool, where, " ".join(diagnostic.message.split())))

    for package, findings in sorted(by_package.items()):
        print(package)
        for tool, where, message in sorted(findings):
            print(f"  {tool:<12} {where:<18} {message}")
    errors = [run for run in runs if run.error != ""]
    for run in errors:
        print(f"\n{run.tool} failed:\n{run.error}")

    print(f"\ngate: {len(by_package)} of {packages} packages failed")
    return 1 if len(by_package) > 0 or len(errors) > 0 else 0


if __name__ == "__main__":
    sys.exit(main())
