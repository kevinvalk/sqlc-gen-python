"""Capture the requests sqlc sends a plugin, as committed test input.

Run through `cargo xtask capture [--sqlc-bin PATH] [--check]`, for the sqlc
version `.prototools` pins.

Every case in sqlc's own end-to-end suite (`internal/endtoend/testdata` at the
release tag) and every schema under `tests/schemas/src` goes through sqlc's
built-in `json` codegen, which dumps the exact `GenerateRequest` a plugin gets.
Cases run the way sqlc's `TestReplay` runs them: once per context they allow,
`base` (static analysis) and `managed-db` (analysed against a live PostgreSQL).

Each request is reduced to what a wasm plugin sees, minus what a test case
supplies itself, and written to:

- `tests/corpus/<case>/request[.managed-db][.<n>].json`: one file per distinct
  request, at the first case (in path order) that produced it. The tree mirrors
  sqlc's testdata, so a request sits at the path of its Go reference output.
- `tests/corpus/<case>/schema[.<n>].sql` beside it: the schema sqlc read for
  that package, as one script a database can run (`schema_script`), so the
  runtime sweep can build the tables a request describes.
- `tests/corpus/MANIFEST.tsv`: every case and context, and what became of it.
- `tests/corpus/SOURCE.toml` and `NOTICE`: provenance, and sqlc's MIT license.
- `tests/corpus/inflection.tsv`: sqlc's own singular of every table name in
  these requests, and of `WORDS`, run through sqlc's `internal/inflection`
  (which needs Go).
- `tests/schemas/<name>.json`: the golden-test fixtures.
- `protos/sqlc.proto`: re-vendored from the same tag, so the wire types always
  match the requests.

With `--check`, everything is written under `target/capture/<version>/check`
instead and compared byte for byte with what is committed, which proves the
committed files are exactly what the capture produces. Then the WASM plugin
runs under the same sqlc, which it must hold to the wire contract
(`wire_contract`): over the invoicing and pgtypes fixtures it writes exactly
their golden cases.

Nothing is written until everything that can fail has run.

The testdata is untrusted input. Configs are rewritten to keep only analysis
settings, which leaves sqlc nothing to run but itself: no plugins, no vet.
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
from pathlib import Path
from queue import Queue
from typing import TYPE_CHECKING, Any

import yaml

import postgres

if TYPE_CHECKING:
    from collections.abc import Iterable, Iterator

ROOT = Path(__file__).resolve().parents[2]
CORPUS = "tests/corpus"
SCHEMAS = "tests/schemas"
# Everything a capture writes, relative to where it writes.
CAPTURED = (
    f"{CORPUS}/**/request*.json",
    f"{CORPUS}/**/schema*.sql",
    f"{CORPUS}/MANIFEST.tsv",
    f"{CORPUS}/SOURCE.toml",
    f"{CORPUS}/NOTICE",
    f"{CORPUS}/inflection.tsv",
    f"{SCHEMAS}/*.json",
    "protos/sqlc.proto",
)
WASM = "target/wasm32-wasip1/release/sqlc-gen-python.wasm"
REPO = "sqlc-dev/sqlc"
TESTDATA = "internal/endtoend/testdata"
PROTO = "protos/plugin/codegen.proto"
INFLECTION = "internal/inflection"
# Words beyond the corpus's table names that exercise each rule of sqlc's
# singularisation, and each correction sqlc makes to it.
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
# The server sqlc's own test suite starts, pinned to the minor version: the
# managed-db requests depend on its catalog. `POSTGRESQL_SERVER_URI` names a
# server to use instead, as in sqlc's tests; it must be this version too.
POSTGRES = "16.15"
# Managed-db runs share a server only one at a time: sqlc names the database it
# creates after the schema, and concurrent sqlc processes would race on it.
SERVERS = 4

CONFIGS = ("sqlc.json", "sqlc.yaml", "sqlc.yml")
CONTEXTS = ("base", "managed-db")
# The settings that decide a request. The rest of a config (Go options, plugins,
# vet rules, cloud) never reaches a plugin's input.
ANALYSIS = (
    "engine",
    "schema",
    "queries",
    "database",
    "analyzer",
    "strict_function_checks",
    "strict_order_by",
)
# Dropped before any wasm plugin runs: `removePGCatalog`, internal/ext/wasm/wasm.go.
WASM_STRIPPED = ("pg_catalog", "information_schema")

Json = dict[str, Any]


@dataclass(frozen=True)
class Run:
    """One `sqlc generate` of one case in one context."""

    case: str
    context: str
    dir: Path
    config: Json
    env: dict[str, str]
    expected_error: str | None


@dataclass(frozen=True)
class Outcome:
    """What a run produced.

    `captured` requests, the `error` sqlc's test expects, an unexpected `failure`,
    or `skipped` for a context sqlc's tests do not have.
    """

    run: Run
    status: str
    requests: tuple[Json, ...] = ()
    detail: str = ""


def main() -> None:
    """Capture everything, or change nothing if anything fails."""
    parser = argparse.ArgumentParser()
    parser.add_argument("--sqlc-bin", type=Path, help="use this sqlc binary")
    parser.add_argument(
        "--check", action="store_true", help="fail if the capture differs"
    )
    args = parser.parse_args()
    version = f"v{pinned_sqlc().removeprefix('v')}"
    work = ROOT / "target/capture" / version
    out = work / "check" if args.check else ROOT
    shutil.rmtree(work / "check", ignore_errors=True)
    started = time.monotonic()
    signal.signal(signal.SIGTERM, signal.default_int_handler)  # Clean up.

    require_tools()
    sqlc = sqlc_binary(args.sqlc_bin, version, work)
    print(f"checking out sqlc {version}'s test cases", flush=True)
    source, commit = clone_sqlc(version, work)

    upstream = copy_tree(source / TESTDATA, work / "run/upstream")
    local = copy_tree(ROOT / SCHEMAS / "src", work / "run/local")
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
    if failed:
        sys.exit(f"{len(failed)} runs did not end the way sqlc's own test expects")

    inflection = inflect(source, outcomes)

    print(f"writing to {out}", flush=True)
    vendor_proto(source, version, out)
    for outcome in (o for o in outcomes if o.run.dir.is_relative_to(local)):
        (request,) = outcome.requests
        write_json(out / SCHEMAS / f"{outcome.run.case}.json", request)
    corpus = [o for o in outcomes if o.run.dir.is_relative_to(upstream)]
    write_corpus(corpus, out / CORPUS)
    (out / CORPUS / "inflection.tsv").write_text(inflection)
    (out / CORPUS / "SOURCE.toml").write_text(
        "# Written by `cargo xtask capture`. Do not edit.\n"
        f'sqlc = "{version}"\n'
        f'commit = "{commit}"\n'
        f'testdata = "https://github.com/{REPO}/tree/{commit}/{TESTDATA}"\n'
        f'managed_db = "PostgreSQL {POSTGRES}"\n'
    )
    (out / CORPUS / "NOTICE").write_text(
        f"The requests in this directory are the output of sqlc {version} run over\n"
        f"its own end-to-end test cases ({TESTDATA} at commit\n"
        f"{commit}), which are distributed under this license:\n\n"
        + (source / "LICENSE").read_text()
    )
    summarize(corpus, out / CORPUS, time.monotonic() - started)
    if args.check:
        if differences := compare(out, ROOT):
            print("\n".join(differences))
            sys.exit(f"the capture differs from what is committed; see {out}")
        wire_contract(sqlc)


def wire_contract(sqlc: Path) -> None:
    """Run the WASM plugin under sqlc, in a scratch directory, and hold it to
    what sqlc does with its answers (CLAUDE.md, source 1).

    It writes only inside `out`, and exactly the golden case: invoicing on
    SQLAlchemy and, async, on psycopg, and pgtypes. No options (so empty
    `plugin_options` bytes, which name no backend), no `async`, an unknown
    option and an unsupported feature each fail sqlc (exit 1) with the
    plugin's message on stderr, which sqlc shows only on failure, and write
    nothing.
    """
    print("holding the WASM plugin to sqlc's wire contract", flush=True)
    run(os.environ.get("CARGO", "cargo"), "wasm", cwd=ROOT)
    work = ROOT / "target/capture/wire"
    wasm = ROOT / WASM
    digest = hashlib.sha256(wasm.read_bytes()).hexdigest()
    plugin = {"name": "python", "wasm": {"url": wasm.as_uri(), "sha256": digest}}

    def fixture(name: str) -> Json:
        """Copy fixture `name` to `work`; return its package's paths."""
        source = ROOT / SCHEMAS / "src" / name
        shutil.rmtree(work, ignore_errors=True)
        shutil.copytree(source, work, ignore=shutil.ignore_patterns("sqlc.json", "gen"))
        (package,) = json.loads((source / "sqlc.json").read_text())["sql"]
        return {key: package[key] for key in ("schema", "queries", "engine")}

    def generate(package: Json, options: Json) -> subprocess.CompletedProcess[str]:
        """Run sqlc with `options`, if any; exit if it wrote outside `out`."""
        shutil.rmtree(work / "out", ignore_errors=True)
        codegen = {"out": "out", "plugin": "python", "options": options}
        config = {
            "version": "2",
            "plugins": [plugin],
            # `{}` leaves `options` out, which sqlc sends as empty bytes.
            "sql": [package | {"codegen": [{k: v for k, v in codegen.items() if v}]}],
        }
        (work / "sqlc.json").write_text(json.dumps(config))
        before = set(work.rglob("*"))
        result = subprocess.run(
            [sqlc, "generate"], cwd=work, capture_output=True, text=True, check=False
        )
        written = set(work.rglob("*")) - before
        if stray := [p for p in written if not p.is_relative_to(work / "out")]:
            sys.exit(f"sqlc wrote outside `out` with {options}: {stray}")
        return result

    def fails(package: Json, options: Json, *expected: str) -> None:
        result = generate(package, options)
        missing = [text for text in expected if text not in result.stderr]
        if result.returncode != 1 or missing or (work / "out").exists():
            sys.exit(
                f"sqlc with {options} must exit 1, write nothing and show"
                f" {missing} on stderr; it exited {result.returncode}:\n{result.stderr}"
            )

    for name, options, golden in [
        (
            "invoicing",
            {"backend": "sqlalchemy", "async": False},
            "invoicing_sqlalchemy",
        ),
        ("invoicing", {"backend": "psycopg", "async": True}, "invoicing_psycopg_async"),
        ("pgtypes", {"backend": "sqlalchemy", "async": False}, "pgtypes_sqlalchemy"),
    ]:
        if (result := generate(fixture(name), options)).returncode:
            sys.exit(f"sqlc failed:\n{result.stderr}")
        same_output(work / "out", ROOT / "tests/cases" / golden)
    invoicing = fixture("invoicing")
    fails(invoicing, {}, "reading plugin options", "missing field `backend`")
    fails(
        invoicing,
        {"backend": "sqlalchemy"},
        "reading plugin options",
        "missing field `async`",
    )
    backend = {"backend": "sqlalchemy", "async": False}
    fails(
        invoicing, backend | {"overrides": []}, "reading plugin options", "`overrides`"
    )
    (work / "query/unsupported.sql").write_text(
        "-- name: Touch :execlastid\nUPDATE customers SET name = name;\n"
    )
    fails(
        invoicing,
        backend,
        'unsupported.sql: query "Touch"',
        ":execlastid is not supported",
    )


def same_output(written: Path, golden: Path) -> None:
    """Exit unless sqlc wrote exactly a golden case's files, byte for byte."""

    def output(directory: Path) -> dict[str, bytes]:
        files = [p for p in directory.iterdir() if p.is_file() and p.name[0] != "."]
        return {p.name: p.read_bytes() for p in files if p.name != "case.json"}

    found, expected = output(written), output(golden)
    names = found.keys() | expected.keys()
    if differing := sorted(n for n in names if found.get(n) != expected.get(n)):
        where = f"{written.relative_to(ROOT)} differs from {golden.relative_to(ROOT)}"
        sys.exit(f"{where}: {', '.join(differing)}")


def run(*command: str | Path, cwd: Path | None = None, stdin: str | None = None) -> str:
    """Run a command that must succeed; return its stdout, or exit with stderr."""
    try:
        return subprocess.run(
            command, cwd=cwd, input=stdin, check=True, capture_output=True, text=True
        ).stdout
    except subprocess.CalledProcessError as error:
        line = " ".join(map(str, command))
        sys.exit(f"`{line}` failed ({error.returncode}):\n{error.stderr.strip()}")


def require_tools() -> None:
    """Exit unless what the capture runs is on PATH, before any of it runs."""
    needed = ["git", "go"]
    needed += [] if os.environ.get("POSTGRESQL_SERVER_URI") else ["docker"]
    if missing := [tool for tool in needed if shutil.which(tool) is None]:
        sys.exit(f"the capture needs {', '.join(missing)} on PATH")


def pinned_sqlc() -> str:
    """Return the sqlc version `.prototools` pins for development."""
    prototools = (ROOT / ".prototools").read_text()
    match = re.search(r'^sqlc = "(.+)"$', prototools, re.MULTILINE)
    if match is None:
        sys.exit(".prototools pins no sqlc version")
    return match[1]


def sqlc_binary(given: Path | None, version: str, work: Path) -> Path:
    """Return `given`, or else the release binary; either must be `version`."""
    if given and not os.access(given, os.X_OK):
        sys.exit(f"--sqlc-bin {given} is not an executable file")
    sqlc = given or download_sqlc(version, work)
    if (found := run(sqlc, "version").strip()) != version:
        sys.exit(f"{sqlc} is sqlc {found}, not {version}")
    return sqlc


def download_sqlc(version: str, work: Path) -> Path:
    """Extract the release binary for this host, checked against its digest.

    The archive is kept and checked again on every run, so what runs is always
    what was published.
    """
    arch = {"x86_64": "amd64", "amd64": "amd64", "arm64": "arm64", "aarch64": "arm64"}
    host = f"{sys.platform}_{arch[platform.machine().lower()]}"
    name = f"sqlc_{version[1:]}_{host}.tar.gz"
    release = json.loads(fetch(f"api.github.com/repos/{REPO}/releases/tags/{version}"))
    (digest,) = (a["digest"] for a in release["assets"] if a["name"] == name)
    cached = work / name
    print(f"fetching {name}, checked against its digest", flush=True)
    archive = cached.read_bytes() if cached.is_file() else b""
    if f"sha256:{hashlib.sha256(archive).hexdigest()}" != digest:
        archive = fetch(f"github.com/{REPO}/releases/download/{version}/{name}")
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
    """Return the body behind `https://<url>`.

    Calls to GitHub's API carry `GITHUB_TOKEN` if it is set: unauthenticated,
    the API allows a CI runner's shared address too few requests.
    """
    token = os.environ.get("GITHUB_TOKEN")
    api = token and url.startswith("api.github.com/")
    headers = {"Authorization": f"Bearer {token}"} if api else {}
    request = urllib.request.Request(f"https://{url}", headers=headers)
    with urllib.request.urlopen(request) as response:  # noqa: S310 -- always https
        body: bytes = response.read()
        return body


def clone_sqlc(version: str, work: Path) -> tuple[Path, str]:
    """Check out sqlc's source at the tag (just what we read); return its commit.

    A checkout kept from an earlier run is reused only while it is still the
    tag, unmodified.
    """
    source = work / "sqlc"
    git = ("git", "-C", str(source))
    if source.is_dir() and (
        run(*git, "status", "--porcelain")
        or run(*git, "rev-parse", "HEAD")
        != run(*git, "rev-parse", f"refs/tags/{version}^{{commit}}")
    ):
        shutil.rmtree(source)
    if not source.is_dir():
        partial = work / "sqlc.partial"
        shutil.rmtree(partial, ignore_errors=True)
        url = f"https://github.com/{REPO}.git"
        run("git", "clone", "--quiet", "--depth=1", "--filter=blob:none",
               "--sparse", f"--branch={version}", url, str(partial))  # fmt: skip
        partial.rename(source)
    # What we read; a checkout kept from before may be missing a part of it.
    run(*git, "sparse-checkout", "set", TESTDATA, INFLECTION,
                 str(Path(PROTO).parent))  # fmt: skip
    return source, run(*git, "rev-parse", "HEAD").strip()


def inflect(source: Path, outcomes: Iterable[Outcome]) -> str:
    """Return `name<TAB>singular` lines for every table name and `WORDS`,
    singularised by sqlc's own Go code.
    """
    names = set(WORDS) | {
        table["rel"]["name"]
        for outcome in outcomes
        for request in outcome.requests
        for schema in request["catalog"]["schemas"]
        for table in schema.get("tables", [])
    }
    print(f"singularising {len(names)} names with sqlc's Go", flush=True)
    # Inside sqlc's module, the only place its internal package can be used.
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
    """Copy the plugin protocol from the tag the corpus comes from."""
    url = f"https://github.com/{REPO}/blob/{version}/{PROTO}"
    header = f"// Copied verbatim from {url} (sqlc release {version})\n"
    (out / "protos").mkdir(parents=True, exist_ok=True)
    (out / "protos/sqlc.proto").write_text(header + (source / PROTO).read_text())


def copy_tree(source: Path, destination: Path) -> Path:
    """Copy cases to run in; sqlc resolves paths like `../schema.sql`."""
    shutil.rmtree(destination, ignore_errors=True)
    ignore = shutil.ignore_patterns("*.go", "go.mod", "go.sum", "gen")
    shutil.copytree(source, destination, ignore=ignore)
    return destination


def postgres_servers(stack: ExitStack) -> Queue[str]:
    """Start the PostgreSQL servers the managed-db context needs.

    Fails, rather than capturing half the corpus, without Docker or
    `POSTGRESQL_SERVER_URI`, or if a server is not the pinned version.
    """
    servers: Queue[str] = Queue()
    # One URL when `POSTGRESQL_SERVER_URI` names it, so it is never shared.
    urls = {
        stack.enter_context(
            postgres.server(f"postgres:{POSTGRES}", "POSTGRESQL_SERVER_URI")
        )
        for _ in range(SERVERS)
    }
    for url in urls:
        if (found := postgres.version(url)) != POSTGRES:
            sys.exit(f"the managed-db server is PostgreSQL {found}, not {POSTGRES}")
        # sqlc takes a plain libpq URL.
        plain = url.set(drivername="postgresql")
        servers.put(plain.render_as_string(hide_password=False))
    return servers


def upstream_runs(root: Path) -> Iterator[Run]:
    """Find cases as sqlc's `FindTests` does and run them as `TestReplay` does.

    A case is the first directory on a path that holds a config; it runs in each
    context its `exec.json` allows, with that file's environment.
    """
    for directory, children, files in os.walk(root):
        children.sort()
        config = next((name for name in CONFIGS if name in files), None)
        if config is None:
            continue
        children.clear()
        case = Path(directory)
        settings: Json = (
            json.loads((case / "exec.json").read_text()) if "exec.json" in files else {}
        )
        for context in settings.get("contexts", CONTEXTS):
            yield Run(
                case=case.relative_to(root).as_posix(),
                context=context,
                dir=case,
                config=analysis_only(case / config),
                env=settings.get("env", {}),
                expected_error=expected_error(case, context),
            )


def local_runs(root: Path) -> Iterator[Run]:
    """Run each schema of the golden tests, statically."""
    for case in sorted(root.iterdir()):
        config = analysis_only(case / "sqlc.json")
        yield Run(case.name, "base", case, config, {}, None)


def analysis_only(path: Path) -> Json:
    """Reduce a config to what decides the requests, as config version 2."""
    text = path.read_text()
    config: Json = json.loads(text) if path.suffix == ".json" else yaml.safe_load(text)
    packages = config["packages"] if str(config["version"]) == "1" else config["sql"]
    sql = [
        {"engine": "postgresql"}  # The version 1 default.
        | {key: package[key] for key in ANALYSIS if package.get(key) is not None}
        for package in packages
    ]
    servers = {"servers": config["servers"]} if config.get("servers") else {}
    return {"version": "2", "sql": sql, **servers}


def expected_error(case: Path, context: str) -> str | None:
    """Return the stderr sqlc's test expects, found the way `parseStderr` does."""
    for name in (f"stderr/{context}.txt", f"stderr_{sys.platform}.txt", "stderr.txt"):
        if (case / name).is_file():
            return comparable((case / name).read_text())
    return None


def generate(sqlc: Path, servers: Queue[str], run: Run) -> Outcome:
    """Run sqlc's `json` codegen, which writes the request a plugin would get."""
    if run.context not in CONTEXTS:
        return Outcome(run, "skipped", detail="sqlc's tests have no such context")
    if run.context == "base":
        return generate_with(sqlc, run, run.config)
    server = servers.get()
    try:
        # sqlc's managed-db test context: every package analysed on a server.
        sql = [
            package | {"database": {"managed": True}} for package in run.config["sql"]
        ]
        database = {"name": "postgres", "engine": "postgresql", "uri": server}
        return generate_with(
            sqlc, run, run.config | {"servers": [database], "sql": sql}
        )
    finally:
        servers.put(server)


def generate_with(sqlc: Path, run: Run, config: Json) -> Outcome:
    """Run sqlc with `config`, written beside the original config it came from."""
    out = f"_capture/{run.context}"
    sql = [
        package | {"gen": {"json": {"out": out, "filename": f"{index}.json"}}}
        for index, package in enumerate(config["sql"])
    ]
    path = f"sqlc.capture.{run.context}.json"
    (run.dir / path).write_text(json.dumps(config | {"sql": sql}))
    result = subprocess.run(
        [sqlc, "generate", "--file", path],
        cwd=run.dir,
        env={**os.environ, "SQLCCACHE": str(run.dir / out / "cache"), **run.env},
        capture_output=True,
        text=True,
        check=False,
    )
    if result.returncode:
        error = comparable(result.stderr)
        status = "error" if error == run.expected_error else "failure"
        return Outcome(run, status, detail=one_line(error))
    dumps = [run.dir / out / f"{index}.json" for index in range(len(sql))]
    requests = tuple(plugin_view(json.loads(dump.read_text())) for dump in dumps)
    shutil.rmtree(run.dir / out)  # Each dump carries ~1.5 MB of pg_catalog.
    return Outcome(run, "captured", requests)


def plugin_view(request: Json) -> Json:
    """Reduce a request to what a wasm plugin gets and a test case cannot supply.

    Options and the output directory belong to a case; the paths of the source
    files mean nothing without them.
    """
    catalog = request["catalog"]
    schemas = [s for s in catalog["schemas"] if s["name"] not in WASM_STRIPPED]
    return {
        "settings": {"engine": request["settings"]["engine"]},
        "catalog": catalog | {"schemas": schemas},
        "queries": request["queries"],
        "sqlc_version": request["sqlc_version"],
    }


def write_corpus(outcomes: Iterable[Outcome], corpus: Path) -> None:
    """Replace the stored requests, one file per distinct one, and the manifest."""
    for stale in [*corpus.glob("**/request*.json"), *corpus.glob("**/schema*.sql")]:
        stale.unlink()
    for directory in sorted(corpus.glob("**/"), reverse=True):
        if not any(directory.iterdir()):
            directory.rmdir()

    stored: dict[str, str] = {}  # Request -> where it is stored.
    rows = ["case\tcontext\trequest\tnote"]
    for outcome in sorted(outcomes, key=lambda o: (o.run.case, o.run.context)):
        run, requests = outcome.run, outcome.requests
        if outcome.status != "captured":
            rows.append(
                f"{run.case}\t{run.context}\t-\t{outcome.status}: {outcome.detail}"
            )
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
    """Return the schema sqlc read for one package of a run, as one script.

    Its files come in sqlc's order (`sqlpath.Glob`), each without the
    rollback half of a migration or psql meta-commands (`internal/migrations`),
    and each ended by a `;` of its own.
    """
    patterns = run.config["sql"][package]["schema"]
    paths: list[Path] = []
    for pattern in [patterns] if isinstance(patterns, str) else patterns:
        if any(c in pattern for c in "*?[]"):
            paths += sorted(run.dir.glob(pattern))
        else:
            paths.append(run.dir / pattern)
    files = [
        file
        for path in paths
        for file in (sorted(path.iterdir()) if path.is_dir() else [path])
        if file.suffix == ".sql"
        and not file.name.startswith(".")
        and not file.name.endswith(".down.sql")
    ]
    return "".join(
        f"-- {Path(os.path.relpath(file, run.dir)).as_posix()}\n"
        f"{runnable(file.read_text())}\n;\n"
        for file in files
    )


# Where sqlc stops reading a migration: goose, sql-migrate, tern and dbmate.
ROLLBACK = (
    "-- +goose down",
    "-- +migrate down",
    "---- create above / drop below ----",
    "-- migrate:down",
)
PSQL_META = re.compile(r"^\\[A-Za-z!?;][^\n]*$")


def runnable(sql: str) -> str:
    """Cut what sqlc does not read: a migration's rollback and psql commands."""
    lines: list[str] = []
    for line in sql.splitlines():
        if line.lower().startswith(ROLLBACK):
            break
        if not PSQL_META.match(line):
            lines.append(line)
    return "\n".join(lines)


def write_json(path: Path, value: Json) -> None:
    """Write JSON in one stable layout, keeping sqlc's order of everything."""
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, indent=2, ensure_ascii=False) + "\n")


def summarize(outcomes: list[Outcome], corpus: Path, seconds: float) -> None:
    """Print what a reviewer of the capture diff wants to know first."""
    requests = list(corpus.glob("**/request*.json"))
    size = sum(path.stat().st_size for path in requests)
    engines = Counter(json.loads(p.read_text())["settings"]["engine"] for p in requests)
    runs = Counter(f"{o.run.context} {o.status}" for o in outcomes)
    print(f"{len(requests)} distinct requests, {size / 1e6:.1f} MB, in {seconds:.0f}s")
    print("by engine:", ", ".join(f"{e} {n}" for e, n in sorted(engines.items())))
    print("runs:", ", ".join(f"{k} {n}" for k, n in sorted(runs.items())))
    skipped = Counter(o.detail for o in outcomes if o.status == "skipped")
    for reason, count in skipped.items():
        print(f"skipped {count}: {reason} (see MANIFEST.tsv)")


def comparable(stderr: str) -> str:
    """Normalize stderr the way sqlc's test compares it (`stderrTransformer`).

    The `# package <name>` header goes too: it names the Go package, which the
    rewritten config no longer has.
    """
    stderr = re.sub(r"^# package .*\n", "", stderr.replace("\r", ""))
    return stderr.replace("\\", "/").strip()


def one_line(text: str) -> str:
    """Squeeze an error message onto one manifest line."""
    return " ".join(text.split())[:200]


def compare(captured: Path, committed: Path) -> list[str]:
    """Every file of `CAPTURED` that differs between two trees, or is in one only."""

    def files(root: Path) -> set[Path]:
        return {p.relative_to(root) for glob in CAPTURED for p in root.glob(glob)}

    return [
        f"differs: {path}"
        for path in sorted(files(captured) | files(committed))
        if not (captured / path).is_file()
        or not (committed / path).is_file()
        or (captured / path).read_bytes() != (committed / path).read_bytes()
    ]


if __name__ == "__main__":
    main()
