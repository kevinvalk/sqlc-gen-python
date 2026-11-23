# sqlc-gen-python

Write a named SQL query and call it as a typed Python function. This [sqlc](https://sqlc.dev) plugin generates Python 3.11+ for PostgreSQL, MySQL/MariaDB and SQLite.

> This was fully LLM written using Fable 5.2.

## Use

Start with a table and the query you want to call.

```sql
-- schema.sql
CREATE TABLE invoices (id bigint PRIMARY KEY, total numeric(12, 2) NOT NULL);
```

```sql
-- invoices.sql
-- name: GetInvoice :one
SELECT id, total FROM invoices WHERE id = $1;
```

In `sqlc.yaml`, pin the plugin and choose the connection you use. Here we use psycopg.

```yaml
version: "2"
plugins:
  - name: python
    wasm:
      url: https://github.com/kevinvalk/sqlc-gen-python/releases/download/v1.1.0/sqlc-gen-python.wasm
      sha256: 2aed95ea2e251ad62e5b35254b6a2aef7ad2c5407f2b36707483a3e2595bea54
sql:
  - engine: postgresql
    schema: schema.sql
    queries: invoices.sql
    codegen:
      - out: app/db
        plugin: python
        options:
          backend: psycopg
          async: false
```

With sqlc installed, run these commands beside `sqlc.yaml`.

```sh
python -m pip install 'psycopg[binary]'
sqlc generate
```

Set `DATABASE_URL` to a PostgreSQL database with that table. Pass your connection to the generated function. Your code owns the transaction.

```python
import os

import psycopg

from app.db import invoices

with psycopg.connect(os.environ["DATABASE_URL"]) as connection:
    invoice = invoices.get_invoice(connection, id=42)  # models.Invoice | None
    if invoice is not None:
        print(invoice.total)  # decimal.Decimal
```

The plugin writes `models.py` and one module per query file. Your application owns `__init__.py`. [Complete examples](examples/)

This README describes the current branch. For example, streaming and counted bulk inserts arrived after v1.1.0. Read the README at your release tag for its API. [Releases](https://github.com/kevinvalk/sqlc-gen-python/releases/latest) · [Latest passing build of main](https://github.com/kevinvalk/sqlc-gen-python/releases/tag/unstable)

## Options

Set these under `sql[].codegen[].options` in `sqlc.yaml`. Put the four `models.*` settings in a nested `models` mapping.

| Option | Default | What it does |
| --- | --- | --- |
| `backend` | Required | Use `sqlalchemy` on any supported engine, `psycopg` on PostgreSQL, `pymysql` on MySQL/MariaDB, or `sqlite3` on SQLite. |
| `async` | Required | Use `false` for synchronous calls or `true` for async calls. The native async drivers are psycopg, aiosqlite and asyncmy ≥ 0.2.17. |
| `records` | `dataclass` | Models, rows and parameter records are frozen, keyword-only dataclasses. Choose `pydantic` for frozen Pydantic models that forbid extra fields. |
| `raise_on_none` | `false` | Raise when a `:one` query returns no row. |
| `max_params` | Unset | Queries with more than this many parameters take one `params: <Query>Params` record. |
| `rename` | `{}` | Map SQL names to Python names for tables, enums, columns, parameters or enum members. |
| `overrides` | `[]` | Each rule sets a `python_type` for exactly one `db_type` or `column`. Pair `to_db` and `from_db` to convert values on writes and reads. |
| `unknown_type` | Unset | Set `object` or `Any` to accept a type the plugin cannot verify. By default, generation fails. |
| `models.module` | Unset | Write only query modules. Import models from the module another entry generates for the same schema and backend, e.g. `app.models`. |
| `models.only` | `false` | Write only `models.py`. Give sqlc a placeholder query such as `SELECT 1`. |
| `models.omit_unused` | `false` | Generate and validate only the models and enums this entry’s queries use. |
| `models.exact_table_names` | `false` | Use the table’s name for its model class, including the plural. |
| `omit_sqlc_version` | `false` | Omit the sqlc version from generated file headers. |

Choose at most one of `models.module`, `models.only: true` and `models.omit_unused: true`. If your sqlc entry has a `database` block, add `analyzer: {database: false}` alongside it so the plugin receives sqlc’s static analysis.

[Supported drivers](docs/reference.md#connections-and-drivers) · [Type overrides and converters](docs/reference.md#type-overrides-and-converters)

## What a query returns

A query selecting one column returns that column’s value per row. Selecting all of a table’s columns in order can return its model. Other multi-column results get a `<Query>Row`. Nullable parameters default to `None`.

| sqlc command | Python result |
| --- | --- |
| `:one` | One row, or `None` when absent. A second row raises. Set `raise_on_none` to raise on a missing row too. |
| `:many` | A list. Read-only queries also get a `stream_<query>` context manager for iterating one row at a time. |
| `:exec` / `:execrows` / `:execresult` | `None`, a row count, or the driver’s result or cursor, respectively. |
| `:execlastid` | The generated integer ID, or `0` when no row was inserted (MySQL). |
| `:copyfrom` | The number of rows written from an iterable of `<Query>Params` records. |
| `:batchexec` | `None` after executing the PostgreSQL batch. |
| `:batchone` / `:batchmany` | A list in input order, with one `:one` result or one list of rows per input (PostgreSQL). |

[Rows and parameters](docs/reference.md#rows-and-parameters) · [Streaming](docs/reference.md#streaming) · [Bulk inserts and timings](docs/reference.md#bulk-inserts)

## Known limitations

- **Ambiguous SQL types.** By default, generation fails on an ambiguous expression or an unverified database type. Cast the expression in SQL, e.g. `coalesce(sum(total), 0)::numeric`. For an unmapped database type, supply a type override.
- **PostgreSQL nullability.** With `CREATE TABLE AS`, inheritance or a computed view, sqlc can describe a nullable column exactly like a required one. Python can receive `None` in that field. Use `coalesce` in the query if the caller needs a value.
- **SQLite query text and storage.** Keep query files ASCII, including comments. sqlc can truncate statements after a non-ASCII character. Use `STRICT` tables for enforced storage types. Columns such as `DATETIME` and `DECIMAL` need a concrete storage type or an override.

[Engine-specific limitations and workarounds](docs/limitations.md) · [Types returned by each driver](docs/typemap.md)

## Migrating from another Python sqlc generator

We found 85 public Python projects using sqlc. In 26 distinct workloads, the project’s own tests passed and called generated queries. **25 passed the same tests after migration**, with 444 tests passing on each generator. All 25 needed a caller edit. The remaining project failed on Python 3.9. Neither MySQL project supplied Python tests, so we ran their queries on MySQL and MariaDB ourselves. [Migration effort and results by engine](docs/application-migration-audit.md#generation-by-engine)

We also ran SQL from 20 other projects through the generated Python. All 152 database and driver runs passed on populated databases. [Workloads and replay instructions](docs/downstream-workloads.md)

## Develop

Run `cargo test` for the Rust suite and `cargo xtask check` for the generated Python checks. [Architecture](docs/development.md) · [How we test](docs/testing.md)

## License

Choose the [AGPL-3.0-or-later](LICENSE.AGPL-3.0) or a [commercial license](LICENSE.LicenseRef-KevinValk-Commercial). For commercial licensing, contact kevin@valk.email. The generated code is yours and carries none of the plugin’s license terms.
