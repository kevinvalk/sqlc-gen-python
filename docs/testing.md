# Testing

## What the tests read

**The examples** (`examples/`) are sqlc projects of our own: a `sqlc.yaml` and its SQL, and beside it the Python each `codegen` entry generates. They are the golden tests, as `cargo test` holds the plugin to that output byte for byte. `cargo xtask bless` rewrites it, once we have read the change and it is more correct. What sqlc sends the plugin for each is in `tests/sqlc/examples/`.

**sqlc's endtoend tests** (`tests/sqlc/endtoend/`) are the few hundred small projects sqlc tests its own Go codegen on (`internal/endtoend/testdata` in its repository). We keep what sqlc sends a plugin for each of them, with its schema, and generate every one under each profile of `profiles.json`: a backend and its options. Nobody wrote down what our Python should be for them, so they cannot be golden tests. Instead every output must hold what any correct output holds, and the runtime sweep runs those for PostgreSQL. They give us the query shapes we would never think of writing ourselves.

We commit them, rather than run sqlc, so that `cargo test` needs no sqlc, Docker or network. sqlc is MIT licensed and `NOTICE` carries its license. `SOURCE.toml` pins the sqlc release and commit, and `cargo xtask capture --check` proves the committed files are exactly what that release sends, so they are never edited by hand: a sqlc upgrade is `cargo xtask capture`. It also writes `inflection.tsv`: sqlc's singular of every table name in the tests and of some words beyond them, which our port of its inflection is held to. Two files there are ours:

- `profiles.json`: the options each request is generated under;
- `ledger.tsv`: for each request and each profile whose backend runs its engine, a hash of the output or the error it fails with, so a change shows what it fixes and what it breaks; `cargo xtask bless` rewrites it, and refuses a request that stops generating unless told `--allow-regressions`.

## The layers

Each layer catches what the one before cannot see. `cargo xtask help` lists the commands.

- **`cargo test`** pins every golden case byte for byte, and holds every golden and endtoend output to the invariants no type checker or runtime can see (`tests/rust/endtoend.rs`, each with a planted bug in `tests/rust/invariants.rs`): a NOT NULL column annotated `X | None`, two parameters bound to each other's names, a `# type: ignore`.
- **`cargo xtask check`** runs every distinct output through the static gate (`tests/python/gate.py`): ruff with every rule, pyright strict and mypy strict, as `tests/python/pyproject.toml` sets them. And the scanner proof (`tests/python/scanner.py`) holds the `text()` rewriting to libpg_query, the scanner sqlc parses with, on a few thousand statements Hypothesis draws from what PostgreSQL scans specially.
- **`cargo xtask verify`** (`tests/python/verify.py`) runs the generated code:
  - the gate over `tests/python/suite/usage.py`, which pins the types application code sees and the wrong values that must stay type errors;
  - pytest (`tests/python/suite/`): a scenario through every function of every backend, sync and async, on PostgreSQL, MySQL and SQLite, every value held to its annotation;
  - the type probe (`tests/python/typemap.py`): every mapped type at its edges through every driver, written to `docs/typemap.md`;
  - the runtime sweep (`tests/python/autoprop.py`): we take sqlc's SQL as correct, and hold the Python around it (the rewritten placeholders, the bound parameters, the columns read by position, the converted types, the annotations) to that SQL run directly. For every PostgreSQL package of the golden cases and the endtoend tests on `sqlalchemy` or `psycopg` without `unknown_type`, it seeds the tables and calls every function with drawn arguments, a hundred times, and every result must match its annotation. Only a sync function's result is compared with what sqlc's SQL returned, as a coroutine runs the same SQL, so an async package is held to its annotations alone. What sqlc misreports is in `tests/python/known-failures.tsv`. A function is exercised once a call reached what it reads: a row, or for a command, a call the database accepted. `tests/python/exercised.tsv` records each one, and a function no longer exercised fails the sweep until its line is deleted by hand. A sweep is the same from run to run, so a package's report is cached under `target/sweep/` by a hash of all it reads, and only a package whose output changed is swept again;
  - pytest and the probe again on the newest Python, with SQLAlchemy 2.1.
- **`cargo xtask capture --check`**, after proving the captured requests, is the one test of the built WASM as sqlc runs it (`wire_contract` in `tests/python/capture.py`): in each example `sqlc generate` writes exactly its golden cases and nothing outside them, and options sqlc must fail on (none, no `async`, a typo, an unsupported command, a backend that does not run the engine) fail it with the plugin's message, writing nothing.
  The wire tests exercise named PostgreSQL predicates against the driver, both orders of mixed MySQL scalar and slice binds, the missing length of a MySQL SET slice, ordinary MySQL and SQLite parameter names, and a SQLite write with a `RETURNING` clause beside a streaming read.
  The capture also checks the raw query text that pinned sqlc emits after a UTF-8 comment in a SQLite file. On a sqlc upgrade this tells us when to remove the ASCII restriction.
  The same run checks five PostgreSQL predicates with a misreported parameter, first in the form the plugin diagnoses and then with a cast sqlc can bind. It generates and executes four valid predicates with psycopg. They check the Python parameter annotations and returned rows for mixed-width integer comparisons, date subtraction, date-plus-time and a timestamp reused against a cast. It also compares the full requests for three pairs of PostgreSQL schemas. A table created with `AS`, an inheritance parent and a computed view each have a nullable column that sqlc reports exactly like a declared NOT NULL column. Both the catalog and the stored values are checked against a running PostgreSQL server.

With `--mutants`, `verify` plants plausible generator bugs (`MUTANTS` in `verify.py`) one at a time, and the layer named for each must catch it, saying why. With `--check`, as CI runs it, a `docs/typemap.md` or `exercised.tsv` that would change fails the run. `cargo xtask bless` runs `verify` before it accepts a change, as a wrong type that is still well typed passes the gate.

The PostgreSQL type probe binds JSON back as a nullable parameter, so reading JSON `null` as Python `None` loses the distinction from SQL NULL there. The invoicing scenario also binds `None` to a required JSON parameter and checks the stored document with `jsonb_typeof`.

The [public migration audit](migration-audit.md) records which downstream projects generated successfully, which needed changes, and which queries ran against their databases.

The opt-in downstream suite regenerates pinned external SQL with the current WASM and checks populated results, streaming and stored values on PostgreSQL, SQLite, MySQL and MariaDB. The audit records the generation diagnostics alongside the passing workloads. [Downstream workloads and replay commands](downstream-workloads.md)
