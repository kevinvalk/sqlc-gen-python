# sqlc-gen-python

A [sqlc](https://sqlc.dev) plugin that generates typed Python 3.11+ for PostgreSQL, MySQL and SQLite.

> [!IMPORTANT]
> This was fully LLM written using Fable 5.2. I used all my expertise and knowledge to get this into the best shape I can imagine. For example, I let this be cross checked against all open-source projects using SQLC. So this might even be the "best" Python SQLC generator, but I feel you, and I don't even know what my own stance is on LLM software...

v1.0.0 was tested on 1,166 open-source sqlc projects: each generated as it was, or got an error saying what to cast or override. All 2,874 packages it generated passed ruff, pyright strict and mypy strict, and 1,847 ran on real databases with every value checked against its annotation (two [known limitations](#known-limitations) aside).

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

There is no `asyncpg` backend: it is somewhat faster, but async only, untyped and rarely released, while psycopg serves sync and async with one typed API and has the `COPY`, pipelines and server-side cursors the batches and streams need. On SQLAlchemy, asyncpg is a driver like any other.

Each release is tested under the sqlc its notes name (v1.31.1 now), and the generated code on Python 3.11 and the newest, with psycopg 3.3 (libpq 14 at least, for pipelines), SQLAlchemy 2.0 and 2.1 (on psycopg, asyncpg, PyMySQL, asyncmy, `sqlite3` and aiosqlite), PyMySQL 1.2, asyncmy 0.2 (0.2.17 at least, the first to bind a negative `timedelta`), aiosqlite 0.22 and pydantic 2.14. Every type each engine maps is read back through each of its drivers, at its edges ([`docs/typemap.md`](docs/typemap.md)).

### Types are never guessed

The annotations are what the drivers return, and what the plugin cannot know is an error, not an `Any`, unless you set `unknown_type`. Two of them come up early:

- **`...; add a cast`.** sqlc types an expression or a function's result by its form, not its operands: `sum(amount)` of a `numeric` as `bigint`, `nullif(label, '')` as `bool`, a `coalesce` as its first argument and a `CASE` as its `ELSE` (we accept one whose every branch is a string, or on PostgreSQL every one `TRUE` or `FALSE`, as its type is then the `ELSE`'s). A cast says the type: `coalesce(sum(amount), 0)::numeric(12, 2)`. sqlc reports every cast NOT NULL, so coalesce what can be NULL ([known limitations](#known-limitations)).
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
| `:one` | the row, or `None` without one (with `raise_on_none` it raises instead); a second row always raises: SQLAlchemy's `MultipleResultsFound`, or `models.MultipleRowsError` |
| `:many` | a `list` of rows |
| `:exec` | `None` |
| `:execrows` | the number of rows, an `int` |
| `:execlastid` | the id MySQL generated, an `int`, or `0` when it inserted no row (MySQL only) |
| `:execresult` | the library's own result or cursor |
| `:copyfrom` | how many rows it wrote, an `int`, for `rows` of `<Query>Params` |
| `:batchexec`, `:batchone`, `:batchmany` | a `list` of one result per row (PostgreSQL only) |

A row of one column is its value, so a `:one` of a nullable column returns `None` for a NULL and for no row alike (`raise_on_none` tells them apart). A row of exactly a table's columns is that table's model, and any other is a `<Query>Row` beside the function. Every one is a frozen, keyword-only dataclass (or a pydantic model, with `records: pydantic`), and every enum an `enum.StrEnum`. `sqlc.embed(t)` is a field of `t`'s model, and `sqlc.slice` a `list`.

A `:many` that only reads also has a `stream_<query>`, which reads each row as you iterate instead of all of them first, and closes its cursor when you leave it:

```python
with invoices.stream_list_invoices_by_status(connection, statuses=[InvoiceStatus.PAID]) as rows:
    for row in rows:
        export(row)
```

With `async: true` it is `async with` and `async for`. On PostgreSQL a stream needs a transaction, and on psycopg one of each stream is open at a time per connection: opening it again inside itself raises `DuplicateCursor`, which aborts the transaction. On MySQL a statement on the connection before you leave a stream ends the stream early, with only a `UserWarning`: run it on a second connection, or read the list function instead.

A nullable parameter, such as a `sqlc.narg`, defaults to `None`, so a filter or a partial update (`coalesce(sqlc.narg(name), name)`) takes only what the caller has, `**` a dict of it included. Every other parameter is required. On PostgreSQL and SQLite sqlc types a parameter by its first use, so a filter casts it there: `(sqlc.narg(status)::invoice_status IS NULL OR status = sqlc.narg(status))`, or `CAST(sqlc.narg(name) AS TEXT) IS NULL` on SQLite. On MySQL `(sqlc.narg(name) IS NULL OR name = sqlc.narg(name))` needs no cast. To make a parameter of a nullable column required, cast it (`@paid_at::timestamptz`): sqlc reports every cast NOT NULL, so it then takes a `dt.datetime` and never `None`.

### Writing many rows with `:copyfrom`

A `:copyfrom` of a plain `INSERT INTO t (a, b) VALUES (@a, @b)` writes all its `rows` in as few round trips as its library allows. Writing 10,000 rows of four columns in one transaction (Python 3.11, November 2026, the database in Docker on the same machine):

| Engine | Backend, on driver | `:copyfrom` runs | `:copyfrom` | A `:exec` per row |
| --- | --- | --- | --- | --- |
| PostgreSQL | `psycopg` | `COPY ... FROM STDIN` | 9 ms | 3.5 s |
| PostgreSQL | `psycopg` async | `COPY ... FROM STDIN` | 9 ms | 3.7 s |
| PostgreSQL | `sqlalchemy` on psycopg | `executemany` | 130 ms | 3.8 s |
| PostgreSQL | `sqlalchemy` async, on asyncpg | `executemany` | 50 ms | 5.8 s |
| PostgreSQL | `sqlalchemy` async, on psycopg | `executemany` | 610 ms | 6.2 s |
| MySQL | `pymysql` | `INSERT`s of many rows | 120 ms | 4.8 s |
| MySQL | `pymysql` async, on asyncmy | `INSERT`s of many rows | 110 ms | 3.9 s |
| MySQL | `sqlalchemy` on PyMySQL | `INSERT`s of many rows | 120 ms | 4.0 s |
| MySQL | `sqlalchemy` async, on asyncmy | `INSERT`s of many rows | 100 ms | 4.4 s |
| SQLite | `sqlite3` | `executemany` | 6 ms | 8 ms |
| SQLite | `sqlite3` async, on aiosqlite | `executemany` | 5 ms | 290 ms |
| SQLite | `sqlalchemy` | `executemany` | 19 ms | 79 ms |
| SQLite | `sqlalchemy` async, on aiosqlite | `executemany` | 19 ms | 1.1 s |

We take each library's fastest path that keeps an `INSERT`'s guarantees, so not MySQL's `LOAD DATA`, and no `COPY` for an `INSERT ... OVERRIDING USER VALUE`, whose identity values `COPY` would store. Only psycopg's `COPY` reads `rows` as it goes and writes all or none: elsewhere, batch a large load and run it in a transaction.

## Options

| Option | Default | |
| --- | --- | --- |
| `backend` | required | `sqlalchemy`, `psycopg`, `sqlite3` or `pymysql`, as in the table above |
| `async` | required | `true` makes every function a coroutine on the backend's async connection |
| `raise_on_none` | `false` | A `:one` without a row raises rather than return `None`: SQLAlchemy's `NoResultFound`, or `models.NoRowError` |
| `max_params` | none | A query with more parameters than this takes them as one `params: <Query>Params` record |
| `overrides` | `[]` | The Python type of a database type, `{db_type: vector, python_type: str}`, or of a column, `{column: invoices.total, python_type: ...}`, which wins and reaches a parameter tied to it, but not a cast one (`@total::numeric`). An override names the type and converts nothing, so it names what the driver returns: `{db_type: numeric, python_type: float}` still holds a `Decimal`, and on SQLite, where that is a `str`, `int` or `float`, a `bool` override of an `INTEGER` column holds `1`. On PostgreSQL a type goes by its own name or its standard one (`int8` or `bigint`, which is a `bigserial` column's too), but not by `decimal`, `char`, `float` or `bit varying`; on MySQL and SQLite by the name sqlc reports. Its override never reaches a parameter sqlc could type at none of its uses, which still needs its cast, nor the `integer` of a `LIMIT` or an `OFFSET`. A dotted name (`app.types.Embedding`) is imported |
| `rename` | `{}` | Python names for SQL names: a table or enum (`schema.name`, or `name` in the default schema) to its class, a column or parameter to its name, an enum label to its member (`"+1": plus_one`) |
| `models.module` | none | Import the models from this module (`app.models`), which another entry writes, and write the query modules alone |
| `models.only` | `false` | Write `models.py` alone, for the entries of the same `backend` that set `models.module`, as a model reads what its backend returns. Give it a placeholder query (`SELECT 1`), as sqlc requires one |
| `models.omit_unused` | `false` | Write only the models and enums this entry's queries use, and report only their problems. Not with `models.only` or `models.module` |
| `models.exact_table_names` | `false` | Name model classes after their table, without singularising; `rename` names one table's class |
| `records` | `dataclass` | `pydantic` makes every model, `Row` and `Params` a frozen `pydantic.BaseModel` that forbids unknown fields. A field that would hide a `BaseModel` attribute takes a trailing `_` (`json_`), and so does a parameter of that name, which keeps its name in a `Params` record |
| `unknown_type` | none | Annotate every type the plugin cannot verify `Any` or `object` instead of failing on it |
| `omit_sqlc_version` | `false` | Leave sqlc's version out of each file's header |

An unknown option, a rename that matches nothing, and an override that names no type or column of the schema or the queries, is an error. Entries may share one list of options (a YAML anchor), but an entry that sets `models.only` reads no query, so there a `rename` of a query's alias or parameter is an error.

## Known limitations

What sqlc reports is never second-guessed, but where the statement proves sqlc wrong (a scalar subquery that finds no row, an aggregate of no rows, a column an outer join may leave empty) the type is widened to `| None`. What remains:

- **Computed values are NOT NULL to sqlc**, every cast included, so `bio::text` of a nullable column, an aggregate of a nullable column in a group, a window function such as `lag`, or a `coalesce` without a default may still return `None`. SQLite and MySQL meet this more often, as there most computed columns need a cast. And nothing is widened inside a CTE or a subquery: a value computed there, a key its `ROLLUP` leaves out, or a column of a `RIGHT`, `FULL` or parenthesised join is annotated as sqlc reports it. End with a default and cast: `coalesce(sum(x), 0)::numeric(12, 2)`.
- **A parameter inside an expression** takes sqlc's guess: `d + @days` is a `dt.date`, `(at, id) < (@at, @id)` makes both a `datetime`, `issued_at::date = @day` a `datetime`, `settings ? @key` a JSON document and `tags[@i]` a `list[str]`. Cast the parameter: `@days::int`, `@key::text`.
- **A computed column read through a view, a CTE or a subquery** looks like a declared one, so sqlc's guess at its type goes unnoticed: there a `coalesce` has the type of its first argument, a `NULLIF` is a `bool` and a `CASE` has the type of its `ELSE`. Cast it inside.
- **A `UNION` is its first branch** to sqlc, whatever a later one returns: an `int` column over a `numeric` one is annotated `int`, and a NOT NULL column over a nullable one never `None`. Cast the first branch to the type the whole returns, and give a nullable later branch a default.
- **Arrays** may hold NULL elements and any number of dimensions, whatever the annotation says, and an array of an enum raises `ValueError` on either. Add a `CHECK` where it matters.
- **Type checkers accept** a `datetime` for a `date`, a `bool` for an `int`, a `Decimal` or `UUID` inside a PostgreSQL JSON parameter, and a record holding a list in a `set`; the last two raise.
- **A JSON parameter on PostgreSQL takes the document, not its text**: no `str`, as one would be stored as a JSON string. Pass the value, or `json.loads(text)`. `None` is SQL NULL for a nullable parameter. For any other, psycopg stores the JSON `null` and SQLAlchemy sends NULL, which a NOT NULL column refuses.
- **Time**: an `interval` loses its months (`'1 mon'` is 30 days). psycopg and asyncpg disagree on a naive `datetime` and return a `timestamptz` in different zones: pass aware datetimes, and convert before `.date()`. Keep `'infinity'` out of a `timestamptz`.
- **sqlc's catalog** calls a `CREATE TABLE AS` column NOT NULL when its source is, and a parent's column NOT NULL when an `INHERITS` child declares it so: both may be `None`.
- **An aggregate inside a set-returning function**, as `unnest(array_agg(id))`, reads as the one row an aggregate returns, so its `:one` raises without a row, `raise_on_none` or not.
- **A `:one` that writes** has written before it raises on a second row, and a batch is not atomic: run them in a transaction you can roll back.
- **psycopg** returns what the connection's adapters make: a loader registered on it (`register_hstore`, pgvector's) changes what the generated code gets. Its `COPY` fires no rule and copies into no view, and neither a `COPY` nor a stream runs inside a pipeline of the caller's (`connection.pipeline()`).
- **MySQL** returns a `FLOAT` to six digits, a `TIMESTAMP` naive in the session's zone, and a zero date as text; `:execrows` counts the rows changed, not matched, unless the connection sets `CLIENT.FOUND_ROWS` (SQLAlchemy does). A boolean sqlc calls NOT NULL, such as a computed one, reads a NULL as `False`, and an asyncmy coroutine cancelled mid-statement leaves its connection unusable. A `JSON` column is its text, a `str` both ways: `json.dumps` what you pass and `json.loads` what you read. sqlc reads no parameter inside `> ANY (SELECT ...)` or `> ALL (SELECT ...)`, nor a `sqlc.slice` without a plain column on its left, and a `:copyfrom` binds none after `ON DUPLICATE KEY UPDATE`, where the drivers format none (write `VALUES(column)`); we refuse each.
- **SQLite** types are affinities, not constraints: another program may store a `str` in an `INTEGER` column. Use `STRICT` tables, and declare a `TEXT` primary key `NOT NULL`, as SQLite lets it be NULL. A boolean sqlc calls NOT NULL reads a NULL as `False`, as on MySQL. sqlc reads no parameter in a `HAVING`, an `ORDER BY` or an `ON CONFLICT ... DO UPDATE`, and misnumbers a `sqlc.slice` used twice; we refuse each. It writes a `*` out without quoting a column name that needs it (`order id`), so the statement fails at its first call: list the columns. A query file must be ASCII, comments included, as sqlc cuts every statement after a non-ASCII character short ([sqlc#4235](https://github.com/sqlc-dev/sqlc/issues/4235)); we refuse it.
- **SQLAlchemy on asyncpg** raises asyncpg's own error, not a `DBAPIError`, for what fails while a stream reads.
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

The code the plugin generates is yours, and carries none of these license terms.

`SPDX-License-Identifier: AGPL-3.0-or-later OR LicenseRef-KevinValk-Commercial`
