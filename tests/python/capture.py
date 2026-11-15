"""Capture the requests sqlc sends a plugin, as committed test input.

usage: cargo xtask capture [--sqlc-bin PATH] [--check]

We run every case of sqlc's own end-to-end suite (``internal/endtoend/testdata`` at the release tag ``.prototools``
pins) and every example under ``examples/`` through sqlc's built-in ``json`` codegen, which dumps the exact
``GenerateRequest`` a plugin gets. A case runs the way sqlc's ``TestReplay`` runs it: once per context it allows,
``base`` (static analysis) and ``managed-db`` (analysed against a live PostgreSQL). Each request is reduced to what a
wasm plugin sees, minus what a test case supplies itself, and written to:

- ``tests/sqlc/endtoend/<case>/request[.managed-db][.<n>].json``: one file per distinct request, at the first case (in
  path order) that produced it. The tree mirrors sqlc's testdata, so a request sits at the path of its Go output.
- ``tests/sqlc/endtoend/<case>/schema[.<n>].sql`` beside it: the schema sqlc read for that package as one script a
  database can run, so the runtime sweep can build the tables a request describes.
- ``tests/sqlc/endtoend/MANIFEST.tsv``: every case and context, and what became of it.
- ``tests/sqlc/endtoend/SOURCE.toml`` and ``NOTICE``: provenance, and sqlc's MIT license.
- ``tests/sqlc/endtoend/inflection.tsv``: sqlc's own singular (``internal/inflection``, which needs Go) of every table
  name in these requests and of ``WORDS``.
- ``tests/sqlc/examples/<example>.json``: what sqlc sends for each example, which the golden tests generate from.
- ``protos/sqlc.proto``: re-vendored from the same tag, so the wire types always match the requests.

With ``--check`` we write under ``target/capture/<version>/check`` instead and compare byte for byte with what is
committed, then hold the WASM plugin to sqlc's wire contract (``wire_contract``). Nothing is written until everything
that can fail has run.

SECURITY: The testdata is untrusted input. We rewrite each config to keep only analysis settings, which leaves sqlc
nothing to run but itself: no plugins, no vet.
"""

from __future__ import annotations

import argparse
import hashlib
import io
import json
import os
import platform
import re
import shutil
import signal
import subprocess
import sys
import tarfile
import time
import urllib.request
from collections import Counter
from concurrent.futures import ThreadPoolExecutor
from contextlib import ExitStack
from dataclasses import dataclass
from functools import partial
from itertools import groupby
from pathlib import Path
from queue import Queue
from typing import TYPE_CHECKING, Any

import yaml

import postgres
from examples import EXAMPLES, ROOT, golden_cases

if TYPE_CHECKING:
    from collections.abc import Iterable, Iterator

CORPUS = "tests/sqlc/endtoend"
REQUESTS = "tests/sqlc/examples"
# Everything a capture writes, relative to where it writes
CAPTURED = (
    f"{CORPUS}/**/request*.json",
    f"{CORPUS}/**/schema*.sql",
    f"{CORPUS}/MANIFEST.tsv",
    f"{CORPUS}/SOURCE.toml",
    f"{CORPUS}/NOTICE",
    f"{CORPUS}/inflection.tsv",
    f"{REQUESTS}/*.json",
    "protos/sqlc.proto",
)
WASM = "target/wasm32-wasip1/release/sqlc-gen-python.wasm"
# We take it out of the environment, as only `fetch` needs it and not what this script runs (sqlc over test cases it
# does not trust, go, docker).
GITHUB_TOKEN = os.environ.pop("GITHUB_TOKEN", None)
REPOSITORY = "sqlc-dev/sqlc"
TESTDATA = "internal/endtoend/testdata"
PROTO = "protos/plugin/codegen.proto"
INFLECTION = "internal/inflection"
# Words beyond the corpus's table names that exercise each rule of sqlc's singularisation, and each correction sqlc
# makes to it.
WORDS = (
    "addresses",
    "alias",
    "aliases",
    "analyses",
    "articles",
    "axes",
    "boxes",
    "bus",
    "buses",
    "business",
    "businesses",
    "calories",
    "campus",
    "categories",
    "children",
    "crises",
    "criteria",
    "data",
    "fish",
    "geese",
    "hero",
    "heroes",
    "indices",
    "kpis",
    "knives",
    "leaves",
    "lives",
    "matrices",
    "media",
    "meta",
    "metadata",
    "mice",
    "moves",
    "news",
    "octopi",
    "oxen",
    "potatoes",
    "quizzes",
    "series",
    "sheep",
    "species",
    "statuses",
    "vertices",
    "viri",
    "waves",
    "wolves",
)
# The server sqlc's own test suite starts, pinned to the minor version as the managed-db requests depend on its catalog.
# As in sqlc's tests, `POSTGRESQL_SERVER_URI` names a server to use instead, which must be this version too.
POSTGRES = "16.15"

# Managed-db runs share a server only one at a time, as sqlc names the database it creates after the schema and
# concurrent sqlc processes would race on it.
SERVERS = 4

CONFIGS = ("sqlc.json", "sqlc.yaml", "sqlc.yml")
CONTEXTS = ("base", "managed-db")
# The commands of sqlc's tests that compile a case's queries, as `generate` does. Any other (`parse`, `analyze`, `fmt`
# after v1.31.1) reads SQL that need not compile, and sends a plugin nothing.
COMPILES = ("generate", "diff", "vet")
# The settings that decide a request. The rest of a config (Go options, plugins, vet rules, cloud) never reaches a
# plugin's input.
ANALYSIS = (
    "engine",
    "schema",
    "queries",
    "database",
    "analyzer",
    "strict_function_checks",
    "strict_order_by",
)
# sqlc drops these before any wasm plugin runs: `removePGCatalog`, internal/ext/wasm/wasm.go.
WASM_STRIPPED = ("pg_catalog", "information_schema")

Json = dict[str, Any]


@dataclass(frozen=True)
class Run:
    """One ``sqlc generate`` of one case in one context."""

    case: str
    context: str
    command: str
    directory: Path
    config: Json
    environment: dict[str, str]
    expected_error: str | None


@dataclass(frozen=True)
class Outcome:
    run: Run
    # "captured" requests, the "error" sqlc's test expects, an unexpected "failure", or "skipped" for a context sqlc's
    # tests do not have or a command that compiles nothing
    status: str
    requests: tuple[Json, ...] = ()
    detail: str = ""


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--sqlc-bin", type=Path, help="use this sqlc binary")
    parser.add_argument("--check", action="store_true", help="fail if the capture differs")
    args = parser.parse_args()
    version = f"v{pinned_sqlc().removeprefix('v')}"
    work = ROOT / "target/capture" / version
    out = work / "check" if args.check else ROOT
    shutil.rmtree(work / "check", ignore_errors=True)
    started = time.monotonic()
    signal.signal(signal.SIGTERM, signal.default_int_handler)  # So a termination still cleans up

    require_tools()
    sqlc = sqlc_binary(args.sqlc_bin, version, work)
    print(f"checking out sqlc {version}'s test cases", flush=True)
    source, commit = clone_sqlc(version, work)

    upstream = copy_tree(source / TESTDATA, work / "run/upstream")
    local = copy_tree(EXAMPLES, work / "run/local")
    runs = [*upstream_runs(upstream), *local_runs(local)]
    with ExitStack() as stack, ThreadPoolExecutor() as pool:
        servers = postgres_servers(stack)
        print(f"{len(runs)} sqlc runs on {servers.qsize()} servers", flush=True)
        try:
            outcomes = list(pool.map(partial(generate, sqlc, servers), runs))
        except BaseException:
            pool.shutdown(cancel_futures=True)
            raise

    failed = [o for o in outcomes if o.status == "failure"]
    for outcome in failed:
        print(f"{outcome.run.case} [{outcome.run.context}]: {outcome.detail}")
    if len(failed) > 0:
        sys.exit(f"{len(failed)} runs did not end the way sqlc's own test expects")

    inflection = inflect(source, outcomes)

    print(f"writing to {out}", flush=True)
    vendor_proto(source, version, out)
    for outcome in (o for o in outcomes if o.run.directory.is_relative_to(local)):
        (request,) = outcome.requests
        write_json(out / REQUESTS / f"{outcome.run.case}.json", request)
    corpus = [o for o in outcomes if o.run.directory.is_relative_to(upstream)]
    write_corpus(corpus, out / CORPUS)
    (out / CORPUS / "inflection.tsv").write_text(inflection)
    (out / CORPUS / "SOURCE.toml").write_text(
        "# Written by `cargo xtask capture`. Do not edit.\n"
        f'sqlc = "{version}"\n'
        f'commit = "{commit}"\n'
        f'testdata = "https://github.com/{REPOSITORY}/tree/{commit}/{TESTDATA}"\n'
        f'managed_db = "PostgreSQL {POSTGRES}"\n'
    )
    (out / CORPUS / "NOTICE").write_text(
        f"The requests in this directory are the output of sqlc {version} run over\n"
        f"its own end-to-end test cases ({TESTDATA} at commit\n"
        f"{commit}), which are distributed under this license:\n\n" + (source / "LICENSE").read_text()
    )
    summarize(corpus, out / CORPUS, time.monotonic() - started)
    if args.check:
        if len(differences := compare(out, ROOT)) > 0:
            print("\n".join(differences))
            sys.exit(f"the capture differs from what is committed; see {out}")
        wire_contract(sqlc)


def wire_contract(sqlc: Path) -> None:
    """Run the WASM plugin under sqlc, in scratch copies of the examples, and hold it to what sqlc does with its answers
    (AGENTS.md, source 1).
    """
    print("holding the WASM plugin to sqlc's wire contract", flush=True)
    run(os.environ.get("CARGO", "cargo"), "wasm", cwd=ROOT)
    wire = Wire(sqlc, ROOT / "target/capture/wire")

    # In each example, `sqlc generate` with its `process` plugin replaced by the WASM writes exactly its golden cases
    # (every codegen `out`, byte for byte) and nothing outside them.
    for example, cases in groupby(golden_cases(), key=lambda case: case.example):
        if (result := wire.generate(wire.example(example))).returncode != 0:
            sys.exit(f"sqlc failed in examples/{example}:\n{result.stderr}")
        for case in cases:
            same_output(wire.work / case.name, case.dir)

    # No options (so empty `plugin_options` bytes, which name no backend), no `async`, an unknown option, an unsupported
    # feature, and a backend that does not run the engine must each fail sqlc (exit 1) and write nothing. The plugin's
    # message must be on stderr, which sqlc shows only on failure.
    invoicing = wire.example("invoicing")
    wire.fails(invoicing, {}, "reading plugin options", "missing field `backend`")
    wire.fails(
        invoicing,
        {"backend": "sqlalchemy"},
        "reading plugin options",
        "missing field `async`",
    )
    backend = {"backend": "sqlalchemy", "async": False}
    # A typo in an option is an error, never a silent no-op.
    wire.fails(invoicing, backend | {"overides": []}, "reading plugin options", "`overides`")
    (wire.work / "query/unsupported.sql").write_text("-- name: Touch :execlastid\nUPDATE customers SET name = name;\n")
    wire.fails(
        invoicing,
        backend,
        'unsupported.sql: query "Touch"',
        ":execlastid is not supported",
    )
    wire.fails(
        wire.example("billing"),
        {"backend": "psycopg", "async": False},
        "the psycopg backend does not run mysql",
    )


def outs(config: Json) -> list[str]:
    return [codegen["out"] for package in config["sql"] for codegen in package.get("codegen", [])]


class Wire:
    """sqlc running the WASM plugin in the scratch directory ``work``."""

    def __init__(self, sqlc: Path, work: Path) -> None:
        self.sqlc, self.work = sqlc, work
        wasm = ROOT / WASM
        digest = hashlib.sha256(wasm.read_bytes()).hexdigest()
        self.plugin = {
            "name": "python",
            "wasm": {"url": wasm.as_uri(), "sha256": digest},
        }

    def example(self, name: str) -> Json:
        """Copy example ``name``'s SQL to ``work`` and return its config, switched to the WASM."""
        source = EXAMPLES / name
        config: Json = yaml.safe_load((source / "sqlc.yaml").read_text())
        shutil.rmtree(self.work, ignore_errors=True)
        ignore = shutil.ignore_patterns("sqlc.yaml", *outs(config))
        shutil.copytree(source, self.work, ignore=ignore)
        return config | {"plugins": [self.plugin]}

    def generate(self, config: Json) -> subprocess.CompletedProcess[str]:
        """Run sqlc with ``config``, and exit when it wrote outside its ``out`` directories."""
        targets = [self.work / out for out in outs(config)]
        for target in targets:
            shutil.rmtree(target, ignore_errors=True)
        (self.work / "sqlc.yaml").write_text(yaml.safe_dump(config))
        before = set(self.work.rglob("*"))
        result = subprocess.run(
            [self.sqlc, "generate"],
            cwd=self.work,
            capture_output=True,
            text=True,
            check=False,
        )
        written = set(self.work.rglob("*")) - before
        if len(stray := [path for path in written if not any(map(path.is_relative_to, targets))]) > 0:
            sys.exit(f"sqlc wrote outside the codegen `out`s: {stray}")
        return result

    def fails(self, config: Json, options: Json, *expected: str) -> None:
        """Exit unless one codegen entry of ``options`` fails as ``expected``."""
        (package,) = config["sql"]

        # We leave out `options` when they are `{}`, which sqlc then sends as empty bytes.
        codegen: Json = {"out": "out", "plugin": "python"}
        codegen |= {"options": options} if len(options) > 0 else {}

        result = self.generate(config | {"sql": [package | {"codegen": [codegen]}]})
        missing = [text for text in expected if text not in result.stderr]
        if result.returncode != 1 or len(missing) > 0 or (self.work / "out").exists():
            sys.exit(
                f"sqlc with {options} must exit 1, write nothing and show"
                f" {missing} on stderr; it exited {result.returncode}:\n{result.stderr}"
            )


def same_output(written: Path, golden: Path) -> None:
    def output(directory: Path) -> dict[str, bytes]:
        files = [path for path in directory.iterdir() if path.is_file() and path.name[0] != "."]
        return {file.name: file.read_bytes() for file in files}

    found, expected = output(written), output(golden)
    names = found.keys() | expected.keys()
    if len(differing := sorted(name for name in names if found.get(name) != expected.get(name))) > 0:
        where = f"{written} differs from {golden.relative_to(ROOT)}"
        sys.exit(f"{where}: {', '.join(differing)}")


def run(*command: str | Path, cwd: Path | None = None, stdin: str | None = None) -> str:
    try:
        return subprocess.run(command, cwd=cwd, input=stdin, check=True, capture_output=True, text=True).stdout
    except subprocess.CalledProcessError as error:
        line = " ".join(map(str, command))
        sys.exit(f"`{line}` failed ({error.returncode}):\n{error.stderr.strip()}")


def require_tools() -> None:
    needed = ["git", "go"]
    needed += [] if os.environ.get("POSTGRESQL_SERVER_URI") else ["docker"]
    if len(missing := [tool for tool in needed if shutil.which(tool) is None]) > 0:
        sys.exit(f"the capture needs {', '.join(missing)} on PATH")


def pinned_sqlc() -> str:
    prototools = (ROOT / ".prototools").read_text()
    match = re.search(r'^sqlc = "(.+)"$', prototools, re.MULTILINE)
    if match is None:
        sys.exit(".prototools pins no sqlc version")
    return match[1]


def sqlc_binary(given: Path | None, version: str, work: Path) -> Path:
    """Return ``given``, or else the release binary, as long as it is sqlc ``version``."""
    if given is not None and not os.access(given, os.X_OK):
        sys.exit(f"--sqlc-bin {given} is not an executable file")
    sqlc = given if given is not None else download_sqlc(version, work)
    if (found := run(sqlc, "version").strip()) != version:
        sys.exit(f"{sqlc} is sqlc {found}, not {version}")
    return sqlc


def download_sqlc(version: str, work: Path) -> Path:
    """Extract the release binary for this host. We keep the archive but check it against its digest on every run, so
    what runs is always what was published.
    """
    architectures = {"x86_64": "amd64", "amd64": "amd64", "arm64": "arm64", "aarch64": "arm64"}
    host = f"{sys.platform}_{architectures[platform.machine().lower()]}"
    name = f"sqlc_{version[1:]}_{host}.tar.gz"
    release = json.loads(fetch(f"api.github.com/repos/{REPOSITORY}/releases/tags/{version}"))
    (digest,) = (asset["digest"] for asset in release["assets"] if asset["name"] == name)
    cached = work / name
    print(f"fetching {name}, checked against its digest", flush=True)
    archive = cached.read_bytes() if cached.is_file() else b""
    if f"sha256:{hashlib.sha256(archive).hexdigest()}" != digest:
        archive = fetch(f"github.com/{REPOSITORY}/releases/download/{version}/{name}")
    if f"sha256:{hashlib.sha256(archive).hexdigest()}" != digest:
        sys.exit(f"{name} does not match its published digest {digest}")
    cached.parent.mkdir(parents=True, exist_ok=True)
    cached.write_bytes(archive)

    binary = work / "bin/sqlc"

    with tarfile.open(fileobj=io.BytesIO(archive)) as tar:
        member = tar.extractfile("sqlc")
        if member is None:
            sys.exit(f"{name} holds no sqlc binary")
        binary.parent.mkdir(parents=True, exist_ok=True)
        binary.write_bytes(member.read())
    binary.chmod(0o755)
    return binary


def fetch(url: str) -> bytes:
    """Return the body behind ``https://<url>``."""
    # Unauthenticated, GitHub's API allows a CI runner's shared address too few requests.
    is_api = GITHUB_TOKEN is not None and url.startswith("api.github.com/")
    headers = {"Authorization": f"Bearer {GITHUB_TOKEN}"} if is_api else {}
    request = urllib.request.Request(f"https://{url}", headers=headers)

    # The timeout makes a download that stalls fail the run, rather than hold it.
    with urllib.request.urlopen(request, timeout=120) as response:  # noqa: S310 -- always https
        body: bytes = response.read()
        return body


def clone_sqlc(version: str, work: Path) -> tuple[Path, str]:
    """Check out just what we read of sqlc's source at the tag, and return its commit."""
    source = work / "sqlc"
    git = ("git", "-C", str(source))

    # We reuse a checkout kept from an earlier run only while it is still the tag, unmodified.
    if source.is_dir() and (
        run(*git, "status", "--porcelain") != ""
        or run(*git, "rev-parse", "HEAD") != run(*git, "rev-parse", f"refs/tags/{version}^{{commit}}")
    ):
        shutil.rmtree(source)
    if not source.is_dir():
        checkout = work / "sqlc.partial"
        shutil.rmtree(checkout, ignore_errors=True)
        url = f"https://github.com/{REPOSITORY}.git"
        run("git", "clone", "--quiet", "--depth=1", "--filter=blob:none",
               "--sparse", f"--branch={version}", url, str(checkout))  # fmt: skip
        checkout.rename(source)

    # We always set what we read, as a checkout kept from before may be missing a part of it.
    run(*git, "sparse-checkout", "set", TESTDATA, INFLECTION,
                 str(Path(PROTO).parent))  # fmt: skip
    return source, run(*git, "rev-parse", "HEAD").strip()


def inflect(source: Path, outcomes: Iterable[Outcome]) -> str:
    """Return ``name<TAB>singular`` lines for every table name and ``WORDS``, singularised by sqlc's own Go code."""
    names = set(WORDS) | {
        table["rel"]["name"]
        for outcome in outcomes
        for request in outcome.requests
        for schema in request["catalog"]["schemas"]
        for table in schema.get("tables", [])
    }
    print(f"singularising {len(names)} names with sqlc's Go", flush=True)
    # We put the program inside sqlc's module, as that is the only place its internal package can be used.
    program = source / INFLECTION / "capture/main.go"
    program.parent.mkdir()
    program.write_text(INFLECT_GO)
    try:
        lines = "".join(f"{name}\n" for name in sorted(names))
        return run("go", "run", f"./{INFLECTION}/capture", cwd=source, stdin=lines)
    finally:
        shutil.rmtree(program.parent)


INFLECT_GO = r"""package main

import (
	"bufio"
	"fmt"
	"os"

	"github.com/sqlc-dev/sqlc/internal/inflection"
)

func main() {
	lines := bufio.NewScanner(os.Stdin)
	for lines.Scan() {
		name := lines.Text()
		fmt.Printf("%s\t%s\n", name, inflection.Singular(inflection.SingularParams{Name: name}))
	}
}
"""


def vendor_proto(source: Path, version: str, out: Path) -> None:
    url = f"https://github.com/{REPOSITORY}/blob/{version}/{PROTO}"
    header = f"// Copied verbatim from {url} (sqlc release {version})\n"
    (out / "protos").mkdir(parents=True, exist_ok=True)
    (out / "protos/sqlc.proto").write_text(header + (source / PROTO).read_text())


def copy_tree(source: Path, destination: Path) -> Path:
    # We run cases in a copy of the whole tree, as sqlc resolves paths like `../schema.sql`.
    shutil.rmtree(destination, ignore_errors=True)
    ignore = shutil.ignore_patterns("*.go", "go.mod", "go.sum", "gen")
    shutil.copytree(source, destination, ignore=ignore)
    return destination


def postgres_servers(stack: ExitStack) -> Queue[str]:
    """Start the PostgreSQL servers the managed-db context needs. We fail without Docker or ``POSTGRESQL_SERVER_URI``,
    or when a server is not the pinned version, rather than capture half the corpus.
    """
    servers: Queue[str] = Queue()

    # This is a set, so the one URL that `POSTGRESQL_SERVER_URI` names is never shared.
    urls = {
        stack.enter_context(postgres.server(f"postgres:{POSTGRES}", "POSTGRESQL_SERVER_URI")) for _ in range(SERVERS)
    }
    for url in urls:
        if (found := postgres.version(url)) != POSTGRES:
            sys.exit(f"the managed-db server is PostgreSQL {found}, not {POSTGRES}")
        # sqlc takes a plain libpq URL.
        plain = url.set(drivername="postgresql")
        servers.put(plain.render_as_string(hide_password=False))
    return servers


def upstream_runs(root: Path) -> Iterator[Run]:
    """Find cases as sqlc's ``FindTests`` does, and run them as ``TestReplay`` does. A case is the first directory on a
    path that holds a config, and it runs in each context its ``exec.json`` allows, with that file's environment.
    """
    for directory, children, files in os.walk(root):
        children.sort()
        config = next((name for name in CONFIGS if name in files), None)
        if config is None:
            continue
        children.clear()
        case = Path(directory)
        settings: Json = json.loads((case / "exec.json").read_text()) if "exec.json" in files else {}
        for context in settings.get("contexts", CONTEXTS):
            yield Run(
                case=case.relative_to(root).as_posix(),
                context=context,
                command=settings.get("command", "generate"),
                directory=case,
                config=analysis_only(case / config),
                environment=settings.get("env", {}),
                expected_error=expected_error(case, context),
            )


def local_runs(root: Path) -> Iterator[Run]:
    # Statically only, as an example's codegen entries are the golden tests'.
    for example in sorted(path.parent for path in root.glob("*/sqlc.yaml")):
        config = analysis_only(example / "sqlc.yaml")
        yield Run(example.name, "base", "generate", example, config, {}, None)


def analysis_only(path: Path) -> Json:
    """Reduce a config to what decides the requests, as config version 2."""
    text = path.read_text()
    config: Json = json.loads(text) if path.suffix == ".json" else yaml.safe_load(text)
    packages = config["packages"] if str(config["version"]) == "1" else config["sql"]
    sql = [
        {"engine": "postgresql"}  # The version 1 default
        | {key: package[key] for key in ANALYSIS if package.get(key) is not None}
        for package in packages
    ]
    servers = {"servers": config["servers"]} if config.get("servers") else {}
    return {"version": "2", "sql": sql, **servers}


def expected_error(case: Path, context: str) -> str | None:
    """Return the stderr sqlc's test expects, found the way ``parseStderr`` does."""
    for name in (f"stderr/{context}.txt", f"stderr_{sys.platform}.txt", "stderr.txt"):
        if (case / name).is_file():
            return comparable((case / name).read_text())
    return None


def generate(sqlc: Path, servers: Queue[str], run: Run) -> Outcome:
    """Run sqlc's ``json`` codegen, which writes the request a plugin would get."""
    if run.context not in CONTEXTS:
        return Outcome(run, "skipped", detail="sqlc's tests have no such context")
    if run.command not in COMPILES:
        return Outcome(run, "skipped", detail="sqlc's test compiles no queries")
    if run.context == "base":
        return generate_with(sqlc, run, run.config)
    server = servers.get()
    try:
        # As in sqlc's managed-db test context, every package is analysed on a server.
        sql = [package | {"database": {"managed": True}} for package in run.config["sql"]]
        database = {"name": "postgres", "engine": "postgresql", "uri": server}
        return generate_with(sqlc, run, run.config | {"servers": [database], "sql": sql})
    finally:
        servers.put(server)


def generate_with(sqlc: Path, run: Run, config: Json) -> Outcome:
    """Run sqlc with ``config``, written beside the original config it came from."""
    out = f"_capture/{run.context}"
    sql = [
        package | {"gen": {"json": {"out": out, "filename": f"{index}.json"}}}
        for index, package in enumerate(config["sql"])
    ]
    path = f"sqlc.capture.{run.context}.json"
    (run.directory / path).write_text(json.dumps(config | {"sql": sql}))
    result = subprocess.run(
        [sqlc, "generate", "--file", path],
        cwd=run.directory,
        env={**os.environ, "SQLCCACHE": str(run.directory / out / "cache"), **run.environment},
        capture_output=True,
        text=True,
        check=False,
    )
    if result.returncode != 0:
        error = comparable(result.stderr)
        status = "error" if error == run.expected_error else "failure"
        return Outcome(run, status, detail=one_line(error))
    dumps = [run.directory / out / f"{index}.json" for index in range(len(sql))]
    requests = tuple(plugin_view(json.loads(dump.read_text())) for dump in dumps)
    shutil.rmtree(run.directory / out)  # Each dump carries ~1.5 MB of pg_catalog
    return Outcome(run, "captured", requests)


def plugin_view(request: Json) -> Json:
    """Reduce a request to what a wasm plugin gets and a test case cannot supply. Options and the output directory
    belong to a case, and the paths of the source files mean nothing without them.
    """
    catalog = request["catalog"]
    schemas = [schema for schema in catalog["schemas"] if schema["name"] not in WASM_STRIPPED]
    return {
        "settings": {"engine": request["settings"]["engine"]},
        "catalog": catalog | {"schemas": schemas},
        "queries": request["queries"],
        "sqlc_version": request["sqlc_version"],
    }


def write_corpus(outcomes: Iterable[Outcome], corpus: Path) -> None:
    for stale in [*corpus.glob("**/request*.json"), *corpus.glob("**/schema*.sql")]:
        stale.unlink()
    for directory in sorted(corpus.glob("**/"), reverse=True):
        if not any(directory.iterdir()):
            directory.rmdir()

    stored: dict[str, str] = {}  # Request to where it is stored
    rows = ["case\tcontext\trequest\tnote"]
    for outcome in sorted(outcomes, key=lambda o: (o.run.case, o.run.context)):
        run, requests = outcome.run, outcome.requests
        if outcome.status != "captured":
            rows.append(f"{run.case}\t{run.context}\t-\t{outcome.status}: {outcome.detail}")
        expected = run.expected_error
        note = f"sqlc expects: {one_line(expected)}" if expected is not None else ""
        for index, request in enumerate(requests):
            context = "" if run.context == "base" else f".{run.context}"
            package = f".{index}" if len(requests) > 1 else ""
            if (key := json.dumps(request)) not in stored:
                stored[key] = f"{run.case}/request{context}{package}.json"
                write_json(corpus / stored[key], request)
                schema = corpus / run.case / f"schema{package}.sql"
                schema.write_text(schema_script(run, index))
            rows.append(f"{run.case}\t{run.context}\t{stored[key]}\t{note}")
    corpus.mkdir(parents=True, exist_ok=True)
    (corpus / "MANIFEST.tsv").write_text("\n".join(rows) + "\n")


def schema_script(run: Run, package: int) -> str:
    """Return the schema sqlc read for one package of a run, as one script. Its files come in sqlc's order
    (``sqlpath.Glob``), each without what sqlc does not read (``internal/migrations``) and ended by a ``;`` of its own.
    """
    patterns = run.config["sql"][package]["schema"]
    paths: list[Path] = []
    for pattern in [patterns] if isinstance(patterns, str) else patterns:
        if any(c in pattern for c in "*?[]"):
            paths += sorted(run.directory.glob(pattern))
        else:
            paths.append(run.directory / pattern)
    files = [
        file
        for path in paths
        for file in (sorted(path.iterdir()) if path.is_dir() else [path])
        if file.suffix == ".sql" and not file.name.startswith(".") and not file.name.endswith(".down.sql")
    ]
    return "".join(
        f"-- {Path(os.path.relpath(file, run.directory)).as_posix()}\n{runnable(file.read_text())}\n;\n"
        for file in files
    )


# Where sqlc stops reading a migration of goose, sql-migrate, tern and dbmate
ROLLBACK = (
    "-- +goose down",
    "-- +migrate down",
    "---- create above / drop below ----",
    "-- migrate:down",
)
PSQL_META = re.compile(r"^\\[A-Za-z!?;][^\n]*$")


def runnable(sql: str) -> str:
    """Cut what sqlc does not read: a migration's rollback half and psql meta-commands."""
    lines: list[str] = []
    for line in sql.splitlines():
        if line.lower().startswith(ROLLBACK):
            break
        if not PSQL_META.match(line):
            lines.append(line)
    return "\n".join(lines)


def write_json(path: Path, value: Json) -> None:
    # One stable layout that keeps sqlc's order of everything
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, indent=2, ensure_ascii=False) + "\n")


def summarize(outcomes: list[Outcome], corpus: Path, seconds: float) -> None:
    """Print what a reviewer of the capture diff wants to know first."""
    requests = list(corpus.glob("**/request*.json"))
    size = sum(path.stat().st_size for path in requests)
    engines = Counter(json.loads(path.read_text())["settings"]["engine"] for path in requests)
    runs = Counter(f"{o.run.context} {o.status}" for o in outcomes)
    print(f"{len(requests)} distinct requests, {size / 1e6:.1f} MB, in {seconds:.0f}s")
    print("by engine:", ", ".join(f"{engine} {count}" for engine, count in sorted(engines.items())))
    print("runs:", ", ".join(f"{outcome} {count}" for outcome, count in sorted(runs.items())))
    skipped = Counter(o.detail for o in outcomes if o.status == "skipped")
    for reason, count in skipped.items():
        print(f"skipped {count}: {reason} (see MANIFEST.tsv)")


def comparable(stderr: str) -> str:
    """Normalize stderr the way sqlc's test compares it (``stderrTransformer``). We also drop the ``# package <name>``
    header, as it names the Go package, which the rewritten config no longer has.
    """
    stderr = re.sub(r"^# package .*\n", "", stderr.replace("\r", ""))
    return stderr.replace("\\", "/").strip()


def one_line(text: str) -> str:
    return " ".join(text.split())[:200]


def compare(captured: Path, committed: Path) -> list[str]:
    """Every file of ``CAPTURED`` that differs between two trees, or is in one only."""

    def files(root: Path) -> set[Path]:
        return {path.relative_to(root) for pattern in CAPTURED for path in root.glob(pattern)}

    return [
        f"differs: {path}"
        for path in sorted(files(captured) | files(committed))
        if not (captured / path).is_file()
        or not (committed / path).is_file()
        or (captured / path).read_bytes() != (committed / path).read_bytes()
    ]


if __name__ == "__main__":
    main()
