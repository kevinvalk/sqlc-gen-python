# sqlc-gen-python

A [sqlc](https://sqlc.dev) codegen plugin that generates Python, written in Rust
and compiled to WASM.

It builds a [ruff](https://github.com/astral-sh/ruff) Python AST and hands it to
ruff to unparse and format, so the output is byte-identical to what `ruff format`
would produce — generated files never show up as a diff in your own formatting
step. The plugin is ~3 MB of WASM, against ~19 MB for the official Go plugin.

> **Status: work in progress.** Models and enums are generated; query modules are
> not yet. See [Roadmap](#roadmap).

## Usage

```yaml
version: "2"
plugins:
  - name: python
    wasm:
      url: file://./target/wasm32-wasip1/release/sqlc-gen-python.wasm
sql:
  - schema: schema.sql
    queries: query.sql
    engine: postgresql
    codegen:
      - out: src/authors
        plugin: python
        options:
          package: authors
```

## Options

Options are grouped by what they affect rather than flattened into one `emit_*`
namespace, and unknown keys are rejected — a typo in `sqlc.yaml` fails loudly
instead of silently doing nothing.

| Option | Default | Meaning |
| --- | --- | --- |
| `package` | last segment of `out` | Python package the generated modules live in. Query modules import their models from here. |
| `backend` | `sqlalchemy` | Database access layer to generate against. |
| `async` | `false` | Emit `async def` coroutines instead of blocking functions. |
| `omit_sqlc_version` | `false` | Leave sqlc's version out of the header, so output does not churn on upgrade. |
| `models.emit` | `true` | Whether to write the models module. |
| `models.file` | `models.py` | Filename for the models module. |
| `models.style` | `dataclass` | `dataclass` or `pydantic`. |
| `models.exact_table_names` | `false` | Name classes after the table verbatim, skipping singularisation. |
| `models.inflection_exclude` | `[]` | Table names that must not be singularised. |
| `queries.emit` | `true` | Whether to write query modules. |
| `queries.style` | `class` | `class` for methods on a `Querier`, `module` for free functions. |
| `queries.many` | `iterator` | How `:many` returns rows: `iterator` streams, `list` buffers. |
| `queries.parameter_limit` | `4` | Parameters a query may take before they are bundled into a params class. `0` always bundles. |
| `queries.style_classes` | `dataclass` | Representation for generated params and row classes. |
| `enums.style` | `str_mixin` | `str_mixin` for `(str, enum.Enum)`, `str_enum` for `enum.StrEnum` (Python 3.11+). |

This is **not** compatible with the option names used by
[`sqlc-dev/sqlc-gen-python`](https://github.com/sqlc-dev/sqlc-gen-python) or the
[`codean-io` fork](https://github.com/codean-io/sqlc-gen-python). The old
`emit_sync_querier` / `emit_async_querier` / `emit_module` flags are gone.

## Development

```sh
cargo test                                  # unit specs and golden tests
cargo wasm                                  # the WASM plugin
cargo clippy --all-targets                  # lint

UPDATE_GOLDEN=1 cargo test --test golden    # rewrite expected output
cargo test --test golden -- --ignored       # re-capture fixtures from sqlc
```

`cargo wasm` is an alias in `.cargo/config.toml`; there is no build system
beyond cargo.

### How the tests work

Unit tests live in a `_spec.rs` sibling of the module they specify
(`options.rs` / `options_spec.rs`), wired with
`#[cfg(test)] #[path = "options_spec.rs"] mod spec;`. They are written as
specifications — each names one rule and checks it — rather than as snapshots of
current behaviour. Exact generated output is pinned by the golden tests instead.

sqlc has a built-in `json` codegen that dumps the exact `GenerateRequest` a
plugin receives. `make fixtures` captures one per schema under
`tests/schemas/src/`, prunes the engine's own catalog schemas (2 MB down to
~15 KB), and commits the result.

Each case under `tests/cases/` names a fixture and the options to generate it
with, and holds the expected `.py` files alongside. So the tests run against real
sqlc output without sqlc or a WASM runtime in the loop, and **reviewing a change
means reading the diff of the generated Python** — the artifact that matters.
Every generated file is also re-parsed in-test, so invalid Python can never be
committed as the new expected output.

## Architecture

```
src/
  plugin.rs          the sqlc wire protocol, generated from protos/sqlc.proto
  config.rs          what the user asked for
  python.rs          writing Python: syntax, types, imports, rendering
  schema.rs          the catalog resolved into named classes and typed fields
  engine/            which Python type a column type maps to
    mod.rs  postgresql.rs  mysql.rs  sqlite.rs
  backend/           the database access layer generated code targets
    mod.rs  sqlalchemy.rs
  emit/              assembling files, one module per generated file
    mod.rs  models.rs
```

Every module has its spec beside it: `spec.rs` next to a `mod.rs`,
`<name>_spec.rs` next to `<name>.rs`.

The two folders are the project's two axes, and they are not the same kind of
thing. An **engine** only changes which Python type a column maps to — that is
one lookup table, which is why those files are short. A **backend** changes the
shape of the generated code: how a statement runs, what a connection is, what a
placeholder looks like. Adding PostgreSQL support to a new backend is real work;
adding a new engine to an existing backend is a table.

A type reference and its import are held as one value rather than two fields, so
`decimal.Decimal` and the `import decimal` that makes it resolve cannot drift
apart. That turns the engine tables into a plain vocabulary — `"numeric" =>
Type::DECIMAL` — and lets `FieldType` derive both its annotation and its imports
from one description of what the annotation is made of.

Case conversion is [`heck`] and singularisation is [`pluralizer`]; the only
hand-written naming left is a one-word correction to the latter.

[`heck`]: https://docs.rs/heck
[`pluralizer`]: https://docs.rs/pluralizer

`emit` builds nothing by hand: `emit::node` constructs ruff AST nodes,
`emit::imports` collects what each file depends on, and `emit::render` calls
ruff's `Generator` to unparse the tree and ruff's formatter to lay it out.

The one thing that is not a tree is the header. ruff's AST has nowhere to put a
`#` comment — `format_module_ast` recovers comments from `CommentRanges` into the
*source text*, and asserts every range maps onto real source — so the banner is
prepended to the formatted output as a string. Everything else the tree can
express goes through the AST, with schema documentation becoming a docstring
rather than a comment.

## Roadmap

- [x] Models, enums, arrays, multi-schema catalogs
- [x] PostgreSQL types; MySQL and SQLite skeletons
- [x] SQLAlchemy placeholder rewriting and connection types
- [ ] Query modules, SQLAlchemy sync and async
- [ ] `sqlc.embed`, `sqlc.slice`, `overrides`, `rename`
- [ ] MySQL and SQLite
- [ ] psycopg3 and asyncpg backends

## License

Dual-licensed:

- **Open source:** AGPLv3 — free to use, modify and share, as long as derivative
  works are also open.
- **Commercial:** available for closed-source or commercial use without AGPL
  obligations. Enquiries: kevin@valk.email

`SPDX-License-Identifier: AGPL-3.0-or-later OR LicenseRef-KevinValk-Commercial`
