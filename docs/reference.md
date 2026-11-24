# Reference

What the README leaves out. When generation fails, the message says what to change, so [the last section](#what-generation-cannot-see) only lists what generation cannot see.

## Connections and drivers

| Backend | Database | Synchronous driver | Asynchronous driver |
| --- | --- | --- | --- |
| `sqlalchemy` | all three | psycopg, PyMySQL, sqlite3 | psycopg, asyncpg, asyncmy, aiosqlite |
| `psycopg` | PostgreSQL | psycopg 3 | psycopg 3 |
| `pymysql` | MySQL, MariaDB | PyMySQL | asyncmy |
| `sqlite3` | SQLite | sqlite3 | aiosqlite |

asyncpg has no backend of its own: pass a SQLAlchemy async connection.

psycopg reads an array of a type outside PostgreSQL's own, an extension's such as `citext` or your own enum, as its text until the type is registered on the connection. So a function that reads one registers its types first, once per connection, with one query to the catalog per type, as [psycopg recommends](https://www.psycopg.org/psycopg3/docs/basic/pgtypes.html). On SQLAlchemy it does so on the psycopg connection underneath, so such a module imports psycopg. asyncpg returns a broken item for an empty string in an array of `citext`, `ltree` or `lquery`, so async SQLAlchemy refuses those arrays: use the `psycopg` backend, or cast the array to `text[]`.

## Rows and parameters

A `:one` that finds no row returns `None`, and one that finds a second row raises. With `raise_on_none` the missing row raises too. On SQLAlchemy those are `NoResultFound` and `MultipleResultsFound`, on the other backends `models.NoRowError` and `models.MultipleRowsError`. Without `raise_on_none`, a `:one` of a single nullable column returns `None` for no row and for a NULL alike.

An `INSERT ... RETURNING` can return no row either, as a trigger may skip the insert. And when a write returns a second row, it has already run by the time the function raises, so call it in a transaction you can roll back.

## Streaming

A `:many` that only reads also has a `stream_<query>`, which reads a row at a time and closes its cursor when the block ends.

```python
with invoices.stream_list_invoices_by_status(connection, statuses=[InvoiceStatus.PAID]) as rows:
    for row in rows:
        export(row)
```

On MySQL, finish a stream before you run anything else on its connection, or use a second connection. The driver ends the stream early with nothing but a `UserWarning`.

## Type overrides and converters

An override with only a `python_type` changes the annotation and converts nothing: the value is still what the driver returns. To convert, name two functions as well. `to_db` takes your value and returns what the driver binds, and `from_db` takes what the driver returns, as [the type map](typemap.md) lists it, and returns your value. Neither is called for NULL, and for an array each is called per element. [The ledger example](../examples/ledger/sqlc.yaml) keeps a JSON document in a SQLite `TEXT` column this way.

If you lint the generated files and Ruff sorts your override's module as first-party, list it under `known-third-party`, as that is where the generated imports put it.

## Bulk inserts

`:copyfrom` takes an iterable of `<Query>Params` and returns how many rows the database wrote. On the `psycopg` backend it is PostgreSQL's `COPY` and not the `INSERT` you wrote: rules do not fire, and a `GENERATED ALWAYS` identity column stores the value it is given. Every other backend runs the `INSERT`, in bulk where the driver counts the rows. SQLAlchemy on asyncpg does not count them, so there it is one `INSERT` per row.

For 10,000 rows into a local PostgreSQL, `COPY` took 60 ms and SQLAlchemy on psycopg 140 ms, where one `INSERT` per row took 3.4 s.

## What generation cannot see

These generate without an error and can still surprise you. [The type map](typemap.md) adds, per driver, the values that raise or change on their way back.

On PostgreSQL:

- A view's column from the outer side of a join is reported as required, like the computed one the README names.
- In a row compared with `IN`, as `(a, b) IN ((@a, @b))`, sqlc types every parameter by the first column. We refuse that for `=` and `<`, but do not read it here, so cast each parameter.
- A JSON parameter takes the document and never its `json.dumps` text. `None` stores JSON `null` in a required parameter and SQL NULL in a nullable one or in an array. Both read back as `None`.
- On psycopg, cast an array parameter that you compare, as `tag_ids @> @ids::int[]`. psycopg sends a list of small integers as `smallint[]`, and PostgreSQL has no operator between arrays of two types. An insert or update needs no cast.

On MySQL and MariaDB:

- sqlc types a view's expression column by a guess, and nothing tells us which table is a view. So `price * quantity` reads as `int` where MySQL returns a `Decimal`, and can be NULL in a required field. Cast such a column inside the view.
- sqlc does not report a table's `DEFAULT CHARSET=binary`, whose text columns the drivers return as `bytes`.
- `:execrows` counts changed rows on `pymysql` and matched rows on `sqlalchemy`, which connects with `CLIENT.FOUND_ROWS`. A `:copyfrom` with `ON DUPLICATE KEY UPDATE` counts 2 for each row it updates, as MySQL does.

On SQLite:

- A table that is not `STRICT` can hold any type in any column, and a column-level `PRIMARY KEY` other than `INTEGER PRIMARY KEY` can hold NULL though sqlc reports it as required. Use `STRICT` tables and say `NOT NULL`.
- A view's expression column is typed by a guess, as on MySQL.

## Coming from another generator

sqlc's own Python generator puts the queries on a `Querier` that holds the connection. Here each query is a function that takes the connection first and its arguments by keyword, and a `:many` returns a list, with `stream_<query>` for a row at a time. To keep asyncpg, pass a SQLAlchemy async connection. [The projects we migrated](migrated-projects.tsv) show what each one needed.
