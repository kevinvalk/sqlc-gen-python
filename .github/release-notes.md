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
          backend: psycopg # or sqlalchemy; sqlite3 or pymysql for sqlite or mysql
          async: false # true for coroutines
```

`backend` and `async` are required, and the options this release takes are in [its README](${SERVER}/${REPO}/blob/${TAG}/README.md#options).

${COMMITS}
