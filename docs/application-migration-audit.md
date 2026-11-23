# Python application migration audit

On 22 November 2026, 25 of 26 projects with a working query-backed baseline passed the same selected tests after migration, counting the MCP-Universe user API once. The 25 successful selections contain 444 tests. Nine additional repositories that vendor that API each passed both tests on both generators. A separate models-only project passed 11 tests without caller changes. [Results by engine](../README.md#migrating-from-another-python-sqlc-generator)

## What we counted

| Unit | Result | How to read it |
| --- | ---: | --- |
| Public repository candidates | 147 | The previous 136 pins plus 11 new search hits. |
| Excluded candidates | 62 | Seven generator fixtures or copies of upstream generator tests, 44 previous hits without an eligible configuration, and 11 new documentation, tool catalog or fixture hits. |
| Downstream repositories | 85 | Applications, libraries, examples and templates. One has entries for all three engines. |
| Configured entries | 104 | 100 generated on all four applicable profiles, across 81 projects. Four entries lacked authoritative inputs. |
| Projects with Python test files | 53 | 32 of 85 had none. Each file named `test_*.py`, `*_test.py`, `test.py` or `tests.py` was a candidate for inspection. |
| Projects taken to execution | 37 | The remaining 16 had missing inputs or tests for which inspection did not establish a generated database operation. Each has a reason in the ledger. |
| Passing original baselines | 36 | 35 query workloads and one models-only workload. One original smoke test failed during import. |
| Query-backed ports | 34 / 35 | 25 of 26 with the MCP-Universe workload counted once, plus 9 of 9 additional copies. The remaining port failed during collection on its original Python 3.9 stack. |

The project ledger gives each revision, engine, configuration, test-file count, outcome and reason. The run ledger gives the exact pytest selection and working directory, Python version, pass/fail/skip counts and generated functions observed. A blank count means that phase produced no JUnit results. The generation audit also identifies which entries needed additional options with their SQL unchanged. [Project ledger](audits/python-application-tests.tsv) · [Run ledger](audits/python-application-runs.tsv) · [Generation audit](migration-audit.md)

### Discovery

We combined the original inventory with 17 additional GitHub code searches for plugin names in sqlc YAML and JSON, Nix, READMEs, sync/async options, and generated-file headers in Python. The refresh found 114 distinct repositories, including 11 outside the original inventory. We inspected the matched source at each new pin. One hit was a Go generated-file header inside a Python test string. A historical sample in the original inventory, Atlantic, has its Python output configuration and its dashboard calls commented out. Its seven queries generated, but it contributes no active project or upstream test to the tables. [Original 136 pins](audits/python-sqlc-projects.tsv) · [Eleven additional pins and decisions](audits/python-search-additions.tsv)

GitHub code search indexes public default branches under its indexing rules and caps a query at 1,000 returned items. We repeated the broad Python-header search in three file-size bands, which returned 682, 310 and 94 hits. The counts describe the public repositories found by these searches. Private projects, unindexed files, other branches and projects that leave no searchable Python sqlc evidence can be missed. [GitHub search API](https://docs.github.com/en/rest/search/search#search-code)

### Comparing the two generators

We used the project's checked-in generated package, or its pinned original generator for the CRUD smoke script. For the port we regenerated with sqlc v1.31.1 and this plugin's WASM (SHA-256 `6b800798b05cd1b276b454c27ac639e9328da4d34497a71e54bc03da67cd57a1`). Each phase ran against a fresh database with the same schema, test selection and fixture repairs. Passing means the same named cases passed, with the same skips, after changing the generated package and its consumers. Most projects supplied a focused integration suite. The ledger states the scope so a passing selection can be distinguished from a whole-application migration.

A Python call-stack trace recorded the generated function when execution entered a database driver or SQLAlchemy execute method during a test. For the two b-stead HTTP services we also traced the server process. For one CRUD script we verified the original generated SQL constants used in direct SQLAlchemy calls. We checked the source headers after migration to exclude old generated functions still on the exercised path. A test suite may also exercise routing, validation and mocks. The passing test count is the size of the selected suite. The function lists measure observed database use and do not estimate line or branch coverage.

We classified the greatest change needed to generate the selected entry and run its tested callers. **Drop-in** allows choosing the plugin, backend and mode without changing callers or SQL. **Mechanical** covers imports, connection arguments, records, keyword names and list iteration. **Targeted** includes SQL or recovered schema, nullable values, JSON representation or enum mappings. **Substantial** means a change to connection ownership and transactions or to a third-party database adapter's interface. The classifications describe actual edits in the exercised paths. They do not estimate engineering hours or the cost of converting untested application code.

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

The 23 successful query selections above the platform failure exercised **232 generated functions across packages**, counting a function once per project and generated module. The common-library suite for Smartnews contributed 99 passing tests and 11 observed functions. Smart Credit Office contributed four tests and 48 functions, illustrating how differently two suites can exercise SQL. The nine MCP-Universe copies each regenerated their own package and exercised `create_user` and `get_user_by_email` against PostgreSQL. The two photo-approval applications share their test file but have different schema and query inputs, and both ran. [Run ledger](audits/python-application-runs.tsv)

| What changed or failed | Evidence from the port |
| --- | --- |
| JSON input | NinjaMagic passed pre-encoded game state to a JSON argument. Passing the Pydantic model's JSON-compatible dictionary fixed the inventory round trip. Smart Credit Office supplied documents in place of its driver's wrapper. The generated function owns serialization in both cases. [NinjaMagic inventory](https://github.com/dzaramelcone/ninjamagic-py/blob/d7fadb33fad43586df78c310ad194d6ba5f9172d/ninjamagic/inventory.py) |
| Nullability and enums | The brain dashboard needed to handle a group with no non-null start time. An HTTP service needed a nullable named status bind. The blood-bank app needed a JSON cast and distinct Python names for positive and negative blood groups. Its full backend suite passed all 53 tests. [Brain Dashboard queries](https://github.com/rafi123123/TAU-brain-dashboard/blob/47fa0b6df307290be19a7cbe0284482f7ffce6a4/api/db/query.sql) · [Blood-bank project](https://github.com/japetsch/dat251-group-c/tree/ea2e0791b4f762ded71fcbf9ce671b287e2ad67e) |
| Native asyncpg | Nixbot's original pool rejected a SQLAlchemy statement. The exercised application paths now use SQLAlchemy's async engine on asyncpg, with an explicit transaction and Python JSON values. The same six generated operations ran in both phases. The two tests still check cancellation, status and timestamps using direct SQL. [Nixbot persistence tests](https://github.com/Mic92/nixbot/blob/171a8c77d6e8e2b5b99ce5870c042a2dbc2cda62/nixbot/nixbot/tests/test_db.py) |
| SQLAlchemy emulation | A library implementing its own SQLAlchemy interface assumed binds named `p1`, `p2`. Keeping bind order and implementing result iteration and `one_or_none()` got its existing insert, lookup and list assertions passing. Its one upstream test covers those methods. [Shim test](https://github.com/tandemdude/sqlc-asyncpg-compat/blob/1663d6cf9211bcd50a0a8c8a6055d8806e662eca/tests/test_function.py) |
| Unsupported Python | The template passed 15 admin tests on Python 3.9. The regenerated models failed to import `enum.StrEnum`. Its requirements also pin SQLAlchemy 1.4.32 and Pydantic 1.9.0. A platform upgrade is outside the measured port. [Pinned requirements](https://github.com/danicc097/FastAPI-React-Postgres-Docker-Traefik-template/blob/2f63b72f35ae42ea57ab9471fd955e5fc22ce56f/backend/requirements.txt) |
| Broken original | Hyrex's own smoke test imports a dispatcher module absent from that revision. Replacing the worker scenario would change what was being tested. [Original smoke test](https://github.com/hyrex-labs/hyrex-python/blob/a082030a835327268aa2d088de9edfdb671b084e/tests/smoke_test.py) |

Some upstream fixtures needed repairs before either phase could be trusted. We supplied missing migrations or sequences, directed services at a disposable database, and gave asynchronous fixtures the lifetime their tests require. One RSS test used live DNS to check email deliverability and was deselected on both sides. Tarun's CRUD smoke script caught exceptions and printed errors. In both runs we re-raised exceptions and added the same checks for creation, retrieval, update, list membership and soft deletion. Its single case includes audit-authored assertions. The models-only OSINT suite ran its own SQL in both phases. [Original CRUD smoke script](https://github.com/TarunrRajanala/test-repo/blob/a4c0ca439db909a72eb2d00e057c206fc1729491/test_sqlalchemy_write.py) · [OSINT project](https://github.com/geldois/osint-engine/tree/be31b3e2e1794fb35793e8c70134e632ce2f34c3)

The round trips reproduced the existing nullability and JSON contracts. A Python value serialized twice, a raw pool passed to a SQLAlchemy function and a Python 3.9 import each fail for a reason outside SQL type inference. None of the observed failures called for a new rule in `sqlc_typing`. [Generator type guarantees and limitations](../README.md#known-limitations)

## SQLite

| Project | Original → port | Tests with an observed generated call in the port | Distinct port functions |
| --- | ---: | ---: | ---: |
| Clanker, feedback store | 24 → 24 | 18 | 4 |
| X-Ray Proxy, full suite | 21 → 21 | 3 | 2 |

Clanker checks filters, duplicate handling and persistence across reopening the database. X-Ray Proxy writes ship graphics through generated Python, reads them back and compares the application's response dictionaries. Both entries needed SQL and caller changes. Five of seven SQLite projects lacked Python test files, and one of those five also lacked a required SQL input. Six entries separately passed our populated scenarios on four profiles each. [Clanker feedback tests](https://github.com/alex-furrier/clanker/blob/626926ea1b7b3369c3ff64d3b2d50b8dbcb0d1e4/tests/test_sql_feedback.py) · [X-Ray Proxy tests](https://github.com/rsky/x-ray-proxy/tree/ec3a1b2309626289102c3742e681ccaf235641ba/tests) · [Scenario results](migration-audit.md#verification-and-replay)

## MySQL and MariaDB

The two MySQL projects were `rizkia-as-actmp/UP_airbnb` and `zztkm/sqlc-python-example`. Both generated from their original SQL. Neither supplied Python tests. We had already exercised both with project-specific scenarios on MySQL and MariaDB 11.4. Two projects × four profiles gives eight passing runs on each server. The upstream-suite success rate, drop-in rate and cost of changing a deployed caller remain unknown. [Pinned entries](audits/python-application-tests.tsv) · [Four-profile scenario audit](migration-audit.md#verification-and-replay)

## Rechecking the results

The local evidence archive at `target/migration-audit/python-sqlc-application-tests.tar.gz` contains the GitHub search queries and returned paths, the fixture and caller changes, generated packages, Python dependency versions, JUnit results and query traces. Its replay instructions use checkouts at the recorded Git revisions. We verified the archive from clean checkouts and fresh Python environments for Clanker (24 tests) and sqlc-python-test (three tests). Both phases reproduced the named cases and the observed generated functions. The checked-in ledgers preserve the project outcomes and named query evidence when the build directory is cleaned. The scenarios in the earlier generation audit have their own replay packet and different denominators. [Earlier generation and runtime audit](migration-audit.md#verification-and-replay)
