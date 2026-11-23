# Running Python against other projects’ sqlc queries

On 23 November 2026 we checked 43 sqlc entries from 32 public projects outside the Python migration inventory. We concentrated on MySQL because neither of the two earlier MySQL projects supplied Python database tests. This cohort includes projects that ship Go and other clients. It measures what happens when their SQL is compiled to Python, populated with data and called through real drivers. The earlier audit measures the effort of migrating an existing Python application. [Python migration results](application-migration-audit.md)

## Generation

We used GitHub code search for MySQL and SQLite configurations and selected distinct repositories with application-specific SQL. We included every entry in the selected configurations, including unsuccessful ones. This is a targeted sample. Some repositories were already present in the older cross-language generation corpus. Each row of the ledger identifies its exact Git revision and configuration entry. [Generation ledger and diagnostics](audits/external-sqlc-projects.tsv)

| Engine | Repositories¹ | Entries | Original SQL generated Python on four profiles | Python generator reported a diagnostic² | sqlc could not compile the inputs³ |
| --- | ---: | ---: | ---: | ---: | ---: |
| MySQL | 24 | 28 | 15 | 11 | 2 |
| SQLite | 9 | 10 | 4 | 3 | 3 |
| PostgreSQL | 5 | 5 | 2 | 2 | 1 |
| **Total** | **32** | **43** | **21** | **16** | **6** |

¹ A repository can use more than one engine. MySQL and MariaDB consume the same generated packages.

² We also ran sqlc v1.31.1’s built-in Go generator against the same SQL. It generated all 16 entries in this column. The Python diagnostics identified such issues as untyped concatenation parameters, arithmetic or aggregates whose result type sqlc could not establish, and computed columns in derived tables. We tested one repair. Casting the derived `ROW_NUMBER()` to `UNSIGNED` in the task-system leaderboard let the whole entry generate. On both servers, two creators with different task counts then ranked first and second across all four profiles. The other 15 entries retain their diagnostics. [Pinned leaderboard query](https://github.com/mizmorr/task_system/tree/0f831c2d6156ba7ea81d97c43dbb8b021888c36e/internal/app/adapters/secondary/mysql/sql)

³ Both generators stopped on the same inputs. Filex contributed three entries, Fastship had no active query declaration, Kent renders a temporary query file in its build, and sqlc rejected NanoDEP’s MySQL upsert syntax. The published NanoDEP Go client still passed its upstream MySQL storage tests at this revision. [NanoDEP MySQL storage tests](https://github.com/micromdm/nanodep/tree/a40e6977cbe11973536c21a02671215694e8f6ab/storage/mysql)

## Database results

We exercised **all 21 entries that generated from their original SQL**, plus the leaderboard entry after the cast. These 22 entries come from 20 projects. A profile is one backend in one execution mode. Every entry ran on SQLAlchemy and the native backend, both sync and async. The 88 generated packages also passed Ruff formatting and lint, strict pyright and mypy, and import checks under Python 3.11 and 3.15. [Static gate](testing.md)

| Database tested | Projects | Entries exercised | Profiles passed | Generated function calls |
| --- | ---: | ---: | ---: | ---: |
| MySQL 8.4.13 | 14 | 16 | 64 / 64 | 432 |
| MariaDB 11.4.13 | 14 | 16 | 64 / 64 | 432 |
| SQLite 3.53.1 | 4 | 4 | 16 / 16 | 104 |
| PostgreSQL 18.6 | 2 | 2 | 8 / 8 | 68 |
| **Total** | **20 distinct** | **22 distinct** | **152 / 152** | **1,036** |

Across the 22 entries we called **101 of 334 named queries** and 22 of their streaming variants, counting each query once per entry across databases and backends. That covers 73 of 215 named MySQL queries, 17 of 98 SQLite queries and 11 of 21 PostgreSQL queries. The static checks cover every generated module, including the uncalled functions.

Each profile starts with a fresh database. The scenarios call generated Python, check the returned values and annotations, and query the stored state on a separate connection. For each exercised read with a streaming variant we also verified that the stream returned the same items. The call count includes repeated calls and streams. In the run ledger, result items count materialized records and scalars, including generated IDs, once per ordinary call. Direct statements include setup and independent comparisons. These are audit-authored scenarios. Passing covers the named functions and arguments in the ledger. [Per-profile calls and functions](audits/external-sqlc-runs.tsv) · [Executable assertions](../tests/python/suite/downstream/scenarios.py)

- **Transaction visibility.** The go-kratos example inserted a user and details inside one transaction. A second connection could see neither the uncommitted user nor the rolled-back details. A committed pair could be read back with a join. [Original transaction example](https://github.com/go-kratos/examples/tree/61daed1ec4d5a94d689bc8fab9bc960c6af73ead/transaction/sqlc)
- **Values that can look right with the wrong type.** The golf catalog round-tripped a six-place decimal latitude, a NULL longitude and a boolean. The household ledger checked a leap-day date and a multi-value tag filter. The SQLite broadcast cache updated and read back BLOBs containing zero and non-UTF-8 bytes. A PostgreSQL scheduler checked nested JSON and the transition from an active to a paused task. [Golf catalog](https://github.com/jamiewhitney/fairways-core/tree/3a572683017ffe02f8815e02aea03337318b2c99/migrations/catalog) · [Household ledger](https://github.com/peng225/kakeibodb/tree/cbb1dca5814b2d2e21f8b736f4b8df03ea37ac39/internal/repository/mysql/sqlc)
- **Server-dependent results.** A golf booking at 13:00 was queried with `DATE(datetime) = ?`. Binding the same afternoon datetime gave no matching row on MySQL and a match on MariaDB. Direct SQL on a separate connection reproduced each result. Binding midnight matched the booking on both servers. [Booking query](https://github.com/jamiewhitney/fairways-core/blob/3a572683017ffe02f8815e02aea03337318b2c99/migrations/bookings/query.sql)

We verified two failure cases in disposable copies of the generated packages. Truncating one byte from each returned SQLite BLOB failed the byte-for-byte comparison while preserving the Python type. Inserting a premature commit into the generated MySQL insert failed the independent connection’s visibility check.

### Original application tests

For two projects we additionally compiled the upstream Go tests unchanged and set their database DSNs. A missing DSN skips these tests upstream, so we inspected the actual test outcomes. NanoDEP’s MySQL configuration asks for the driver’s default timestamp handling. Its storage code parses timestamp strings itself. [NanoDEP driver configuration](https://github.com/micromdm/nanodep/blob/a40e6977cbe11973536c21a02671215694e8f6ab/storage/mysql/sqlc.yaml)

| Original Go storage suite | MySQL | MariaDB | PostgreSQL |
| --- | --- | --- | --- |
| NanoCMD workflow engine | Passed | Failed on MySQL-style upserts | — |
| NanoCMD profile storage | Passed | Failed on MySQL-style upserts | — |
| NanoDEP storage | Passed | Failed on MySQL-style upserts | Passed |

MariaDB rejected the applications’ `VALUES (…) AS alias ON DUPLICATE KEY UPDATE` syntax. The NanoCMD Python profile scenario seeds its own rows and exercises lookups, slice filters and deletion. Its passing MariaDB run and the full Go suite’s failure describe different operations. [NanoCMD engine tests](https://github.com/micromdm/nanocmd/tree/c571ad2907d68b9f01175d7f7105e3d366b1e0cc/engine/storage/mysql) · [NanoCMD profile tests](https://github.com/micromdm/nanocmd/tree/c571ad2907d68b9f01175d7f7105e3d366b1e0cc/subsystem/profile/storage/mysql)

## Replay

Build the current WASM from the repository root. The command below uses the captured sqlc binary when available, or `sqlc` from `PATH`. The audited compiler was v1.31.1. The runner fetches pinned Git objects into `target/downstream/`. SQLite needs no database service. The other servers use disposable Docker databases. [Replay runner](../tests/python/suite/downstream/replay.py)

```sh
cargo wasm
cd tests/python
uv run --locked python -m suite.downstream.replay --server sqlite --project censys/cencli
uv run --locked python -m suite.downstream.replay --server mysql
uv run --locked python -m suite.downstream.replay --server mariadb
uv run --locked python -m suite.downstream.replay --server postgresql
```

Omit `--project` to run the full cohort for that server. Use `--sources DIR` for existing checkouts under `DIR/OWNER/REPOSITORY`, or supply another compiler or plugin with `--sqlc PATH` and `--wasm PATH`. The runner reads SQL from Git objects at the ledger revision. It applies forward migrations in isolated databases, omits database-selection statements from deployment scripts, and skips the iris-blog dump’s restore of session settings that it never saved. Only the leaderboard entry changes a query.
