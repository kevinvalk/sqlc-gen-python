# SQL and driver limitations

## Expressions and parameters

If generation asks for a cast, put it on the expression whose type sqlc could not establish. For example, sqlc can assign `sum`, `nullif`, `coalesce` or `CASE` a type that disagrees with the database. Plain integer literals already have a known type. So does a `CASE` whose branches are all string literals or, on PostgreSQL, all `TRUE` or `FALSE` literals.

A cast preserves NULL. If an invoice report needs zero when there are no amounts to sum, write `coalesce(sum(amount), 0)::numeric`. If the caller can handle a missing value, keep it nullable. For example, a dashboard encountered a group whose `max(start_time)` was NULL. We mark results optional where the source is visible, including outer joins, scalar subqueries, empty aggregates and window offsets. [Dashboard migration](application-migration-audit.md#postgresql)

For a `UNION`, `INTERSECT` or `EXCEPT`, cast differing branch types to a common SQL type in each branch. A recursive CTE may need a cast on the outer result.

## PostgreSQL

- **Catalog nullability.** We captured identical requests from sqlc v1.31.1 for two columns where PostgreSQL allows NULL in only one. This happens with `CREATE TABLE AS`, an inheritance parent and a computed view column. Python can receive `None` in a required field. Use an explicitly constrained table or `coalesce` in the query when the caller needs a value. [PostgreSQL capture checks](testing.md#running-sqlc-itself)
- **Parameter types.** sqlc can give the parameter the other operand’s type in date arithmetic, JSON lookups, array indexing and row comparisons. Cast the parameter at its use, e.g. `issued_on + @days::int` or `settings ? @key::text`.
- **Parameter spelling.** Leave a space before `@id` in `id = @id`, since PostgreSQL can read `=@` as one operator. Parenthesize a cast before `AT TIME ZONE`, as in `(@day::date) AT TIME ZONE 'UTC'`. Avoid parameter names starting with `pg_catalog`, which can collide with a placeholder sqlc synthesizes.
- **JSON.** Pass the Python document. If you already have serialized JSON, decode it with `json.loads` first. The annotation excludes a bare `str` because the driver would encode the text as a JSON string. For a required scalar JSON argument, pass `None` explicitly to store JSON `null`. For a nullable argument or an element of a SQL array, `None` stores SQL NULL. Both read back as `None`. [Application examples](application-migration-audit.md#postgresql)
- **Arrays.** PostgreSQL accepts NULL elements in required array columns and allows values of different ranks in one column. The generated reader checks depth, but a JSON document or custom-loaded value that is itself a Python list can hide a rank mismatch. If the number of dimensions matters, enforce it in SQL, e.g. `CHECK (array_ndims(tags) = 1)`. [PostgreSQL array declarations](https://www.postgresql.org/docs/18/arrays.html)
- **Set-returning functions.** An aggregate inside another function, such as `unnest(array_agg(id))`, can yield zero rows. A `:one` caller must handle a missing row unless we know the enclosing calls are scalar.
- **Time.** Pass timezone-aware datetimes and convert to the intended zone before calling `.date()`. Drivers differ on naive inputs and on the zone used for `timestamptz` results. An interval of one month reads as 30 days. Infinite timestamps can raise a driver error. [Measured PostgreSQL driver behavior](typemap.md#postgresql-edges-that-raise-or-are-lossy)
- **Custom loaders.** Registering a psycopg loader for hstore or pgvector can change the Python type your queries receive. Match the override to the registered loader.

## MySQL and MariaDB

- **JSON, ENUM and SET.** JSON reads and writes as text. Use `json.dumps` when writing a document and `json.loads` when reading one. A multi-member `SET` reads as a comma-separated string in declaration order. sqlc describes a one-member `ENUM` and `SET` alike, so specify a `str` override for these.
- **Parameter positions.** sqlc can omit binds inside `> ANY (SELECT …)` and `> ALL (SELECT …)`, and for a `sqlc.slice` without a plain column on its left. In a `:copyfrom`, refer to the inserted value in `ON DUPLICATE KEY UPDATE`, e.g. `VALUES(column)` on server versions that support it. Generation rejects these unsupported binds.
- **Row counts.** `:execrows` counts changed rows by default. When the connection sets `CLIENT.FOUND_ROWS`, as SQLAlchemy does, it counts matched rows.
- **Cancellation.** Cancelling an asyncmy statement in flight can leave its connection unusable.
- **Stored values.** `FLOAT` can read back rounded to six significant digits, `TIMESTAMP` as a naive datetime in the session’s zone, and a zero date can read back as text. [Measured MySQL driver behavior](typemap.md#mysql-edges-that-raise-or-are-lossy)

## SQLite

- **Query files.** Use ASCII throughout, including comments. sqlc v1.31.1 counts characters and then slices by byte offset. After a non-ASCII character, even a plain `SELECT` can reach the plugin with its last characters missing. Generation fails when we detect the damage. [Upstream sqlc issue](https://github.com/sqlc-dev/sqlc/issues/4235) · [SQLite capture check](testing.md#running-sqlc-itself)
- **Storage types.** Use `STRICT` tables when another writer could store text in an integer column. In an ordinary table, declare a `TEXT PRIMARY KEY` explicitly `NOT NULL` when it must have a value. Types with NUMERIC affinity, such as `DECIMAL`, `DATETIME` and `TIMESTAMP`, need an override matching what the application stores, or a concrete storage type such as `TEXT`. [Measured SQLite driver behavior](typemap.md#sqlite-edges-that-raise-or-are-lossy) · [Type overrides and converters](reference.md#type-overrides-and-converters)
- **Parameters and identifiers.** sqlc can miss parameters in `HAVING`, `ORDER BY` and `ON CONFLICT … DO UPDATE`, and misnumber a repeated `sqlc.slice`. Generation rejects those cases. For a column name that requires quotes, such as `"order id"`, list it explicitly. A `*` expanded by sqlc can lose those quotes and fail at execution.

## Python type checking

Python’s type system accepts a `datetime` where a `date` is expected and a `bool` where an `int` is expected. Type checkers also accept some values that fail at runtime. Serializing a JSON document containing a `Decimal` or `UUID` raises an error. So does putting a frozen record in a set when one of its fields holds a list. [Cases exercised by the typing suite](../tests/python/suite/usage.py)
