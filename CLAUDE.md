# Project rules for agents

A sqlc codegen plugin, in Rust compiled to `wasm32-wasip1`, that generates
Python 3.10+ for PostgreSQL, MySQL and SQLite. It builds a ruff Python AST
and renders it with ruff's formatter. Read `docs/development.md` for the architecture.

## Non-negotiables

- **Never commit** (no `git commit`, `merge`, `rebase`, `stash`, `tag`, `push`).
  The user commits. Leave work in the working tree.
- **The generated Python must be correct, typed and idiomatic.** It is used for
  an invoicing system. A wrong type (a float for money, an `int` that is really
  `str | None`) is the worst kind of bug. Never paper over an unknown with `Any`
  silently; never weaken a check to make it pass.
- **Elegance is a requirement, not a nicety.** Smallest design that is correct.
  After making something work, re-read it and remove what is not needed: dead
  options, duplicated helpers, needless indirection, comments that restate code.
  Code should shrink as understanding grows.
- **No backwards compatibility** with `sqlc-dev/sqlc-gen-python` or the codean
  fork: not their option names, not their output shape. They are reference
  material only.

## Sources of truth, most authoritative first

1. The sqlc plugin contract: `protos/sqlc.proto` (pinned to a sqlc release tag)
   and sqlc's runner behaviour (empty `plugin_options` bytes are valid; stderr is
   only shown on failure, so unsupported input must be an error, never a warning;
   file names must stay inside `out`; sqlc never deletes stale files).
2. What sqlc puts in the request: column order, nullability, param numbering,
   `embed_table`, `is_sqlc_slice`, `cmd`, `query.text`. Never re-infer these.
3. What a real PostgreSQL + driver returns at runtime. This beats any type map,
   including ours and the old plugins' (e.g. `money` comes back as `str`).
4. sqlc's Go codegen in its endtoend corpus and its `internal/inflection` rules:
   cross-language conventions (model names, array depth, embed/slice shapes).
5. Strict Python tooling: pyright strict, `mypy --strict`, `ruff check` with the
   project's rule set, all targeting Python 3.10.

## Decided

- Backends: `sqlalchemy` (SQLAlchemy Core, every engine), `psycopg` (psycopg 3
  raw cursors, PostgreSQL), `sqlite3` (`sqlite3`/`aiosqlite`) and `pymysql`
  (PyMySQL, MySQL, sync only: no async MySQL driver is typed and right).
  `backend` and `async` are both required. Each query file is one module of
  functions taking `conn`, coroutines when `async: true`.
- Python 3.10 minimum: `X | None`, `list[T]`, keyword-only dataclass fields,
  PascalCase classes, snake_case functions, UPPER_SNAKE constants and enum
  members, ruff-isort-clean import blocks.
- `:many` returns a `list`; `:one` returns its row and raises without one,
  as sqlc's Go returns `ErrNoRows` (SQLAlchemy's `NoResultFound`, or the
  package's own `NoRowError` on a driver), and on a second row too.
- Class names follow sqlc's own inflection (`media` -> `Medium`).
- Types follow driver reality; enums are coerced into the enum class.
- `cargo test` stays hermetic: no Python, Docker or network. Python tooling lives
  in a uv project under `tests/python`; Docker is needed for `capture`, `verify`,
  CI, and a `bless` that changes a case the runtime proof reads
  (`pgtypes_sqlalchemy`, `invoicing_*`, `ledger_*`), and they fail
  loudly without it.
- The sqlc endtoend corpus (pinned sqlc version) is committed as test input.

## Testing conventions

- Unit specs live in a `_spec.rs` sibling (`foo.rs` / `foo_spec.rs`, `mod.rs` /
  `spec.rs`). Each spec names one rule and checks it.
- Golden output pins exact bytes: each `examples/<example>/` is a sqlc
  project (`sqlc.yaml`, its SQL), and each `codegen` entry's `out` beside it
  holds what the plugin generates, which the tests compare against. What sqlc
  sends for each example is captured in `tests/sqlc/examples/`. Only re-bless when you
  have verified the new output is *more correct*, and say why. Never bless to
  make a red check green.
- Every rule in the generator should be traceable to a source of truth above.
- An example's `out` directories hold only what the generator produces, and
  `cargo xtask bless` owns them. The invoicing cases are the acceptance
  fixture: `cargo xtask verify` proves them with the runtime suite in
  `tests/python/suite/`, and bless will not accept a change to them, or to another
  case the runtime proof reads, that it fails.
- An unknown or unprobed type is an error, never `Any`; the tests reject an
  `Any` field. A type joins the map in `src/engine/postgresql/types.rs` only
  with driver evidence, recorded in `docs/python-output.md` §5 and proven by
  `tests/python/typemap.py`.
