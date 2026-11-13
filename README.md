# sqlc-gen-python

A [sqlc](https://sqlc.dev) codegen plugin that generates Python, written in Rust
and compiled to WASM.

It builds a [ruff](https://github.com/astral-sh/ruff) Python AST and hands it to
ruff to unparse and format, so the output is byte-identical to what `ruff format`
would produce — generated files never show up as a diff in your own formatting
step. The plugin is ~3 MB of WASM, against ~19 MB for the official Go plugin.

> **Status: work in progress.** Models, enums and query modules are generated
> for PostgreSQL, on SQLAlchemy Core or psycopg 3, and for SQLite, on
> SQLAlchemy Core or `sqlite3` and `aiosqlite`; each sync and async. MySQL is
> not supported yet: generating for it is an error. Which sqlc features
> generate, which are a clean error, and which at least one query using them
> ran against PostgreSQL, is [`docs/compat.md`](docs/compat.md). See
> [Roadmap](#roadmap).

## Install

Each [release](https://github.com/kevinvalk/sqlc-gen-python/releases/latest)
holds a complete `sqlc.yaml` to start from, its plugin URL and sha256 filled
in, with the sqlc version it was tested under and the commands that check the
download. The URL names a version, never `latest`, because sqlc refuses a
download whose sha256 differs.

The plugin needs sqlc's static analysis, its default. A `sql` entry with a
`database` block also needs `analyzer: {database: false}`: the database-backed
analysis spells types the way static analysis spells its guesses, which
generation rejects ([§5, rule 2](docs/python-output.md#type-map)).

For a local build, run `cargo wasm` and point the plugin at it, with the sha256
from `shasum -a 256 target/wasm32-wasip1/release/sqlc-gen-python.wasm`
(without one, sqlc warns and hashes the file itself):

```yaml
wasm:
  url: file://./target/wasm32-wasip1/release/sqlc-gen-python.wasm
  sha256: <the hash>
```

## Options

Two are required: `backend` (`sqlalchemy`, `psycopg` for PostgreSQL, or
`sqlite3` for SQLite) and `async` (`true` for coroutines, `false` for blocking
functions). The rest are optional: `omit_sqlc_version`,
`models.exact_table_names` and `models.inflection_exclude`. They, and why
every other option you may know is gone, are in
[the output specification, §7](docs/python-output.md#7-options).

```yaml
codegen:
  - out: src/billing/db
    plugin: python
    options:
      backend: psycopg
      async: false
```

```python
from billing.db import invoices

with psycopg.connect(DSN) as conn, conn.transaction():
    invoice = invoices.get_invoice(conn, id=invoice_id)
```

With `async: true`, the same module holds coroutines instead:
`await invoices.get_invoice(aconn, id=invoice_id)`. A codebase that needs both
adds a second `codegen` entry for the same queries, with another `out`.

Unknown keys are an error, so a typo in `sqlc.yaml` fails loudly instead of silently doing
nothing.

This is **not** compatible with the options or output of
[`sqlc-dev/sqlc-gen-python`](https://github.com/sqlc-dev/sqlc-gen-python) or the
[`codean-io` fork](https://github.com/codean-io/sqlc-gen-python).

What the plugin generates, rule by rule and with the source each rule rests
on, is specified in [`docs/python-output.md`](docs/python-output.md).

## Development

```sh
cargo test             # hermetic: no Python, Docker or network
cargo wasm             # the WASM plugin
cargo xtask check
cargo xtask bless
cargo xtask capture
cargo xtask verify
```

`cargo xtask help` says what each xtask command does and needs; CI
(`.github/workflows/ci.yml`) runs `check`, `verify --mutants --check` and
`capture --check`, then requires a clean rebuild of the plugin to give the same
bytes. Python tooling runs through [uv](https://docs.astral.sh/uv/) in the
`tests/py` project; `cargo test` never needs it. What each layer of tests
proves is [`docs/python-output.md` §Proof](docs/python-output.md#proof), the
invariants every output holds are listed in `tests/output/corpus.rs`, and how the
test input is captured from sqlc is `tests/py/capture.py`.

## Releasing

Set `version` in `Cargo.toml` and run `cargo check`, which updates `Cargo.lock`
to match. Get both onto `main` (a PR, or `git push origin main`) and wait for
CI to pass there, then tag what `main` holds:

```sh
git fetch origin && git tag vX.Y.Z origin/main && git push origin vX.Y.Z
```

`.github/workflows/release.yml` refuses a tag that is not on `main` or not `v`
plus that version, runs all of CI on the tag, and publishes the plugin CI
tested: `capture --check` built it from the committed `Cargo.lock` (`cargo wasm`
is `--locked`, so a stale lock fails the tag) with the toolchain pinned in
`rust-toolchain.toml`, and ran it under the sqlc pinned in `.prototools`, and a
clean rebuild on a second runner of the same image gave the same bytes. The
release holds `sqlc-gen-python.wasm`, `SHA256SUMS`, a signed build provenance
attestation of the same digest, and notes from `.github/release-notes.md` with
the `sqlc.yaml` block and the checks filled in.

Dependabot (`.github/dependabot.yml`) bumps the actions, the crates, the Rust
toolchain and the `tests/py` lock. These pins it cannot follow are bumped by
hand, each held to CI like any other change:

- sqlc, in `.prototools`, then `cargo xtask capture`, which re-captures the
  corpus, the fixtures and `protos/sqlc.proto` from that release;
- the ruff crates' git `tag` in `Cargo.toml`;
- uv's `required-version` in `tests/py/pyproject.toml`, which CI's setup-uv
  reads;
- PostgreSQL: `NEWEST` in `tests/py/postgres.py` for the runtime proof, and
  `POSTGRES` in `tests/py/capture.py` for the managed-db corpus.

CI's Go is `stable` on purpose: sqlc's `go.mod` selects the toolchain.

The workflow cannot stop someone with push access from tagging, nor a published
asset from being replaced, so two repository settings complete it: a tag
ruleset on `v*` that restricts creation to maintainers and blocks updates and
deletion, and Immutable Releases. A tag whose release failed therefore cannot
be moved or reused: fix the cause and release the next patch version.

## Architecture

```
src/
  lib.rs           `generate`: config -> api -> files
  main.rs          the stdio shim sqlc runs
  plugin.rs        the sqlc wire protocol, generated from protos/sqlc.proto
  config.rs        what the user asked for
  python.rs        writing Python: syntax, types, imports, rendering

  api/             the generated API, decided once, whatever the engine and backend
    mod.rs           `Api` and `resolve`: the enums, models and query modules
    models.rs        the catalog as enums, models and typed fields (`Resolver`)
    queries.rs       the queries as functions, parameters and result shapes
    inflection.rs    singularising table names exactly as sqlc does

  render/          writing the `Api` as files
    mod.rs           the package's files, the header, the models module, records
    queries.rs       a query file's modules: statements, row classes, functions

  engine/          the database: what its types are in Python, how its SQL reads
    mod.rs           `Engine`, one variant per engine sqlc analyses for; `Sql`
    postgresql/      types.rs (the type map), lexer.rs (placeholders as code)
    sqlite/          types.rs (the type map), lexer.rs (placeholders as code)

  backend/         the Python library the generated code runs SQL through
    mod.rs           `Backend`, one variant per library
    sqlalchemy/      mod.rs (statements, function bodies), text.rs (text() rewriting)
    dbapi.rs         a driver that takes sqlc's SQL as written: function bodies
    psycopg/         psycopg 3's raw cursors, as a `dbapi::Driver`
    sqlite3/         `sqlite3` and `aiosqlite`, as a `dbapi::Driver`
```

Three layers, which vary independently:

- An **engine** is the database sqlc analysed the queries for, as `engine:`
  names it. It answers which Python type a column type is (every entry backed
  by driver evidence, `docs/typemap.md`), which types are sqlc's guesses, and
  where the placeholders are in its SQL. PostgreSQL and SQLite; MySQL is an
  error until it gets a type map with evidence.
- A **backend** is the library the generated code calls, chosen with the
  `backend` option. It answers how sqlc's SQL is written for it, what the
  statement constant is, what a connection is, and how a function executes and
  fetches. One backend may serve several engines (SQLAlchemy does), and a
  backend tied to one engine (psycopg, `sqlite3`) refuses the others. psycopg
  and `sqlite3` take sqlc's SQL as written, so they share one body writer,
  `dbapi`, and differ only in a `Driver`: the connection, the cursor, and how a
  parameter binds.
- The rest is the **generated API**, the same whatever the engine and backend:
  models, enums, names, parameters, result shapes and errors.

`generate` reads that order: the options (`Config`), then the API
(`api::resolve`: the engine, `Engine::of`; the catalog, `models::resolve`;
the queries into one module per query file, `queries::modules`, which asks the
backend whether it can run each), then the files (`render::files`, which asks
the backend for statements and function bodies). `Engine` and `Backend`
dispatch with an exhaustive `match` per question, so adding a variant makes the
compiler list every question it has to answer.

Each module with rules of its own has its spec beside it: `spec.rs` next to a
`mod.rs`, `<name>_spec.rs` next to `<name>.rs`; the golden tests pin what
they produce together.

Imports are never declared. `python::render` reads them off the names a module
reads, from the fixed vocabulary of `docs/python-output.md` §2, so a name and
the import that makes it resolve cannot drift apart, and a type is just its
annotation — `"numeric" => Type::DECIMAL`, which is `Decimal`.

Case conversion is [`heck`]. Singularisation is a port of sqlc's own rules,
not a library's, because a class must have the name sqlc-gen-go gives the same
table's struct; `tests/corpus/inflection.tsv` holds it to sqlc.

[`heck`]: https://docs.rs/heck

`render` assembles trees from the node builders in `python.rs`, and parses the
fixed parts (a function's body, an annotation) from source fragments whose
every name is a proven identifier; data that is not, such as SQL, only ever
enters the tree as a node. `python::render` calls ruff's `Generator` to unparse
the tree and ruff's formatter to lay it out, after writing each multi-line
string, a docstring or a statement's SQL, as a triple-quoted one.

Two parts of a file are text rather than tree, both from fixed vocabularies of
this crate. The header: ruff's AST has nowhere to put a `#` comment —
`format_module_ast` recovers comments from `CommentRanges` into the *source
text*, and asserts every range maps onto real source — so the banner is
prepended to the formatted output. And the imports: the blank line between
isort's sections is layout a tree cannot carry, so they are written in the
vocabulary's order before the unparsed body. Everything carrying schema data
goes through the AST, with schema documentation becoming a docstring rather
than a comment.

## Roadmap

- [x] Models, enums, arrays, multi-schema catalogs
- [x] PostgreSQL types, each proven against psycopg and asyncpg
- [x] Query modules for PostgreSQL, SQLAlchemy Core sync and async, including
      `sqlc.embed` and `:copyfrom`
- [x] psycopg 3 as a backend, running sqlc's SQL as written
- [x] SQLite, on SQLAlchemy and on `sqlite3` / `aiosqlite`
- [ ] SQLite's `sqlc.slice`, and its types proven as deeply as PostgreSQL's
- [ ] `overrides`, `rename`
- [ ] MySQL, with a type map proven against its drivers

## License

Dual-licensed:

- **Open source:** AGPLv3 — free to use, modify and share, as long as derivative
  works are also open.
- **Commercial:** available for closed-source or commercial use without AGPL
  obligations. Enquiries: kevin@valk.email

`SPDX-License-Identifier: AGPL-3.0-or-later OR LicenseRef-KevinValk-Commercial`
