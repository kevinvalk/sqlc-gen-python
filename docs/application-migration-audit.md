# Python application migration audit

On 22 November 2026, 25 of 26 projects with passing query tests on the original generator passed those same tests after migration. The successful ports accounted for 444 passing tests on each generator. We counted the MCP-Universe user API once. Nine other repositories ship the same API, and each passed its two tests on both generators. One project that used only generated models passed 11 tests without caller changes. [Generation by engine](#generation-by-engine)

## What we counted

| Unit | Result | How to read it |
| --- | ---: | --- |
| Public repository candidates | 147 | 136 previously inspected repositories and 11 new search hits. |
| Excluded candidates | 62 | Seven generator fixtures or copies of upstream generator tests, 44 previous hits without an eligible configuration, and 11 new documentation, tool catalog or fixture hits. |
| Downstream repositories | 85 | Applications, libraries, examples and templates. One has entries for all three engines. |
| Configured entries | 104 | 100 generated on all four applicable profiles, across 81 projects. Four entries were missing their queries or schema. |
| Projects with Python test files | 53 | 32 of 85 had none. Each file named `test_*.py`, `*_test.py`, `test.py` or `tests.py` was a candidate for inspection. |
| Projects whose tests we ran | 37 | For the other 16, inputs were missing or we could not establish that their tests called generated queries. |
| Passing original baselines | 36 | 35 query workloads and one models-only workload. One original smoke test failed during import. |
| Query-backed ports | 34 / 35 | 25 of 26 with the MCP-Universe workload counted once, plus 9 of 9 additional copies. The remaining port failed during collection on its original Python 3.9 stack. |

Look up the revision, configuration and reason we included or excluded a project in the project ledger. The run ledger records the pytest selection, working directory, Python version, passes, failures, skips and observed generated calls. A blank test count means the run produced no JUnit report. [Project ledger](audits/python-application-tests.tsv) · [Run ledger](audits/python-application-runs.tsv) · [Generation audit](migration-audit.md)

### Discovery

We ran 17 more GitHub code searches for plugin names, configuration in YAML, JSON and Nix, README mentions, sync/async options and generated Python headers. We inspected the 11 new repositories among the 114 hits. One match was a Go file header quoted in a Python test. Atlantic, an earlier sample, had commented out both its Python output configuration and its dashboard calls. Its seven queries generated, so we recorded them separately from the active projects and upstream tests. [Original 136 pins](audits/python-sqlc-projects.tsv) · [Eleven additional pins and decisions](audits/python-search-additions.tsv)

We searched public default branches. GitHub limits results to 1,000 per query, so we split the broad Python-header search by file size. The three searches returned 682, 310 and 94 hits. Unindexed files, private repositories, other branches and projects without a searchable reference to sqlc can be missing. [GitHub search API](https://docs.github.com/en/rest/search/search#search-code)

### Comparing the two generators

For the original run we used the checked-in Python package, or the pinned generator for the CRUD smoke script. For the port we used sqlc v1.31.1 and WASM SHA-256 `6b800798b05cd1b276b454c27ac639e9328da4d34497a71e54bc03da67cd57a1`. Both runs started with a fresh database, the same schema and identical fixture repairs. We counted a port as passing when the same named tests passed with the same skips. Most projects supplied a focused integration suite. The run ledger names the tests.

To count a generated call, we traced the Python stack when a test reached the driver or SQLAlchemy’s execute method. For the two b-stead HTTP services we traced the server process too. One CRUD script called SQLAlchemy directly, so we checked which generated SQL constants it used. After migration we checked module headers to make sure the tests reached the new generator’s output. The test count includes routing, validation and mocks. The function count tells you which generated queries reached the database.

We classified each port by the largest change its tested callers needed.

| Migration effort | What we changed |
| --- | --- |
| Drop-in | Chose the plugin, backend and mode. The original callers and SQL worked. |
| Mechanical | Updated imports, connection arguments, record access, keyword names or list iteration. |
| Targeted | Changed SQL, recovered a schema or adjusted how the caller handles NULL, JSON or enums. |
| Substantial | Changed who owns the connection and transaction, or adapted a third-party database interface. |

## Generation by engine

| Engine | Projects | With Python test files | Entries generated | Used original SQL | Edited or recovered SQL | Missing inputs |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| PostgreSQL | 78 | 51 | 92 / 95 | 67 | 25 | 3 |
| SQLite | 7 | 2 | 6 / 7 | 3 | 3 | 1 |
| MySQL / MariaDB | 2 | 0 | 2 / 2 | 2 | 0 | 0 |

All four applicable profiles generated for each successful entry. Of the 72 entries that kept their SQL, three needed additional configuration or a converter. One repository uses all three engines.

## PostgreSQL

| Outcome, with the MCP-Universe user API counted once | Projects | Original tests passed | Port tests passed |
| --- | ---: | ---: | ---: |
| Drop-in query API | 0 | — | — |
| Mechanical migration | 9 | 200 | 200 |
| Targeted migration | 12 | 196 | 196 |
| Substantial migration | 2 | 3 | 3 |
| Original platform incompatible | 1 | 15 | Collection failed |
| Models-only drop-in | 1 | 11 | 11 |
| Original baseline unavailable | 1 | Collection failed | — |
| Additional copies of a passing query workload | 9 | 18 | 18 |

The 23 successful PostgreSQL query ports called **232 generated functions**, counting each once per project and module. Smartnews had 99 passing tests that called 11 functions. Smart Credit Office had four tests that called 48. Each of the nine additional MCP-Universe copies regenerated its package and exercised `create_user` and `get_user_by_email`. We also ran both photo-approval applications because they share a test file but use different schemas and queries. [Run ledger](audits/python-application-runs.tsv)

| What changed or failed | Evidence from the port |
| --- | --- |
| JSON input | NinjaMagic passed pre-encoded game state to a JSON argument. Passing the Pydantic model's JSON-compatible dictionary fixed the inventory round trip. Smart Credit Office supplied documents in place of its driver's wrapper. The generated function owns serialization in both cases. [NinjaMagic inventory](https://github.com/dzaramelcone/ninjamagic-py/blob/d7fadb33fad43586df78c310ad194d6ba5f9172d/ninjamagic/inventory.py) |
| Nullability and enums | The brain dashboard needed to handle a group with no non-null start time. An HTTP service needed a nullable named status bind. The blood-bank app needed a JSON cast and distinct Python names for positive and negative blood groups. Its full backend suite passed all 53 tests. [Brain Dashboard queries](https://github.com/rafi123123/TAU-brain-dashboard/blob/47fa0b6df307290be19a7cbe0284482f7ffce6a4/api/db/query.sql) · [Blood-bank project](https://github.com/japetsch/dat251-group-c/tree/ea2e0791b4f762ded71fcbf9ce671b287e2ad67e) |
| Native asyncpg | Nixbot's original pool rejected a SQLAlchemy statement. The migrated callers use SQLAlchemy's async engine on asyncpg, with an explicit transaction and Python JSON values. The same six generated operations ran before and after the port. The two tests still check cancellation, status and timestamps using direct SQL. [Nixbot persistence tests](https://github.com/Mic92/nixbot/blob/171a8c77d6e8e2b5b99ce5870c042a2dbc2cda62/nixbot/nixbot/tests/test_db.py) |
| SQLAlchemy emulation | A library implementing its own SQLAlchemy interface assumed binds named `p1`, `p2`. Keeping bind order and implementing result iteration and `one_or_none()` got its existing insert, lookup and list assertions passing. Its one upstream test covers those methods. [Shim test](https://github.com/tandemdude/sqlc-asyncpg-compat/blob/1663d6cf9211bcd50a0a8c8a6055d8806e662eca/tests/test_function.py) |
| Unsupported Python | The template passed 15 admin tests on Python 3.9. The regenerated models failed to import `enum.StrEnum`. Its requirements also pin SQLAlchemy 1.4.32 and Pydantic 1.9.0. A platform upgrade is outside the measured port. [Pinned requirements](https://github.com/danicc097/FastAPI-React-Postgres-Docker-Traefik-template/blob/2f63b72f35ae42ea57ab9471fd955e5fc22ce56f/backend/requirements.txt) |
| Broken original | Hyrex's own smoke test imports a dispatcher module absent from that revision. Replacing the worker scenario would change what was being tested. [Original smoke test](https://github.com/hyrex-labs/hyrex-python/blob/a082030a835327268aa2d088de9edfdb671b084e/tests/smoke_test.py) |

We supplied missing migrations or sequences, pointed services at disposable databases and fixed async fixture lifetimes before running either generator. We deselected one RSS test on both sides because it used live DNS to check email deliverability. Tarun’s CRUD smoke script caught exceptions and printed them. In both runs we let those exceptions propagate and asserted creation, retrieval, update, list membership and soft deletion. The OSINT suite ran handwritten SQL in both phases. [Original CRUD smoke script](https://github.com/TarunrRajanala/test-repo/blob/a4c0ca439db909a72eb2d00e057c206fc1729491/test_sqlalchemy_write.py) · [OSINT project](https://github.com/geldois/osint-engine/tree/be31b3e2e1794fb35793e8c70134e632ce2f34c3)

These ports needed changes to JSON serialization, connection ownership or Python versions. The dashboard case exercised the existing NULL handling. We found no new typing rule to add to `sqlc_typing`. [Generator type guarantees and limitations](../README.md#known-limitations)

## SQLite

| Project | Original → port | Tests with an observed generated call in the port | Distinct port functions |
| --- | ---: | ---: | ---: |
| Clanker, feedback store | 24 → 24 | 18 | 4 |
| X-Ray Proxy, full suite | 21 → 21 | 3 | 2 |

Clanker checks filters, duplicate handling and persistence across reopening the database. X-Ray Proxy writes ship graphics through generated Python, reads them back and compares the application's response dictionaries. Both entries needed SQL and caller changes. Five of seven SQLite projects lacked Python test files, and one of those five also lacked a required SQL input. Six entries separately passed our populated scenarios on four profiles each. [Clanker feedback tests](https://github.com/alex-furrier/clanker/blob/626926ea1b7b3369c3ff64d3b2d50b8dbcb0d1e4/tests/test_sql_feedback.py) · [X-Ray Proxy tests](https://github.com/rsky/x-ray-proxy/tree/ec3a1b2309626289102c3742e681ccaf235641ba/tests) · [Scenario results](migration-audit.md#verification-and-replay)

## MySQL and MariaDB

| Server | Projects | Entries using original SQL | Upstream suites available | Project-specific scenarios passed |
| --- | ---: | ---: | ---: | ---: |
| MySQL | 2 | 2 / 2 | 0 | 8 / 8 |
| MariaDB | 2 | 2 / 2 | 0 | 8 / 8 |

We ran `rizkia-as-actmp/UP_airbnb` and `zztkm/sqlc-python-example` on four profiles per server. Neither shipped Python tests. We could check what their queries returned, but could not measure what an existing caller would need to change. [Pinned entries](audits/python-application-tests.tsv) · [Four-profile scenario audit](migration-audit.md#verification-and-replay)

## Rechecking the results

The local archive `target/migration-audit/python-sqlc-application-tests.tar.gz` includes the searches, pinned revisions, fixture and caller edits, generated packages, dependency versions, JUnit results and query traces. From clean checkouts and fresh Python environments, we reproduced the same tests and generated calls for Clanker (24 tests) and sqlc-python-test (three). The checked-in ledgers keep the outcomes after the build directory is cleaned. The earlier generation audit has its own replay archive. [Earlier generation and runtime audit](migration-audit.md#verification-and-replay)
