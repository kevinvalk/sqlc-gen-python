# Development

```sh
cargo test           # the Rust tests: no Python, Docker or network
cargo xtask check    # fmt, clippy, cargo test, Python type checks, ruff and mypy on tests/python
cargo xtask verify   # runs the generated code on PostgreSQL, MySQL, SQLite (Docker)
cargo xtask bless    # accept changed output into examples/, after reading it
cargo xtask capture  # re-capture tests/sqlc/ from the pinned sqlc (network)
cargo wasm           # build the plugin
```

`cargo xtask` is the [xtask convention](https://github.com/matklad/cargo-xtask): these commands are the Rust program in `xtask/`. The Python side runs through [uv](https://docs.astral.sh/uv/).

| Path | |
|---|---|
| `src/` | the plugin |
| `examples/` | sqlc projects and the Python they generate; also the golden tests |
| `tests/sqlc/` | what sqlc sends the plugin, for `examples/` and for sqlc's own test suite (`endtoend/`) |
| `tests/rust/` | `cargo test`: the examples byte for byte, and invariants over everything in `tests/sqlc/` |
| `tests/python/` | type-checks and runs the generated code |
| `docs/typemap.md` | what each driver returns for each mapped type, written by `cargo xtask verify` |

To see what the plugin makes of one statement, let sqlc write the request it sends with its own `json` codegen, and run the plugin on that:

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

- **engine**: the database (`engine:`): its type map, where its placeholders are, and what its SQL says of a result that sqlc does not report.
- **backend**: the Python library (`backend:`): how a statement is written and run, and the few types its drivers disagree on.
- **api**: everything else, decided once. It asks the engine and the backend what only they know and fails there for whatever they cannot do, so `render` after it only writes and cannot fail.

Where sqlc's report is not enough (`engine/select.rs`), we read the SQL's tokens and answer only what they prove. Whatever we cannot read gets the answer that widens a type (a column that may be NULL, a row that may be missing), never the one that narrows it. Try a change there on real statements first: sqlc's request for one, as above, shows what the plugin makes of it.

We never write generated Python as text. Every expression and statement is a ruff AST node (`python/syntax.rs`), so a construct that is not Python does not compile, and schema data can only ever end up as a name or a string. A name the generated code reads or binds itself (`cur`, `Sequence`, `from_row`) is a constant in `python/vocabulary.rs`, so it reads as the name it is: `CUR.attr("execute").call([statement])`. Only a library's own member (`"execute"`) is spelled where it is used, in the backend that calls it.

What a tree cannot hold is the one part that is text: the comments that open a file (the header, and `# ruff: noqa: S105` where a statement's name would trip it), the blank line between import sections, and how a docstring or a multi-line SQL string is spelled. We parse Python in two places only: an override's `python_type`, which is the user's, and each module we unparsed, to find those strings and for ruff to format.

`Engine` and `Backend` answer each question with a `match` that names every variant, so adding one makes the compiler list everything it has to answer. The rules of each module are specified in a `_spec.rs` beside it.

## Proof

Each layer catches what the one before cannot see. `cargo xtask help` lists the commands.

- **`cargo test`** pins every golden case byte for byte, and holds every golden and corpus output to the invariants no type checker or runtime can see (`tests/rust/corpus.rs`, each with a planted bug in `tests/rust/invariants.rs`): a NOT NULL column annotated `X | None`, two parameters bound to each other's names, a `# type: ignore`.
- **`cargo xtask check`** runs every distinct output through the static gate (`tests/python/gate.py`): ruff with every rule, pyright strict and mypy strict, as `tests/python/pyproject.toml` sets them. And the scanner proof (`tests/python/scanner.py`) holds the `text()` rewriting to libpg_query, the scanner sqlc parses with, on a few thousand statements Hypothesis draws from what PostgreSQL scans specially.
- **`cargo xtask verify`** (`tests/python/verify.py`) runs the generated code:
  - the gate over `tests/python/suite/usage.py`, which pins the types application code sees and the mistakes that must stay type errors;
  - pytest (`tests/python/suite/`): a scenario through every function of every backend, sync and async, on PostgreSQL, MySQL and SQLite, every value held to its annotation;
  - the type probe (`tests/python/typemap.py`): every mapped type at its edges through every driver, written to `docs/typemap.md`;
  - the runtime sweep (`tests/python/autoprop.py`): every PostgreSQL function of the golden cases and the corpus called with drawn arguments, each result compared with sqlc's own SQL run directly. What sqlc misreports is in `tests/python/known-failures.tsv`;
  - pytest and the probe again on the newest Python, with SQLAlchemy 2.1.

With `--mutants`, `verify` plants plausible generator bugs (`MUTANTS` in `verify.py`) one at a time, and the layer named for each must catch it, saying why. With `--check`, as CI runs it, a `docs/typemap.md` that would change fails the run. `cargo xtask bless` runs `verify` before it accepts a change, as a wrong type that is still well typed passes the gate.

## Releasing

Every commit on `main` that passes CI is published as the `unstable` prerelease, which moves with `main`. Git will not move a tag it already has, so a fetch fails with "would clobber existing tag" once `unstable` has moved; we let it move in each clone once:

```sh
git config --add remote.origin.fetch '+refs/tags/unstable:refs/tags/unstable'
```

A version is only its tag, on a commit of `main`:

```sh
git fetch origin && git tag vX.Y.Z origin/main && git push origin vX.Y.Z
```

CI runs on the tag, publishes the WASM, a build attestation and the notes in `.github/release-notes.md`, and then commits the release's URL and sha256 into the README on `main`.

Renovate (`.github/renovate.json5`) bumps what is pinned by name: the actions, the crates of `Cargo.toml`, the toolchain, the packages of `tests/python`, the uv CI runs, ruff (the crates' tag and the Python pin together), sqlc and the databases. What only a lock file names (a dependency's own dependencies) moves when what needs it does. Each release waits a week, then is merged into `main` once CI passes on its branch, and so into `unstable`. One that fails becomes a pull request, to finish by hand:

- sqlc: `cargo xtask capture`, and `POSTGRES` in `tests/python/capture.py` to the version sqlc's own tests run;
- ruff: `cargo update` for the crates' new tag, then `cargo xtask bless` if the formatter writes anything differently;
- PostgreSQL or MySQL, or a driver's minor release (the document names each to its minor): `cargo xtask verify` to rewrite `docs/typemap.md`;
- a uv minor: widen `required-version` in `tests/python/pyproject.toml`.
