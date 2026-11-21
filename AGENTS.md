# Project rules for agents

A sqlc codegen plugin, in Rust compiled to `wasm32-wasip1`, that generates Python 3.11+ for PostgreSQL, MySQL and SQLite. It builds a ruff Python AST and renders it with ruff's formatter. Read `docs/development.md` for the architecture, and `docs/testing.md` for how it is proven.

## Non-negotiables

- **Never commit** (no `git commit`, `merge`, `rebase`, `stash`, `tag`, `push`). The user commits. Leave work in the working tree.
- **The generated Python must be correct, typed and idiomatic**, for any project sqlc serves. A wrong type (a float for money, an `int` that is really `str | None`) is the worst kind of bug. Never paper over an unknown with `Any` silently; never weaken a check to make it pass.
- **Elegance is a requirement, not a nicety.** Smallest design that is correct. After making something work, re-read it and remove what is not needed: dead options, duplicated helpers, needless indirection, comments that restate code. Code should shrink as understanding grows.
- **Write code, comments and docs in the owner's taste**: the `code-taste` skill. Comments give a reason in plain "we" sentences, a doc block only says what the name and types cannot, and domain nouns are spelled out.
- **No backwards compatibility** with `sqlc-dev/sqlc-gen-python` or the codean fork: not their option names, not their output shape. They are reference material only.

## Sources of truth, most authoritative first

1. The sqlc plugin contract: `protos/sqlc.proto` (pinned to a sqlc release tag) and sqlc's runner behaviour (empty `plugin_options` bytes are valid; stderr is only shown on failure, so unsupported input must be an error, never a warning; file names must stay inside `out`; sqlc never deletes stale files).
2. What sqlc puts in the request: column order, nullability, param numbering, `embed_table`, `is_sqlc_slice`, `cmd`, `query.text`. Never re-infer these.
3. What a real PostgreSQL + driver returns at runtime. This beats any type map, including ours and the old plugins' (e.g. `money` comes back as `str`).
4. sqlc's Go codegen in its endtoend tests and its `internal/inflection` rules: cross-language conventions (model names, array depth, embed/slice shapes).
5. Strict Python tooling: pyright strict, `mypy --strict`, `ruff check` with the project's rule set, all targeting Python 3.11.

## Decided

- Backends: `sqlalchemy` (SQLAlchemy Core, every engine), `psycopg` (psycopg 3 raw cursors, PostgreSQL), `sqlite3` (`sqlite3`/`aiosqlite`) and `pymysql` (PyMySQL/asyncmy, MySQL: no other async MySQL driver is typed and right). `backend` and `async` are both required. Each query file is one module of functions taking `connection`, coroutines when `async: true`.
- Python 3.11 minimum, as `requires-python` in `tests/python/pyproject.toml` says, which every tool and the gate read: `X | None`, `list[T]`, `enum.StrEnum`, keyword-only dataclass fields, PascalCase classes, snake_case functions, UPPER_SNAKE constants and enum members, ruff-isort-clean import blocks.
- `:many` returns a `list`, and one that only reads also has a `stream_<query>`: a context manager of an iterator that reads each row as it is iterated, on every backend; `:one` returns its row, or `None` without one (`raise_on_none` raises instead: SQLAlchemy's `NoResultFound`, or `models.NoRowError` on a driver), and raises on a second row.
- A nullable parameter defaults to `None`, as `sqlc.narg` is how a query writes a filter without its constraint or a partial update; every other parameter, and every field of a record of results, has no default.
- We write `models.py` and the query modules into `out`, never `__init__.py`, as `out` may be the application's own package. `models.module` points query modules at models another entry of the same `backend` writes. `models.only` writes those models alone, ignoring the placeholder query sqlc requires. `models.omit_unused` writes only the classes the entry's queries use, and so reports only their problems.
- An unknown type is an error. An override naming `Any`, or `unknown_type: Any` or `object`, accepts it as the user's own choice in their `sqlc.yaml`; off by default, as sqlc shows a plugin no warning, so whoever turns it on knows what it hides.
- An uncast `coalesce`, `nullif` or `CASE` in a result is an error, as sqlc types it by one operand whatever the others are, and so is a result list we cannot read that holds one.
- Class names follow sqlc's own inflection (`media` -> `Medium`).
- Types follow driver reality; enums are coerced into the enum class.
- Records are frozen, keyword-only dataclasses, or with `records: pydantic` frozen `pydantic.BaseModel`s that forbid extra fields: one kind per entry, for models, `Row` and `Params` alike.
- `cargo test` stays hermetic: no Python, Docker or network. Python tooling lives in a uv project under `tests/python`; Docker is needed for `capture`, `verify`, CI, and a `bless` that changes anything, and they fail loudly without it.
- What sqlc sends a plugin for its own endtoend tests (pinned sqlc version) is committed as test input, with their MIT notice.

## Declined

Ideas we weighed and do not want, so nobody proposes them again:

- `Row` and `Params` named as the query spells itself (`GetUserByIDRow`), as they are the function's name in PascalCase (`GetUserByIdRow`), like every class we name.
- A runtime sweep on MySQL and SQLite, as their examples' scenarios run every function there.
- A BREAKING section in the release notes, as they list every commit since the last release.
- GitHub Actions pinned by digest, as Renovate holds each to a release a week old at least.
- README notes on prepared statements or PgBouncer, as the generated code prepares nothing: the driver does, by its own settings.
- attrs or msgspec records, as dataclasses and pydantic cover both kinds of record and pyright knows them.
- SQLite date converters registered on the driver, as they are global to the process.
- Indent or other formatting options, as ruff's formatter decides.
- A turso backend, as sqlc has no engine of its own for it.
- A shared `one` / `one_or_none` helper in `models.py`, as each `:one` reads its own rows.

## TODO

- **Converters on an override**: a `to_db` / `from_db` pair of dotted functions, so an override converts the value and not only names its type (a JSON column as a typed model, a SQLite `DATETIME` as `dt.datetime`). Applied inline in the generated code, never registered on the driver or at import; NULL-safe; per element of an array; `from_db` receives the value as our type map types it, so pyright checks the converter; validated at config time; proven at runtime in `verify`.

## Workarounds

Each is for someone else's bug, and goes when the bug does.

- **asyncmy's stubs** leave the parameters of `Cursor.execute`, `executemany` and `fetchmany` unannotated, so strict pyright rejects a call of them (`reportUnknownMemberType`). A query module on asyncmy calls them through a protocol of its own, `_typed(cur)` (`src/backend/dbapi.rs`).
  - Fixed upstream when, after an asyncmy upgrade, `cargo xtask verify --mutants` no longer catches "asyncmy's execute called without the bridge".
  - Then remove `Flavor::untyped`, `prelude` and `Body::typed` (`src/backend/dbapi.rs`), `Backend::prelude` and its call (`src/render/queries.rs`) if no other prelude is left, the names only they read (`src/python/vocabulary.rs`), `ASYNCMY` (`tests/rust/support/module.rs`), the mutant and their specs; make the fixed release the minimum asyncmy (in the README), and re-bless.

- **sqlc's SQLite parser** places each statement by characters but cuts the file by bytes, so after non-ASCII text every statement of the file is shifted and cut short ([sqlc#4235](https://github.com/sqlc-dev/sqlc/issues/4235)). We refuse such a file (`modules` in `src/api/queries.rs`).
  - Fixed upstream when, after a sqlc upgrade and `cargo xtask capture`, a SQLite query after a non-ASCII comment reaches us whole.
  - Then remove the check, its spec and the README limitation, and re-bless.

## Testing conventions

- Unit specs live in a `_spec.rs` sibling (`foo.rs` / `foo_spec.rs`, `mod.rs` / `spec.rs`). Each spec names one rule and checks it.
- Golden output pins exact bytes: each `examples/<example>/` is a sqlc project (`sqlc.yaml`, its SQL), and each `codegen` entry's `out` beside it holds what the plugin generates, which the tests compare against. What sqlc sends for each example is captured in `tests/sqlc/examples/`. Only re-bless when you have verified the new output is *more correct*, and say why. Never bless to make a red check green.
- Every rule in the generator should be traceable to a source of truth above, said in a comment where it is implemented and specified in its `_spec.rs`.
- The README is what users are promised: what a query returns, the options and the known limitations. Change it with the behaviour.
- An example's `out` directories hold only what the generator produces, and `cargo xtask bless` owns them. The invoicing cases are the acceptance fixture: `cargo xtask verify` proves them with the runtime suite in `tests/python/suite/`, and bless will not accept a change that it fails.
- An unknown or unprobed type is an error, never `Any`; the tests reject an `Any` field. A type joins an engine's map (`src/engine/*/types.rs`), or a backend's own (`src/backend/psycopg.rs`), only with driver evidence, proven by `tests/python/typemap.py`, which records it in `docs/typemap.md`: a Rust spec holds each map to an example with a column of every type in it.
