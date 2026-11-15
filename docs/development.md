# Development

```sh
cargo test           # the Rust tests: no Python, Docker or network
cargo xtask check    # fmt, clippy, cargo test, Python type checks, ruff and mypy on tests/python
cargo xtask verify   # runs the generated code on PostgreSQL, MySQL, SQLite (Docker)
cargo xtask bless    # accept changed output into examples/, after reading it
cargo xtask capture  # re-capture tests/sqlc/ from the pinned sqlc (network)
cargo wasm           # build the plugin
```

`cargo xtask` is the [xtask convention](https://github.com/matklad/cargo-xtask):
these commands are the Rust program in `xtask/`. The Python side runs through
[uv](https://docs.astral.sh/uv/).

| Path | |
|---|---|
| `src/` | the plugin |
| `examples/` | sqlc projects and the Python they generate; also the golden tests |
| `tests/sqlc/` | what sqlc sends the plugin, for `examples/` and for sqlc's own test suite (`endtoend/`) |
| `tests/rust/` | `cargo test`: the examples byte for byte, and invariants over everything in `tests/sqlc/` |
| `tests/python/` | type-checks and runs the generated code |
| `docs/python-output.md` | what the plugin generates, rule by rule |

To see what the plugin makes of one statement, have sqlc write the request
it sends, with its own `json` codegen, and run the plugin on it:

```yaml
# sqlc.yaml, beside a schema.sql and a query.sql
version: "2"
sql:
  - engine: postgresql
    schema: schema.sql
    queries: query.sql
    gen:
      json: {out: out, filename: request.json}
```

```sh
sqlc generate
cargo xtask generate out/request.json '{"backend": "psycopg", "async": false}'
```

## Architecture

```
src/
  lib.rs           `generate`: config -> api -> files
  main.rs          the stdio shim sqlc runs
  plugin.rs        the sqlc wire protocol, generated from protos/sqlc.proto
  config.rs        what the user asked for
  python.rs        writing Python, as ruff's tree from the first node to the last
  python/
    syntax.rs        the names Python accepts; a constructor for each node, no parser
      expression.rs    expressions, and what one grows into
      statement.rs     statements, functions, classes, imports
    vocabulary.rs    every name generated code reads or binds, each a constant
    types.rs         the Python types a column can have; an override's annotation
    imports.rs       the imports, read off the names a module reads
    render.rs        a tree as source: ruff unparses, ruff formats

  api/             the generated API, decided once, before anything is written
    mod.rs           `Api` and `resolve`: the enums, models and query modules
    models.rs        the catalog as enums, models and typed fields
    resolver.rs      one catalog read: types by the type map and overrides, renames
    names.rs         the Python names of columns, parameters and enum labels
    queries.rs       the queries as modules, functions and commands
    params.rs        a query's parameters
    results.rs       what a query's rows become
    inflection.rs    singularising table names exactly as sqlc does

  render/          writing the `Api` as files
    mod.rs           the package's files, the header, records
    models.rs        the models module: enums and tables as classes
    queries.rs       a query file's module: statements, row classes, functions

  engine/          the database: what its types are in Python, how its SQL reads
    mod.rs           `Engine`, one variant per engine sqlc analyses for; `Sql`
    select.rs        what sqlc does not report of a result, as far as the SQL proves it
    postgresql/      types.rs (the type map), lexer.rs (placeholders as code)
    sqlite/          types.rs (the type map), lexer.rs (placeholders as code)
    mysql/           types.rs (the type map), lexer.rs (placeholders as code)

  backend/         the Python library the generated code runs SQL through
    mod.rs           `Backend`, one variant per library; what a function returns
    sqlalchemy/      mod.rs (statements, function bodies), text.rs (text() rewriting)
    dbapi.rs         a driver that takes sqlc's SQL as written: function bodies
    psycopg.rs       psycopg 3's raw cursors, as a `dbapi::Driver`
    sqlite3.rs       `sqlite3` and `aiosqlite`, as a `dbapi::Driver`
    pymysql.rs       PyMySQL and asyncmy, as a `dbapi::Driver` that takes `%s`
```

- **engine**: the database (`engine:`): its type map, where its placeholders
  are, and what its SQL says of a result that sqlc does not report.
- **backend**: the Python library (`backend:`): how a statement is written and
  run, and the few types its drivers disagree on.
- **api**: everything else, decided once. It asks the engine and the backend
  what only they know, and fails there for whatever they cannot do; `render`
  after it only writes, and cannot fail.

Where sqlc's report is not enough (`engine/select.rs`), the plugin reads the
SQL's tokens, and answers only what they prove: what it cannot read is the
answer that widens a type (a column that may be NULL, a row that may be
missing), never the one that narrows it. A change there is tried against
real statements first: sqlc's request for one, as above, shows what the
plugin makes of it.

Generated Python is never written as text: every expression and statement is
built as a ruff AST node (`python/syntax.rs`), so a construct that is not
Python does not compile, and schema data can only ever be a name or a string.
A name generated code reads or binds itself (`cur`, `Sequence`, `from_row`)
is a constant of `python/vocabulary.rs`, which reads as the name it is:
`CUR.attr("execute").call([statement])`. What is still spelt where it is used
is a library's own member (`"execute"`), in the backend that calls it.
What is text is what a tree cannot hold: the comments that open a file (the
header, and `# ruff: noqa: S105` where a statement's name would trip it), the
blank line between import sections, and how a docstring or a multi-line SQL
string is spelt. The plugin parses Python in two places: an override's
`python_type`, which is the user's, and each module it has unparsed, to find
those strings and for ruff to format.

`Engine` and `Backend` answer each question with a `match` that names every
variant, so a new one makes the compiler list what it must answer. Each
module's rules are specified in a `_spec.rs` beside it.

## Releasing

Every commit on `main` that passes CI is published as the `unstable`
prerelease, which moves with `main`. A version:

1. Set `version` in `Cargo.toml`, run `cargo check`, and get both onto `main`
   with CI green.
2. `git fetch origin && git tag vX.Y.Z origin/main && git push origin vX.Y.Z`

`release.yml` reruns CI on the tag and publishes the WASM, `SHA256SUMS`, a
build attestation and the notes in `.github/release-notes.md`.

Renovate (`.github/renovate.json5`) bumps what is pinned by name: the
actions, the crates of `Cargo.toml`, the toolchain, the packages of
`tests/python`, the uv CI runs, ruff (the crates' tag and the Python pin
together), sqlc and the databases. What only a lock file names (a dependency's
own dependencies) moves when what needs it does. Each release waits a week,
then is merged into `main` once CI passes on its branch, and so into
`unstable`. One that fails becomes a pull request, to finish by hand:

- sqlc: `cargo xtask capture`, and `POSTGRES` in `tests/python/capture.py` to
  the version sqlc's own tests run;
- ruff: `cargo update` for the crates' new tag, then `cargo xtask bless` if
  the formatter writes anything differently;
- PostgreSQL or MySQL: `cargo xtask verify` to rewrite `docs/typemap.md`;
- a uv minor: widen `required-version` in `tests/python/pyproject.toml`.
