# Running Python against other projects’ sqlc queries

On 23 November 2026 we tried 43 sqlc entries from 32 public projects outside the Python migration audit, including projects with Go clients. We focused on MySQL because neither MySQL project in the earlier audit shipped Python database tests. Here we generated Python from each project’s SQL and wrote scenarios that read and wrote real rows. [Python migration results](application-migration-audit.md)

## Generation

We searched GitHub for MySQL and SQLite configurations containing application queries. We kept every entry from the configurations we selected, including ones that failed. Some repositories also appeared in the earlier cross-language audit. The ledger records the configuration and Git revision for each entry. [Generation ledger and diagnostics](audits/external-sqlc-projects.tsv)

| Engine | Repositories¹ | Entries | Generated on four profiles | Stopped in the Python plugin² | sqlc could not compile the entry³ |
| --- | ---: | ---: | ---: | ---: | ---: |
| MySQL | 24 | 28 | 15 | 11 | 2 |
| SQLite | 9 | 10 | 4 | 3 | 3 |
| PostgreSQL | 5 | 5 | 2 | 2 | 1 |
| **Total** | **32** | **43** | **21** | **16** | **6** |

¹ A repository can use more than one engine. MySQL and MariaDB consume the same generated packages.

² sqlc v1.31.1 generated Go for all 16 entries. The Python plugin reported ambiguous types in concatenation, arithmetic, aggregates and derived columns. We tried one fix. Casting the leaderboard’s `ROW_NUMBER()` to `UNSIGNED` let the whole entry generate. Across all four profiles on both servers, two creators with different task counts ranked first and second as expected. We recorded the errors for the other 15 entries. [Pinned leaderboard query](https://github.com/mizmorr/task_system/tree/0f831c2d6156ba7ea81d97c43dbb8b021888c36e/internal/app/adapters/secondary/mysql/sql)

³ Both generators stopped on the same inputs. Filex contributed three entries, Fastship had no active query declaration, Kent renders a temporary query file in its build, and sqlc rejected NanoDEP’s MySQL upsert syntax. The published NanoDEP Go client still passed its upstream MySQL storage tests at this revision. [NanoDEP MySQL storage tests](https://github.com/micromdm/nanodep/tree/a40e6977cbe11973536c21a02671215694e8f6ab/storage/mysql)

## Database results

We ran all 21 entries that generated with their original SQL, plus the leaderboard after the cast. The 22 entries came from 20 projects. For each we generated four packages using SQLAlchemy and the native backend, each sync and async. All 88 packages passed Ruff, strict pyright, strict mypy and import checks on Python 3.11 and 3.15. [Static gate](testing.md)

| Database tested | Projects | Entries exercised | Profiles passed | Generated function calls |
| --- | ---: | ---: | ---: | ---: |
| MySQL 8.4.13 | 14 | 16 | 64 / 64 | 432 |
| MariaDB 11.4.13 | 14 | 16 | 64 / 64 | 432 |
| SQLite 3.53.1 | 4 | 4 | 16 / 16 | 104 |
| PostgreSQL 18.6 | 2 | 2 | 8 / 8 | 68 |
| **Total** | **20 distinct** | **22 distinct** | **152 / 152** | **1,036** |

We called **101 of 334 named queries** and 22 streaming variants. Counting each query once per entry gives 73 of 215 MySQL queries, 17 of 98 SQLite queries and 11 of 21 PostgreSQL queries. Static checks also covered the uncalled functions.

Each profile started with a fresh database. We called generated functions, checked their results against the annotations and read the stored values from a second connection. For reads with a stream we compared the list with the streamed rows. In the ledger, calls include repeats and streams. Result items count rows and scalars (including IDs) once per ordinary call. Direct statements cover setup and independent comparisons. [Per-profile calls and functions](audits/external-sqlc-runs.tsv) · [Executable assertions](../tests/python/suite/downstream/scenarios.py)

- **Transaction visibility.** The go-kratos example inserted a user and details inside one transaction. A second connection could see neither the uncommitted user nor the rolled-back details. A committed pair could be read back with a join. [Original transaction example](https://github.com/go-kratos/examples/tree/61daed1ec4d5a94d689bc8fab9bc960c6af73ead/transaction/sqlc)
- **Values that can look right with the wrong type.** The golf catalog round-tripped a six-place decimal latitude, a NULL longitude and a boolean. The household ledger checked a leap-day date and a multi-value tag filter. The SQLite broadcast cache updated and read back BLOBs containing zero and non-UTF-8 bytes. A PostgreSQL scheduler checked nested JSON and the transition from an active to a paused task. [Golf catalog](https://github.com/jamiewhitney/fairways-core/tree/3a572683017ffe02f8815e02aea03337318b2c99/migrations/catalog) · [Household ledger](https://github.com/peng225/kakeibodb/tree/cbb1dca5814b2d2e21f8b736f4b8df03ea37ac39/internal/repository/mysql/sqlc)
- **Server-dependent results.** A golf booking at 13:00 was queried with `DATE(datetime) = ?`. Binding the same afternoon datetime gave no matching row on MySQL and a match on MariaDB. Direct SQL on a separate connection reproduced each result. Binding midnight matched the booking on both servers. [Booking query](https://github.com/jamiewhitney/fairways-core/blob/3a572683017ffe02f8815e02aea03337318b2c99/migrations/bookings/query.sql)

We verified two failure cases in disposable copies of the generated packages. Truncating one byte from each returned SQLite BLOB failed the byte-for-byte comparison while preserving the Python type. Inserting a premature commit into the generated MySQL insert failed the independent connection’s visibility check.

### Original application tests

We ran two projects’ own Go storage tests unchanged. Both suites skip tests when their database URL is missing, so we set the URLs and checked that the named tests ran. NanoDEP uses the MySQL driver’s default timestamp handling and parses the resulting strings itself. [NanoDEP driver configuration](https://github.com/micromdm/nanodep/blob/a40e6977cbe11973536c21a02671215694e8f6ab/storage/mysql/sqlc.yaml)

| Original Go storage suite | MySQL | MariaDB | PostgreSQL |
| --- | --- | --- | --- |
| NanoCMD workflow engine | Passed | Failed on MySQL-style upserts | — |
| NanoCMD profile storage | Passed | Failed on MySQL-style upserts | — |
| NanoDEP storage | Passed | Failed on MySQL-style upserts | Passed |

MariaDB rejected the applications’ `VALUES (…) AS alias ON DUPLICATE KEY UPDATE` syntax. The NanoCMD Python profile scenario seeds its own rows and exercises lookups, slice filters and deletion. Its passing MariaDB run and the full Go suite’s failure describe different operations. [NanoCMD engine tests](https://github.com/micromdm/nanocmd/tree/c571ad2907d68b9f01175d7f7105e3d366b1e0cc/engine/storage/mysql) · [NanoCMD profile tests](https://github.com/micromdm/nanocmd/tree/c571ad2907d68b9f01175d7f7105e3d366b1e0cc/subsystem/profile/storage/mysql)

## Replay

From the repository root, build the WASM and choose a server. The runner uses the captured sqlc binary if present, otherwise `sqlc` from `PATH`. We used v1.31.1. It fetches the pinned Git objects into `target/downstream/`. SQLite runs locally. The other engines use disposable Docker databases. [Replay runner](../tests/python/suite/downstream/replay.py)

```sh
cargo wasm
cd tests/python
uv run --locked python -m suite.downstream.replay --server sqlite --project censys/cencli
uv run --locked python -m suite.downstream.replay --server mysql
uv run --locked python -m suite.downstream.replay --server mariadb
uv run --locked python -m suite.downstream.replay --server postgresql
```

Omit `--project` to run all projects for that server. Use `--sources DIR` for existing checkouts under `DIR/OWNER/REPOSITORY`, or supply another compiler or plugin with `--sqlc PATH` and `--wasm PATH`. The runner reads SQL from Git objects at the ledger revision. It applies forward migrations in isolated databases, omits database-selection statements from deployment scripts, and skips the iris-blog dump’s restore of session settings that it never saved. Only the leaderboard entry changes a query.
