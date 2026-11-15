# sqlc-gen-python

A [sqlc](https://sqlc.dev) plugin that generates typed Python 3.10+ for
PostgreSQL, MySQL and SQLite.

## Use

Take the plugin URL and sha256 from a
[release](https://github.com/kevinvalk/sqlc-gen-python/releases/latest), or
from [`unstable`](https://github.com/kevinvalk/sqlc-gen-python/releases/tag/unstable),
the latest commit that passed every check:

```yaml
version: "2"
plugins:
  - name: python
    wasm:
      url: https://github.com/kevinvalk/sqlc-gen-python/releases/download/vX.Y.Z/sqlc-gen-python.wasm
      sha256: <from the release>
sql:
  - engine: postgresql
    schema: schema.sql
    queries: invoices.sql
    codegen:
      - out: src/app/db
        plugin: python
        options:
          backend: psycopg
          async: false
```

`out` becomes a package: `models.py`, and a module of functions per query
file, each taking the connection and its parameters by keyword.

```python
from app.db import invoices

with psycopg.connect(DSN) as conn:
    invoice = invoices.get_invoice(conn, id=invoice_id)  # models.Invoice | None
```

The connection and its transaction are the caller's: nothing is committed.

| `backend` | Engines | `async: true` |
|---|---|---|
| `sqlalchemy` | PostgreSQL, MySQL, SQLite | yes |
| `psycopg` | PostgreSQL | yes |
| `sqlite3` | SQLite | yes (`aiosqlite`) |
| `pymysql` | MySQL | yes (asyncmy) |

`backend` and `async` are required. See [`examples/`](examples/) for complete
projects, and [`docs/python-output.md`](docs/python-output.md) for every rule
and option.

Each release is tested under the sqlc its notes name (v1.31.1 now), and the
generated code on Python 3.10 and the newest, with psycopg 3.3 (libpq 14 at
least), SQLAlchemy 2.0 and 2.1 (on psycopg, asyncpg, PyMySQL, asyncmy,
`sqlite3` and aiosqlite), PyMySQL 1.2, asyncmy 0.2 and aiosqlite 0.22. Every type each
engine maps is read back through each of its drivers, at its edges
([`docs/typemap.md`](docs/typemap.md)).

### Types are never guessed

The annotations are what the drivers return, and what the plugin cannot know
is an error, not an `Any`. Two of them come up early:

- **`... has a type sqlc could not infer; add a cast`.** sqlc types an
  expression or a function's result by its form, not its operands
  (`sum(amount)` of a `numeric` as `bigint`). A cast says the type:
  `coalesce(sum(amount), 0)::numeric(12, 2)`. sqlc reports every cast NOT
  NULL, so coalesce what can be NULL
  ([known limitations](docs/python-output.md#known-limitations)).
- **`... has no verified Python mapping`.** Name the type with an override.
  pgvector's `vector`, which every driver returns as its text:

  ```yaml
          options:
            overrides:
              - db_type: vector
                python_type: str
  ```

The plugin needs sqlc's static analysis: with a `database` block in
`sqlc.yaml`, also set `analyzer: {database: false}`.

## Develop

```sh
cargo test
cargo xtask check
```

More in [`docs/development.md`](docs/development.md).

## License

Dual-licensed:

- **Open source:** AGPLv3 — free to use, modify and share, as long as derivative
  works are also open.
- **Commercial:** available for closed-source or commercial use without AGPL
  obligations. Enquiries: kevin@valk.email

`SPDX-License-Identifier: AGPL-3.0-or-later OR LicenseRef-KevinValk-Commercial`
