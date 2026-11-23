# Testing

## What the tests read

Each project in `examples/` contains SQL, a sqlc configuration and the Python we expect it to generate. We keep the corresponding plugin requests in `tests/sqlc/examples/`. That lets `cargo test` compare the output byte for byte without installing sqlc, Python or Docker.

We also keep requests from sqlc’s own Go generator tests in `tests/sqlc/endtoend/`. These cover queries we would rarely write in an example. We generate Python for each applicable profile (a backend and its options), check the output and run the PostgreSQL packages against a database. [Captured requests and sqlc’s MIT notice](../tests/sqlc/endtoend/)

| File in `tests/sqlc/endtoend/` | What it records |
| --- | --- |
| `SOURCE.toml` | The sqlc release and commit that produced the requests. Run `cargo xtask capture` to update them. |
| `inflection.tsv` | The singular table names sqlc produces, including extra words we check against our Rust implementation. |
| `profiles.json` | The options to use when generating each package. |
| `ledger.tsv` | The output hash or generation error for each request and profile. If a request stops generating, `bless` requires an explicit `--allow-regressions`. |

## The layers

| Command | What it checks |
| --- | --- |
| `cargo test` | Compares example output with the checked-in Python. Also checks rules across packages, such as which parameters bind to which placeholders and which fields must be required. |
| `cargo xtask check` | Runs the Rust checks and tests, then Ruff, strict pyright and strict mypy over the generated packages. Checks SQLAlchemy’s `text()` rewriting against the PostgreSQL scanner with statements drawn by Hypothesis. |
| `cargo xtask verify` | Runs the generated functions against PostgreSQL, MySQL and SQLite. Checks driver values against the annotations and type-checks Python callers, including calls that must stay type errors. |
| `cargo xtask capture --check` | Recreates the plugin requests with the pinned sqlc and compares them byte for byte. Runs the WASM through `sqlc generate` to check the files, paths and errors a user actually gets. |

### Running generated Python

The invoicing, billing and ledger scenarios exercise every generated function in those examples, sync and async. The driver probes read every mapped type at its edges and bind values back. For example, both SQL NULL and PostgreSQL JSON `null` read as Python `None`. We also pass `None` to a required JSON argument and check the stored document with `jsonb_typeof`. The scenarios and probes run on Python 3.11 and the newest supported Python, including SQLAlchemy 2.1. [Runtime suite](../tests/python/suite/) · [Driver results](typemap.md)

The PostgreSQL sweep seeds tables and tries up to 100 generated argument combinations per function. It covers example and upstream packages on psycopg and SQLAlchemy generated without the `unknown_type` option. We check each returned value against its annotation. For synchronous calls we also run sqlc’s SQL directly and compare the results. This catches a placeholder bound to the wrong argument even when both values have the same Python type. [Sweep implementation](../tests/python/autoprop.py)

A read counts as exercised once it returns a row. A command counts once the database accepts it. Losing a previously exercised function fails the sweep. We cache results under `target/sweep/` so we can rerun just the packages whose inputs changed. [Exercised functions](../tests/python/exercised.tsv) · [Known failures](../tests/python/known-failures.tsv)

### Running sqlc itself

For each example, we check that sqlc writes exactly the expected files. An invalid option or unsupported command must fail with the plugin’s message and write nothing. We also run queries where sqlc’s request has surprised us before. [Capture and WASM checks](../tests/python/capture.py)

- **PostgreSQL.** Five predicates reproduce incorrect parameter types. Four valid predicates check the generated annotations and returned rows for mixed-width integers, date subtraction, date plus time and a timestamp reused against a cast. For three pairs of schemas, we compare the complete requests, PostgreSQL’s catalog and the values it actually returns.
- **MySQL.** We check named parameters, scalar and slice parameters in both orders, and the missing length of a `SET` slice.
- **SQLite.** We check parameter names and a write with `RETURNING` alongside a streaming read. A separate capture checks where sqlc cuts two statements after a UTF-8 comment. Once both arrive whole, we can revisit the ASCII restriction.

### Accepting a change

Read the changed Python before running `cargo xtask bless`. A wrong annotation can pass a type checker, so `bless` runs the database checks before accepting changed output. CI uses `verify --check` to catch changes to the type map or exercised-function list. With `verify --mutants`, we plant one bug at a time and require the intended check to catch it. [Verification code](../tests/python/verify.py)

## Public projects

For v1.0.0, 716 of 1,166 public sqlc projects generated from their original SQL. All 2,874 generated packages passed Ruff, strict pyright and strict mypy. We ran 1,847 packages against databases and recorded two known exceptions to the annotations. [v1.0.0 results](https://github.com/kevinvalk/sqlc-gen-python/blob/v1.0.0/README.md)

Later audits checked three different things.

- Which projects generated Python, and which queries needed a cast or a schema change. [Generation audit](migration-audit.md)
- Which existing Python callers needed edits, and whether their own database tests still passed. [Application migration audit](application-migration-audit.md)
- What generated Python returned for other projects’ SQL on populated PostgreSQL, MySQL, MariaDB and SQLite databases. These scenarios can be rerun with the current WASM. [Downstream workloads and commands](downstream-workloads.md)
