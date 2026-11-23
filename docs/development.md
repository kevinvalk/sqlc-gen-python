# Development

| Command | When to use it |
| --- | --- |
| `cargo test` | Run the Rust tests locally. They need no Python, Docker or network. |
| `cargo xtask check` | Check formatting and lint, run the Rust tests and check the generated Python and test scripts. |
| `cargo xtask verify` | Run the Python against PostgreSQL, MySQL and SQLite. The default setup uses Docker. |
| `cargo xtask bless` | Update expected output in `examples/` and the end-to-end ledger after reviewing what changed. |
| `cargo xtask capture` | Refresh `tests/sqlc/` from the pinned sqlc and run the WASM through sqlc. This needs network access. |
| `cargo wasm` | Build the WASM plugin. |

The `xtask/` Rust program runs these checks. It uses uv for the Python tools. [Test inputs and checks](testing.md) · [Cargo xtask convention](https://github.com/matklad/cargo-xtask) · [uv](https://docs.astral.sh/uv/)

To investigate one query, start with what sqlc sent. Put this `sqlc.yaml` beside `schema.sql` and `query.sql`. The first command writes sqlc’s request as JSON. The second runs this checkout against that request.

```yaml
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

sqlc sends a request on stdin. `generate` reads the options, resolves the Python API, then renders it. If a query gets the wrong type, inspect the request and the resolved API before changing how the function is written. [Entry point](../src/lib.rs) · [Plugin protocol](../protos/sqlc.proto)

| Where | What belongs here |
| --- | --- |
| [sqlc_typing](../src/sqlc_typing/) | Facts that sqlc omits or misreports. `inference/select/` follows result columns through joins, CTEs and aggregates. `sqlc_fixes/` handles ambiguous types, misreported parameters and damaged SQL. |
| [engine](../src/engine/) | Each dialect’s lexer, placeholders and driver-verified Python type map. |
| [backend](../src/backend/) | How the Python library executes a query and which types its driver returns differently. SQLAlchemy rewrites statements for `text()`. The native drivers share function bodies in `dbapi.rs`. |
| [api](../src/api/) | The names, models, parameters and return types the caller sees. This is where we apply explicit overrides, match projections to table models and singularise names as sqlc does. |
| [render](../src/render/) | The resolved API as Python modules, including model definitions, query functions and file headers. |
| [python](../src/python/) | Ruff syntax nodes, Python names, annotations, imports and formatting. |

We build each Python expression and statement as a Ruff syntax node. Names owned by the generated code live in `python/vocabulary.rs`. For example, `CUR.attr("execute")` uses our name for the cursor and the driver’s own method name. We parse override annotations as Python expressions. For quoted types in `typing.cast`, we collect imports under `typing.TYPE_CHECKING`. The renderer handles header comments, import spacing, docstrings and multiline SQL, then Ruff formats the module.

`Engine` and `Backend` use exhaustive matches so adding a variant points to every decision it needs. Each module’s `_spec.rs` records its rules.

### When sqlc changes

Use sqlc’s column order, parameter numbers, command and catalog. For row-returning queries, `QueryResult::read` starts with one entry per reported column. `inference::refine` adds facts the Python signature needs. Without evidence to narrow the type, a `:one` can return no row, an embedded table can be absent and an array can contain NULL elements. [Result inference](../src/sqlc_typing/mod.rs)

Before adding a correction, capture the request and reproduce the disagreement on the database. For a fact the protocol omits, identify the Python signature that depends on it. A driver returning PostgreSQL `money` as text belongs in the type map even if sqlc reports the database type perfectly.

| What sqlc sends today | When to remove the rule |
| --- | --- |
| `GetCustomerContact` marks `contact_address` required through two CTEs. PostgreSQL returns NULL for a customer without an email. | Once sqlc marks it nullable, remove the projection correction and rerun both the NULL and populated cases. |
| MySQL’s `FilterCustomers` reports one named parameter as both `any` and `varchar`. In some PostgreSQL row comparisons a parameter gets a different column’s type. | Once the uses have the correct type, remove the recovery or diagnostic. Run both valid and ambiguous predicates through sqlc. |
| An uncast `coalesce(quantity, 0.5)` can get the type of the integer column. A numeric `sum` can get the result type of the first overload. | When sqlc resolves the operand types, remove the cast diagnostic and run the uncast expression against the driver. |
| The protocol omits row presence, array element nullability and whether a `:many` only reads. An ungrouped `count(*)`, for example, returns a row even on an empty table. | Use each fact as the protocol adds it. Removing an inference before then can widen a return type or change which queries have streams. |
| MySQL represents `SET` and `ENUM` as catalog enums. We use length to distinguish multi-label columns, but a slice parameter reports length `-1`. | Once sqlc reports the slice length, remove the column lookup. Once it names the type, remove the distinction in `mysql.rs`. |
| An unexpanded macro or mixed numbered and anonymous SQLite placeholders can disagree with the parameter list. | Remove the diagnostic when the captured SQL and parameter list agree. The lexer still matches placeholders to arguments. |
| On SQLite, `x & 6`, `x \| 8` and the shifts are reported as `bool`, like a comparison. MySQL reports a binary literal (`0x41`) as text. | Once sqlc types them as an integer and as bytes, remove the two diagnostics in `inference/select/mod.rs` and read the values back through each driver. |
| SQLite cuts statements short after a UTF-8 comment and can drop a placeholder. | When both statements in the capture arrive whole, update the capture expectation and remove `checked_queries` and the ASCII restriction. [Upstream regression test](https://github.com/sqlc-dev/sqlc/pull/4568) |

On a sqlc upgrade, run `cargo xtask capture` and `cargo test`. The raw-request tests catch changes to the CTE and repeated-parameter cases even if the generated Python is identical. Remove the affected rule and rerun its database cases. If sqlc supplies all the result facts, `inference::refine` and its private `inference/` directory can go. Driver mappings still belong in the engine and backend. The API still owns explicit overrides, shared Python arguments and the construction of embedded rows. [Raw-request tests](../tests/rust/golden.rs)

## Releasing

Each passing commit on `main` publishes the moving `unstable` tag. If Git reports “would clobber existing tag”, configure the clone once to allow that tag to move.

```sh
git config --add remote.origin.fetch '+refs/tags/unstable:refs/tags/unstable'
```

To release a version, tag the commit on `main`.

```sh
git fetch origin && git tag vX.Y.Z origin/main && git push origin vX.Y.Z
```

CI publishes the WASM, its build attestation and the release notes, then updates the pinned URL and SHA-256 in the README. [Release notes template](../.github/release-notes.md)

Renovate updates direct dependencies after a release is at least a week old. Once CI passes, it merges the update to `main`. A failed update opens a pull request. These upgrades also require local work. [Renovate configuration](../.github/renovate.json5)

- For sqlc, update `POSTGRES` in `tests/python/capture.py` to the version upstream tests use and run `cargo xtask capture`.
- For Ruff, update the crate tag and Python pin together. Run `cargo update`, review any formatter changes, then run `cargo xtask bless` if needed.
- For a PostgreSQL, MySQL or driver minor-version change, run `cargo xtask verify` to refresh the driver type map.
- For a uv minor-version change, widen `required-version` in `tests/python/pyproject.toml`.
