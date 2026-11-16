# sqlc-gen-python

A [sqlc](https://sqlc.dev) plugin that generates typed Python 3.10+ for PostgreSQL, MySQL and SQLite.

> [!IMPORTANT]
> This was fully LLM written using Fable 5.2. I used all my expertise and knowledge to get this into the best shape I can imagine. For example, I let this be cross checked against all open-source projects using SQLC. So this might even be the "best" Python SQLC generator, but I feel you, and I don't even know what my own stance is on LLM software...

Tested on 1,166 open-source sqlc projects: 716 generate as they are, the rest get an error saying what to cast or override. All 2,874 generated packages pass ruff, pyright strict and mypy strict, and 1,847 ran on real databases with every value checked against its annotation (two [known limitations](#known-limitations) aside).

## Use

With the [latest release](https://github.com/kevinvalk/sqlc-gen-python/releases/latest), or the URL and sha256 of [`unstable`](https://github.com/kevinvalk/sqlc-gen-python/releases/tag/unstable), the latest commit that passed every check (its sha256 changes with each):

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
      - out: src/app/db
        plugin: python
        options:
          backend: psycopg
          async: false
```

`out` gets `models.py`, and a module of functions per query file, each taking the connection and its parameters by keyword. Every other file there, `__init__.py` included, is yours.

```python
import psycopg

from app.db import invoices

with psycopg.connect(DSN) as connection:
    invoice = invoices.get_invoice(connection, id=invoice_id)  # models.Invoice | None
```

The connection and its transaction are the caller's: nothing is committed.

| `backend`    | Engines                   | `async: true`     |
| ------------ | ------------------------- | ----------------- |
| `sqlalchemy` | PostgreSQL, MySQL, SQLite | yes               |
| `psycopg`    | PostgreSQL                | yes               |
| `sqlite3`    | SQLite                    | yes (`aiosqlite`) |
| `pymysql`    | MySQL                     | yes (asyncmy)     |

`backend` and `async` are required. See [`examples/`](examples/) for complete projects.

Each release is tested under the sqlc its notes name (v1.31.1 now), and the generated code on Python 3.10 and the newest, with psycopg 3.3 (libpq 14 at least), SQLAlchemy 2.0 and 2.1 (on psycopg, asyncpg, PyMySQL, asyncmy, `sqlite3` and aiosqlite), PyMySQL 1.2, asyncmy 0.2 (0.2.17 at least) and aiosqlite 0.22. Every type each engine maps is read back through each of its drivers, at its edges ([`docs/typemap.md`](docs/typemap.md)).

### Types are never guessed

The annotations are what the drivers return, and what the plugin cannot know is an error, not an `Any`, unless you set `unknown_type`. Two of them come up early:

- **`... has a type sqlc could not infer; add a cast`.** sqlc types an expression or a function's result by its form, not its operands (`sum(amount)` of a `numeric` as `bigint`). A cast says the type: `coalesce(sum(amount), 0)::numeric(12, 2)`. sqlc reports every cast NOT NULL, so coalesce what can be NULL ([known limitations](#known-limitations)).
- **`... has no verified Python mapping`.** Name the type with an override. pgvector's `vector`, which every driver returns as its text:

  ```yaml
  options:
    overrides:
      - db_type: vector
        python_type: str
  ```

  On SQLite that is every type of NUMERIC affinity (`DATETIME`, `TIMESTAMP`, `DECIMAL`), in which SQLite stores a value as the integer, real or text it looks like: declare the column `TEXT`, `INTEGER` or `REAL`, or override the type with what the application stores in it.

The plugin needs sqlc's static analysis: with a `database` block in `sqlc.yaml`, also set `analyzer: {database: false}`.

### What a query returns

| Command | Returns |
| --- | --- |
| `:one` | the row, or `None` without one (with `raise_on_none` it raises instead); a second row always raises |
| `:many` | a `list` of rows |
| `:exec` | `None` |
| `:execrows` | the number of rows, an `int` |
| `:execlastid` | the id MySQL generated, an `int` (MySQL only) |
| `:execresult` | the library's own result or cursor |
| `:copyfrom` | `None`, for `rows` of `<Query>Params` |
| `:batchexec`, `:batchone`, `:batchmany` | a `list` of one result per row (PostgreSQL only) |

A row of one column is its value, a row of exactly a table's columns is that table's model, and any other is a `<Query>Row` beside the function. Every one is a frozen, keyword-only dataclass, and every enum a `str` enum. `sqlc.embed(t)` is a field of `t`'s model, and `sqlc.slice` a `list`.

A nullable parameter, such as a `sqlc.narg`, defaults to `None`, so a filter (`sqlc.narg(ids) IS NULL OR ...`) or a partial update (`coalesce(sqlc.narg(name), name)`) takes only what the caller has, `**` a dict of it included. Every other parameter is required.

## Options

| Option | Default | |
| --- | --- | --- |
| `backend` | required | `sqlalchemy`, `psycopg`, `sqlite3` or `pymysql`, as in the table above |
| `async` | required | `true` makes every function a coroutine on the backend's async connection |
| `raise_on_none` | `false` | A `:one` without a row raises rather than return `None`: SQLAlchemy's `NoResultFound`, or `models.NoRowError` |
| `max_params` | none | A query with more parameters than this takes them as one `params: <Query>Params` record |
| `overrides` | `[]` | The Python type of a database type, `{db_type: vector, python_type: str}`, or of one column, `{column: invoices.total, python_type: ...}`, which wins. A dotted name, `app.types.Embedding`, is imported, and `Any` is yours to choose |
| `rename` | `{}` | Python names for SQL names: a table or enum (`schema.name`, or `name` in the default schema) to its class, a column or parameter to its name, an enum label to its member (`"+1": plus_one`) |
| `models.module` | none | Import the models from this module (`app.models`), written by another entry of the same schema and backend, and write the query modules alone, beside your own code |
| `models.only` | `false` | Write `models.py` alone, for the entries that set `models.module`. sqlc requires a query of every entry, so give this one a placeholder (`SELECT 1`), which writes nothing |
| `models.exact_table_names` | `false` | Name model classes after their table, without singularising |
| `models.inflection_exclude` | `[]` | Tables whose names are not singularised |
| `unknown_type` | none | Annotate every type the plugin cannot verify `Any`, or `object`, which you then narrow before use, instead of failing on it. A project can generate first and type as it goes |
| `omit_sqlc_version` | `false` | Leave sqlc's version out of each file's header |

An unknown option, a rename that matches nothing, and an override that names no type or column of the schema or the queries, is an error. Entries may share one list of options (a YAML anchor).

## Known limitations

What sqlc reports is never second-guessed, but where the statement proves sqlc wrong (a scalar subquery that finds no row, an aggregate of no rows, a column an outer join may leave empty) the type is widened to `| None`. What remains:

- **Computed values are NOT NULL to sqlc**, every cast included. `bio::text` of a nullable column is a `str` that returns `None`, and so is an aggregate of a group with nothing to aggregate, a `CASE` whose `ELSE` is not NULL, a `UNION` whose first branch is not nullable, a `coalesce` that starts with a NOT NULL column, `NULLIF`, the total rows of a `ROLLUP`, and a column of a parenthesised outer join. End with a default and cast: `coalesce(sum(x), 0)::numeric(12, 2)`.
- **A parameter inside an expression** takes sqlc's guess: `d + @days` is a `dt.date`, `(at, id) < (@at, @id)` makes both a `datetime`, and `issued_at::date = @day` a `datetime`. Cast the parameter: `@days::int`.
- **A computed column read through a view, a CTE or a subquery** looks like a declared one, so sqlc's guess at its type goes unnoticed. Cast it inside.
- **Arrays** may hold NULL elements and any number of dimensions, whatever the annotation says. Add a `CHECK` where it matters.
- **Type checkers accept** a `datetime` for a `date`, a `bool` for an `int`, a `Decimal` or `UUID` inside a JSON parameter (which raises, unless the engine's `json_serializer` takes it), and a record holding a list in a `set` (which raises).
- **A JSON parameter takes the document, not its text**: a `Mapping`, `list`, `tuple` or number, but no `str`, as one would be stored as a JSON string and a `json.dumps(...)` encoded twice. Pass the value, or `json.loads(text)`; a document that is a bare string needs an override.
- **Time**: an `interval` loses its months (`'1 mon'` is 30 days). psycopg and asyncpg disagree on a naive `datetime` and return a `timestamptz` in different zones: pass aware datetimes, and convert before `.date()`. Keep `'infinity'` out of a `timestamptz`.
- **A `:one` that writes** has written before it raises on a second row, and a batch is not atomic: run them in a transaction you can roll back.
- **psycopg** returns what the connection's adapters make: a loader registered on it (`register_hstore`, pgvector's) changes what the generated code gets. Its `:copyfrom` is a `COPY`, which fires no rule and copies into no view.
- **MySQL** returns a `FLOAT` to six digits, a `TIMESTAMP` naive in the session's zone, and a zero date as text; `:execrows` counts the rows changed, not matched, unless the connection sets `CLIENT.FOUND_ROWS` (SQLAlchemy does). A boolean that is NULL reads as `False`, and an asyncmy coroutine cancelled mid-statement leaves its connection unusable.
- **SQLite** types are affinities, not constraints: another program may store a `str` in an `INTEGER` column. Use `STRICT` tables, and declare a `TEXT` primary key `NOT NULL`, as SQLite lets it be NULL. A boolean that is NULL reads as `False`, as on MySQL.
- **An override's module** is sorted with the third-party imports: list it under ruff's `known-third-party`.

## Develop

```sh
cargo test
cargo xtask check
```

More in [`docs/development.md`](docs/development.md).

## License

Dual-licensed:

- **Open source:** AGPLv3 — free to use, modify and share, as long as derivative works are also open.
- **Commercial:** available for closed-source or commercial use without AGPL obligations. Enquiries: kevin@valk.email

`SPDX-License-Identifier: AGPL-3.0-or-later OR LicenseRef-KevinValk-Commercial`
