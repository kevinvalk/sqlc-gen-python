# Development

```sh
cargo test                    # Rust only: the examples byte for byte, and the rules every output must hold
cargo xtask check             # also ruff, pyright strict and mypy strict over every generated package
cargo xtask verify            # also runs the generated code on PostgreSQL, MySQL and SQLite
cargo xtask bless             # accepts changed output, once verify passes on it
cargo xtask capture --check   # the pinned sqlc still sends what tests/sqlc/ holds, and runs the WASM under it
cargo wasm                    # builds the plugin
```

Each check sees what the one before cannot. `cargo xtask help` says what each needs: `verify` and `capture` start their databases in Docker.

## Looking at one query

To see what the plugin makes of a statement, let sqlc write the request it sends with its own `json` codegen, and run this checkout on that. When a type is wrong, read the request first, as what sqlc reports decides most of it.

```yaml
# sqlc.yaml, beside a schema.sql and a query.sql
version: "2"
sql:
  - engine: postgresql
    schema: schema.sql
    queries: query.sql
    gen:
      json: {out: out, filename: request.json}
```

```sh
sqlc generate
cargo xtask generate out/request.json '{"backend": "psycopg", "async": false}'
```

## How the parts fit

`generate` in `src/lib.rs` is the whole plugin: it reads the options (`config`), decides the API once (`api`) and writes that as Python (`render`, `python`). The API asks the `engine` what the database's types are in Python and how its SQL reads, the `backend` how its library runs a statement, and `sqlc_typing` for what sqlc leaves out or gets wrong. Each module says what it owns in its opening lines.

## Replaying other projects

`tests/python/suite/downstream/replay.py` runs the sqlc queries of other projects through the built WASM and calls the generated Python on real databases. `projects.tsv` beside it pins each project and says why an entry did not generate.

```sh
cargo wasm
cd tests/python
uv run --locked python -m suite.downstream.replay --server sqlite  # or mysql, mariadb, postgresql
```

## Releasing

Every commit on `main` that passes CI is published as the `unstable` prerelease, a tag that moves. Git will not move a tag it already has, so allow it once in each clone:

```sh
git config --add remote.origin.fetch '+refs/tags/unstable:refs/tags/unstable'
```

A version is only its tag, on a commit of `main`. CI then publishes the WASM with its attestation and the release notes, and commits the release's URL and sha256 into the README.

```sh
git fetch origin && git tag vX.Y.Z origin/main && git push origin vX.Y.Z
```

Renovate merges an update once its release is a week old and CI passes on it. One that fails becomes a pull request, to finish by hand:

- sqlc: `cargo xtask capture`, with `POSTGRES` in `tests/python/capture.py` at the version sqlc's own tests run;
- ruff: the crates' tag and the Python pin move together, then `cargo update`, and `cargo xtask bless` if the formatter writes anything differently;
- PostgreSQL, MySQL or a driver: `cargo xtask verify`, to rewrite `docs/typemap.md`;
- uv: widen `required-version` in `tests/python/pyproject.toml`.
