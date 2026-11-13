# Development

```sh
cargo test           # the Rust tests: no Python, Docker or network
cargo xtask check    # what CI requires: fmt, clippy, cargo test, Python type checks
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
    mysql/           types.rs (the type map), lexer.rs (placeholders as code)

  backend/         the Python library the generated code runs SQL through
    mod.rs           `Backend`, one variant per library
    sqlalchemy/      mod.rs (statements, function bodies), text.rs (text() rewriting)
    dbapi.rs         a driver that takes sqlc's SQL as written: function bodies
    psycopg/         psycopg 3's raw cursors, as a `dbapi::Driver`
    sqlite3/         `sqlite3` and `aiosqlite`, as a `dbapi::Driver`
    pymysql/         PyMySQL, as a `dbapi::Driver` that takes `%s`
```

- **engine**: the database (`engine:`): its type map and where its placeholders are.
- **backend**: the Python library (`backend:`): how a statement is written and run.
- **api**: everything else, decided once whatever the two.

`Engine` and `Backend` answer each question with an exhaustive `match`, so a
new variant makes the compiler list what it must answer. Each module's rules
are specified in a `_spec.rs` beside it.

## Releasing

1. Set `version` in `Cargo.toml`, run `cargo check`, and get both onto `main`
   with CI green.
2. `git fetch origin && git tag vX.Y.Z origin/main && git push origin vX.Y.Z`

`release.yml` reruns CI on the tag and publishes the WASM, `SHA256SUMS`, a
build attestation and the notes in `.github/release-notes.md`. A failed tag
cannot be reused (tag ruleset, immutable releases): release the next patch.

Dependabot bumps the actions, crates, toolchain and Python lock. By hand:

- sqlc in `.prototools`, then `cargo xtask capture`;
- the ruff crates' `tag` in `Cargo.toml` (must match ruff in `tests/python/pyproject.toml`);
- uv's `required-version` in `tests/python/pyproject.toml`;
- PostgreSQL: `NEWEST` in `tests/python/postgres.py`, `POSTGRES` in `tests/python/capture.py`.
