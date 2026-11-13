# sqlc-gen-python

A [sqlc](https://sqlc.dev) plugin that generates typed Python 3.10+ for
PostgreSQL, MySQL and SQLite.

## Use

Take the plugin URL and sha256 from a
[release](https://github.com/kevinvalk/sqlc-gen-python/releases/latest):

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
    queries: query.sql
    codegen:
      - out: src/app/db
        plugin: python
        options:
          backend: psycopg
          async: false
```

```python
from app.db import invoices

with psycopg.connect(DSN) as conn:
    invoice = invoices.get_invoice(conn, id=invoice_id)
```

| `backend` | Engines | `async: true` |
|---|---|---|
| `sqlalchemy` | PostgreSQL, MySQL, SQLite | yes |
| `psycopg` | PostgreSQL | yes |
| `sqlite3` | SQLite | yes (`aiosqlite`) |
| `pymysql` | MySQL | no |

`backend` and `async` are required. See [`examples/`](examples/) for complete
projects, and [`docs/python-output.md`](docs/python-output.md) for every rule
and option.

A type the plugin has no Python type for is an error; name one with an
override. pgvector's `vector`, which every driver returns as its text:

```yaml
        options:
          overrides:
            - db_type: vector
              python_type: str
```

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
