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
| `tests/sqlc/` | what sqlc sends the plugin, for `examples/` and for sqlc's endtoend tests (`endtoend/`) |
| `tests/rust/` | `cargo test`: the examples byte for byte, and invariants over everything in `tests/sqlc/` |
| `tests/python/` | type-checks and runs the generated code |
| `docs/typemap.md` | what each driver returns for each mapped type, written by `cargo xtask verify` |
| `docs/testing.md` | what the tests read, and what each layer proves |

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

  sqlc_typing/     the SQL facts the Python API needs for typing
    mod.rs           starts each result with the columns sqlc reported
    inference/       reads SQL for result types, row and embed presence, array elements and read-only statements
      select/          follows projections through joins, CTEs, aggregates and set operations
    mysql.rs         distinguishes SET columns from ENUM using the reported length and labels
    sqlc_fixes/
      mod.rs           rejects SQLite query files whose statement offsets sqlc has shifted
      bindings.rs      detects unexpanded macros and explains mismatches in the reported placeholders
      parameters.rs    checks parameter types and binds each reported use to its placeholder number
      types.rs         distinguishes declared types from guesses and diagnoses ambiguous expressions
    tokens.rs        token navigation shared by result and parameter inference

  engine/          the database dialect and its Python type map
    mod.rs           `Engine`, one variant per engine sqlc analyses for; `Sql`
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

- **engine** owns each dialect's lexer, placeholder binding and driver-verified type map.
- **backend**: the Python library (`backend:`): how a statement is written and run, and the few types its drivers disagree on.
- **api** combines the corrected request, the engine's type map and the backend's capabilities. It resolves names, records and query signatures before `render` writes any files.

### When sqlc changes

We take column order, parameter numbers, command and catalog from the request. `sqlc_typing::QueryResult::read` adds one entry per result column and calls `inference::refine` once. A default entry uses the reported column as received. With no further evidence a `:one` can return no row, an embedded table can be missing and an array can contain NULL. A new correction needs a captured request and a database result that disagree. A new inference for an omitted field needs a generated Python signature that depends on it.

| Why we read more | Example on pinned sqlc | When we can remove it |
| --- | --- | --- |
| A reported field disagrees with the database | `GetCustomerContact` reports `contact_address.not_null = true` through two CTEs. PostgreSQL returns NULL for a customer without an email. | Recapture the request on the new pin, remove the matching projection rule, and keep the nullable and populated cases in the runtime suite. |
| sqlc guesses or omits a type | MySQL's `FilterCustomers` gives the same named parameter the types `any` and `varchar` at its two uses. Some PostgreSQL row comparisons get a different column's type. | Once the uses carry their correct types, remove the matching recovery or diagnostic. The wire tests cover both the ambiguous predicates and the valid ones. |
| sqlc supplies a plausible but incorrect expression type | An uncast `coalesce(quantity, 0.5)` can take its reported type from the integer column alone. A numeric `sum` can get the first overload’s result type. | When the analyzer resolves the operands and exposes reliable result types, remove the corresponding cast diagnostic and test the uncast expression against the driver. |
| The protocol has no field for a fact our Python API uses | An ungrouped `count(*)` returns a row even for an empty table. An array can be non-NULL and still contain NULL elements. A `:many` can read rows or write and return them. | Read row presence, element nullability or statement kind from the protocol as sqlc adds each one. Removing these inferences earlier widens Python signatures and needs an explicit API decision. |
| A catalog entry represents two distinct database types | MySQL represents SET and ENUM as catalog enums. A multi-label column’s length distinguishes the two. A slice parameter reports length `-1` even when its source column has a length. | The wire test flags a change to the missing slice length. Remove that lookup when sqlc supplies the length, and remove the whole distinction in `mysql.rs` when the request names the kind. |
| sqlc leaves a bind in the SQL or reports it in the wrong position | Unexpanded macros and SQLite’s mixed numbered and anonymous binds can disagree with the parameter list. | Retire the affected diagnostic once the recaptured SQL and binds agree. The dialect lexer still has to match each placeholder to its argument. |
| sqlc emits damaged SQL text | SQLite statements after a UTF-8 comment can lose their final characters, including a bind placeholder. | `cargo xtask capture` checks the original text of two statements. When both arrive whole on the supported version, make that the capture expectation and remove `checked_queries` and the ASCII restriction. [Upstream regression test](https://github.com/sqlc-dev/sqlc/pull/4568) |

On an upgrade, run `cargo xtask capture` and `cargo test`. The raw-request tests in `tests/rust/golden.rs` flag changes to the CTE and repeated-parameter examples even if the generated files are identical. Rerun the runtime cases with the candidate rule removed before retiring it. If upstream supplies all the result facts, replace the call to `inference::refine` and delete the private `inference/` directory. The engine still owns placeholder binding and the driver type map.

Use the boundary when reviewing a typing change. Whether sqlc guessed a type, lost the length of a MySQL SET, or promoted a nullable source column belongs in `sqlc_typing`. What a Python driver returns for PostgreSQL `money`, SQLite storage classes or an enum array belongs in the engine or backend. The API owns explicit overrides, sharing a Python argument across compatible uses, and constructing an absent embedded row from the returned values. These decisions still need tests with a fully accurate sqlc request.

We never write generated Python as text. Every expression and statement is a ruff AST node (`python/syntax.rs`), so a construct that is not Python does not compile, and schema data can only ever end up as a name or a string. A name the generated code reads or binds itself (`cur`, `Sequence`, `from_row`) is a constant in `python/vocabulary.rs`, so it reads as the name it is: `CUR.attr("execute").call([statement])`. Only a library's own member (`"execute"`) is spelled where it is used, in the backend that calls it.

What a tree cannot hold is the one part that is text: the comments that open a file (the header, and `# ruff: noqa: S105` where a statement's name would trip it), the blank line between import sections and after a top-level `if`, and how a docstring or a multi-line SQL string is spelled. We parse the expressions an override supplies, and the quoted driver types in `typing.cast` so their imports go under `typing.TYPE_CHECKING`. We parse the unparsed module to find its strings and let ruff format it.

Where engines or backends differ, `Engine` and `Backend` answer with a `match` that names every variant (a driver's backend through its `dbapi::Driver`), so adding one makes the compiler list everything it has to answer. The rules of each module are specified in a `_spec.rs` beside it.

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
