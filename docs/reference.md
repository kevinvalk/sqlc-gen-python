# Generated Python reference

## Connections and drivers

Pass your connection first and the query arguments by keyword. If `max_params` groups them, pass the generated `<Query>Params` record. You own the transaction. When you rename a query file, remove its old Python module yourself. sqlc keeps existing files in the output directory.

| Backend | Database | Synchronous driver | Asynchronous driver |
| --- | --- | --- | --- |
| `sqlalchemy` | All three engines | psycopg, PyMySQL, sqlite3 | psycopg, asyncpg, asyncmy, aiosqlite |
| `psycopg` | PostgreSQL | psycopg 3 | psycopg 3 |
| `pymysql` | MySQL / MariaDB | PyMySQL | asyncmy ≥ 0.2.17 |
| `sqlite3` | SQLite | sqlite3 (standard library) | aiosqlite |

For asyncpg, pass a SQLAlchemy async connection. With psycopg you can pass the native connection directly.

We test on Python 3.11 and the newest supported Python with psycopg 3.3, SQLAlchemy 2.0 and 2.1, PyMySQL 1.2, asyncmy 0.2.17 or later, aiosqlite 0.22 and Pydantic 2.14. Psycopg pipelines need libpq 14 or later. Each release names its tested sqlc version. [Driver type map](typemap.md)

## Rows and parameters

One column usually gives you its value. A multi-column result uses a table model when it contains all columns of that table in order, with matching names, types and nullability. Otherwise the query module defines a `<Query>Row`. With `sqlc.embed(t)`, one field holds the model for `t`. Enums are `enum.StrEnum`. With `records: pydantic`, a field that would hide a `BaseModel` attribute gets a trailing underscore, e.g. `json_`.

| A `:one` query returns | Default | With `raise_on_none` |
| --- | --- | --- |
| No row | `None` | SQLAlchemy raises `NoResultFound`. Native backends raise `models.NoRowError`. |
| One row with a single NULL column | `None` | `None` |
| Two or more rows | SQLAlchemy raises `MultipleResultsFound`. Native backends raise `models.MultipleRowsError`. | Same error |

A trigger can skip an insert, or a SQLite constraint can ignore it, so even `INSERT … RETURNING` can yield no row. If a write returns a second row, the statement has already run when the function raises. Wrap it in a transaction you can roll back. Use a transaction for atomic batches too.

Use `sqlc.narg` for an optional filter or partial update. Nullable parameters default to `None`. All other parameters and all result fields are required. On PostgreSQL and SQLite, sqlc may need a cast at the parameter’s first use.

| Engine | Optional filter |
| --- | --- |
| PostgreSQL | `sqlc.narg(status)::invoice_status IS NULL OR status = sqlc.narg(status)` |
| SQLite | `CAST(sqlc.narg(name) AS TEXT) IS NULL OR name = sqlc.narg(name)` |
| MySQL | `sqlc.narg(name) IS NULL OR name = sqlc.narg(name)` |

On PostgreSQL, a cast such as `@paid_at::timestamptz` makes an ordinary parameter required even when its column is nullable. Give a `sqlc.slice` and a scalar distinct names when both filter the same column. The slice takes a list and stands alone in its parentheses, as `IN (sqlc.slice(ids))`. An async function spells a SQL parameter named `timeout` as `timeout_`.

## Streaming

A read-only `:many` also generates `stream_<query>`. The context manager reads one row at a time and closes the cursor on exit. An async backend uses `async with` and `async for`.

```python
with invoices.stream_list_invoices_by_status(connection, statuses=[InvoiceStatus.PAID]) as rows:
    for row in rows:
        export(row)
```

PostgreSQL streams require a transaction. On psycopg, reopening the same stream inside itself on one connection raises `DuplicateCursor` and aborts the transaction. Run psycopg streams and COPY outside a caller’s pipeline. When you read a SQLAlchemy stream backed by asyncpg, asyncpg can raise its own exception.

On MySQL, running another statement on a connection with an open stream can end that stream early with a `UserWarning`. Finish consuming it first, or use a second connection.

## Options

The plugin rejects unknown options and unmatched renames or overrides. Qualify table and enum names outside the default schema. Entries can share options with a YAML anchor. A models-only entry reads no queries, so its renames must match the schema. [All options and defaults](../README.md#options)

## Type overrides and converters

Use `db_type` to override a database type, or `column` for one column and the parameters sqlc ties to it. A column override takes precedence. With only `python_type`, the annotation describes what the driver already returns. For example, pgvector’s default text representation can be declared as `{db_type: vector, python_type: str}`. A dotted type name is imported. If Ruff treats your application’s override module as first-party, list it under `known-third-party` to match the generated imports.

For a SQLite `TEXT` column that stores JSON, give the override both conversion functions.

```yaml
overrides:
  - column: accounts.settings
    python_type: object
    to_db: json.dumps
    from_db: json.loads
```

`to_db` receives the application value. The backend then adapts the value for SQL, including JSON encoding on PostgreSQL. `from_db` receives the backend’s verified Python type, or `object` for an unmapped scalar, so a type checker can check the decoder. NULL passes through as `None`. For arrays we convert each non-NULL element at the declared depth.

PostgreSQL overrides accept database names and unambiguous standard spellings, such as `int8` or `bigint` for a `bigserial` column. Use explicit PostgreSQL names in place of the ambiguous spellings `decimal`, `char`, `float` and `bit varying`. MySQL and SQLite use the type name sqlc reports. A parameter with its own cast, such as `@total::numeric`, uses the type override. A parameter without an inferred type needs a SQL cast. sqlc supplies the integer type for `LIMIT` and `OFFSET`.

## Bulk inserts

`:copyfrom` takes rows of `<Query>Params` for a plain `INSERT INTO t (a, b) VALUES (@a, @b)` and returns the number actually written, including when a trigger skips rows. Raw psycopg streams its input into `COPY FROM STDIN` atomically. COPY fires no rules and cannot target a view. It also stores a value for a `GENERATED ALWAYS` identity column, which the `INSERT` would refuse. An insert with `OVERRIDING USER VALUE` uses ordinary INSERT semantics so the database can generate the identity values.

Other backends use a bulk call when the driver reports a row count. SQLAlchemy with asyncpg counts each insert separately. Batch large inputs and wrap them in a transaction. We timed 10,000 four-column rows in one transaction on Python 3.11 with local Docker databases in November 2026. [How the count was verified](migration-audit.md#what-counted-bulk-inserts-require)

| Engine | Backend and driver | Insert method | `:copyfrom` | One `:exec` per row |
| --- | --- | --- | --- | --- |
| PostgreSQL | `psycopg` | `COPY ... FROM STDIN` | 9 ms | 3.5 s |
| PostgreSQL | `psycopg` async | `COPY ... FROM STDIN` | 9 ms | 3.7 s |
| PostgreSQL | `sqlalchemy` on psycopg | `executemany` | 130 ms | 3.8 s |
| PostgreSQL | `sqlalchemy` async, on asyncpg | one counted `INSERT` per row | 8.5 s | 7.4 s |
| PostgreSQL | `sqlalchemy` async, on psycopg | `executemany` | 610 ms | 6.2 s |
| MySQL | `pymysql` | `INSERT`s of many rows | 120 ms | 4.8 s |
| MySQL | `pymysql` async, on asyncmy | `INSERT`s of many rows | 110 ms | 3.9 s |
| MySQL | `sqlalchemy` on PyMySQL | `INSERT`s of many rows | 120 ms | 4.0 s |
| MySQL | `sqlalchemy` async, on asyncmy | `INSERT`s of many rows | 100 ms | 4.4 s |
| SQLite | `sqlite3` | `executemany` | 6 ms | 8 ms |
| SQLite | `sqlite3` async, on aiosqlite | `executemany` | 5 ms | 290 ms |
| SQLite | `sqlalchemy` | `executemany` | 19 ms | 79 ms |
| SQLite | `sqlalchemy` async, on aiosqlite | `executemany` | 19 ms | 1.1 s |
