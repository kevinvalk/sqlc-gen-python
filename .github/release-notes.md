Tested under sqlc v${SQLC}. Add the plugin to `sqlc.yaml`:

```yaml
version: "2"
plugins:
  - name: python
    wasm:
      url: ${SERVER}/${REPO}/releases/download/${TAG}/sqlc-gen-python.wasm
      sha256: ${SHA256}
sql:
  - engine: postgresql
    schema: schema.sql
    queries: query.sql
    codegen:
      - out: src/db
        plugin: python
        options:
          backend: psycopg # or sqlalchemy; sqlite3 for engine: sqlite
          async: false # true for coroutines
```

`backend` and `async` are required. The options this release takes are in
[the output specification, §7](${SERVER}/${REPO}/blob/${TAG}/docs/python-output.md#7-options).
The plugin needs sqlc's static analysis: with a `database` block, add
`analyzer: {database: false}`
([§5, rule 2](${SERVER}/${REPO}/blob/${TAG}/docs/python-output.md#type-map)).

sqlc refuses a download whose sha256 differs. To check a downloaded file against
`SHA256SUMS`, and that it was built from ${TAG} by this repository's release
workflow on a GitHub-hosted runner:

```sh
gh release download ${TAG} --repo ${REPO}
sha256sum -c SHA256SUMS
gh attestation verify sqlc-gen-python.wasm --repo ${REPO} \
  --signer-workflow ${REPO}/.github/workflows/release.yml \
  --source-ref refs/tags/${TAG} --deny-self-hosted-runners
```
