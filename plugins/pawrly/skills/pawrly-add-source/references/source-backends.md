# Source backends reference

Per-kind `config:` and `tables:` for every Pawrly source kind. Full prose: [docs/sources.md](https://github.com/CITGuru/pawrly/blob/main/docs/sources.md). Kitchen-sink config: [examples/pawrly.yaml](https://github.com/CITGuru/pawrly/blob/main/examples/pawrly.yaml). 

Three backends have their own deep-dive page: [http-backend.md](http-backend.md), [openapi.md](openapi.md), and [mcp-backend.md](mcp-backend.md).

## Column type spellings

Used by `file` `schema:`/`partition_cols` and `http` `response.schema`. Spellings are case-insensitive; anything unrecognized falls back to `varchar`.

| Type | Accepted spellings | Notes |
|------|--------------------|-------|
| boolean   | `bool`, `boolean`            | |
| 32-bit int | `int`, `int32`              | |
| 64-bit int | `bigint`, `int64`, `long`   | |
| 32-bit float | `float`, `float32`        | |
| 64-bit float | `double`, `float64`       | |
| date      | `date`                       | |
| string    | `varchar`, `string`, `text`  | default when type is omitted |
| timestamp | `timestamp`                  | `http` only |
| timestamp (tz) | `timestamptz`           | `http` only; RFC 3339 |
| json      | `json`                       | `http` only; raw JSON text |

---

## `file` — local files

Glob auto-discovers one table per file stem; or declare `tables:` for control.

```yaml
- name: data
  kind: file
  config:
    path: ./data/*.parquet      # single file | glob | directory
```

Per-table fields (flat under each `tables:` entry): `name` and `path` are required; the rest are optional — `format` (`parquet|csv|json`, inferred from the path extension), the format-specific blocks `csv` and `json`, plus `schema` and `partition_cols`.

```yaml
  tables:
    - name: metrics
      path: ./data/metrics.tsv
      format: csv
      csv: { header: false, delimiter: "\t" }   # header(true), delimiter(,), quote(")
      schema:                                    # name+type a headerless file
        - { name: host,  type: varchar }
        - { name: value, type: bigint }
    - name: facts
      path: ./data/facts.json
      json: { format: array }                    # array | ndjson | auto
```

**Partitions** — make partition keys queryable columns (one style per table):

```yaml
    - name: events            # events/dt=2026-05-31/region=us/*.parquet
      path: ./lake/events
      format: parquet
      partition_cols:
        - { name: dt,     type: date }           # hive (default): prunes by directory
        - { name: region, type: varchar }
    - name: sessions          # projects/<project>/*.jsonl  (non key=value layout)
      path: ./projects/*/*.jsonl
      format: json
      partition_cols:
        - { name: project, type: varchar, kind: segment, index: 0 }
```

## `file` — object storage (S3 / GCS / Azure)

Add `storage:`; **explicit `tables:` required**, each a single concrete URL.

```yaml
- name: lake
  kind: file
  config:
    storage:
      type: s3                  # s3 | gcs | azure | http
      region: us-east-1         # location, not a credential
      auth:
        type: access_key        # access_key | credential_chain (alias: chain)
        access_key_id: ${secret:AWS_KEY_ID}
        secret_access_key: ${secret:AWS_SECRET}
        # endpoint: ${env:AWS_ENDPOINT}   # MinIO / R2 / other S3-compatible
  tables:
    - { name: events, path: s3://bucket/events/*.parquet, format: parquet }
```

- Auth fields by `type`: **s3** `access_key_id, secret_access_key, session_token, endpoint, url_style`; **gcs** HMAC `access_key_id, secret_access_key`; **azure** `connection_string, account_name`; **http** `header`/`basic`; **any** `credential_chain` (ambient: env / instance profile / `gcloud` / `az login`).
- **Public** buckets and HTTPS files need no `storage:` block — a remote scheme (`s3://`, `gs://`, `az://`, `http(s)://`) auto-routes through DuckDB.
- Remote files use DuckDB readers, so local `csv`/`json`/`partition_cols`/`schema` options don't apply; `http(s)://` paths cannot be globbed (bucket globs are fine).

## `http` — REST & GraphQL APIs

Source-level `config.base_url` is **required**. See [http-backend.md](http-backend.md) for request/response/pagination/auth detail.

```yaml
- name: gh
  kind: http
  config:
    base_url: https://api.github.com
    token: ${secret:GITHUB_TOKEN}        # bearer shorthand; or a full auth: block
    headers: { Accept: application/vnd.github+json }   # constant headers on every call
  raw_table: true                         # optional escape hatch: SELECT ... FROM gh WHERE request_path = '/...'
  tables:
    - name: pulls
      endpoint: /repos/{owner}/{repo}/pulls
      params:
        - { name: owner, required: true }
        - { name: repo,  required: true }
        - { name: state, required: false, default: open }
      response:
        path: $
        schema:
          - { name: number, type: bigint }
          - { name: title,  type: varchar }
          - { name: state,  type: varchar }
      pagination: { type: link_header }
```

**OpenAPI mode** — one table per `GET`, no hand-writing. See [openapi.md](openapi.md) for the full synthesis options:

```yaml
- name: stripe
  kind: http
  config:
    type: openapi
    base_url: https://.../openapi.spec3.yaml   # the SPEC url; API base from spec servers
    auth: { type: header, headers: [{ name: Authorization, bearer: ${secret:STRIPE_API_KEY} }] }
    openapi:
      include: { paths: ["/v1/charges*", "/v1/customers*"] }   # default: every GET
      cache: { ttl: 24h }
```

A `tables:` entry whose name matches a synthesized table **patches** it (set only the field to fix; arrays and type-tagged blocks replace wholesale; `null` clears).

## `mcp` — another MCP server's tools as tables

See [mcp-backend.md](mcp-backend.md) for transports, the `expose` dial, column pinning, and security detail.

```yaml
- name: linear
  kind: mcp
  config:
    transport: streamable_http     # or stdio (with command: [...] + env:)
    url: https://mcp.linear.app/mcp
    expose: read_only              # read_only (default) | all | listed
    auth: { type: header, headers: [{ name: Authorization, bearer: ${secret:LINEAR_API_KEY} }] }
  tables:                          # optional: pin typed columns
    - name: list_issues
      columns:
        - { name: id,    type: varchar, path: [id] }
        - { name: title, type: varchar, path: [title] }
        - { name: status, type: varchar, path: [status] }
```

- `expose`: `read_only` exposes tools with `readOnlyHint`; `all` exposes every non-destructive tool; `listed` exposes only `tables:`/`include:`. `destructiveHint` tools are never auto-exposed. `include`/`exclude` (by tool name) narrow further.
- `tables:` knobs: `tool`, `columns` (`path: [keys]`; empty path = whole element as JSON), `tool_args`, `filters` (bind a SQL filter to a differently-named arg), `limit_binding`, `pagination`.
- `streamable_http` `url` must be `https` (or loopback `http`) and carry no inline creds — use the `auth` block. Runnable: [examples/linear/pawrly.yaml](https://github.com/CITGuru/pawrly/blob/main/examples/linear/pawrly.yaml), [examples/mcp.yaml](https://github.com/CITGuru/pawrly/blob/main/examples/mcp.yaml).

## Databases

```yaml
- name: oltp
  kind: postgres             # aliases: pg, postgresql. mysql is analogous.
  config:                    # or a single dsn: <connection string>
    host: db.internal
    database: app            # or dbname:
    user: readonly
    password: ${secret:PG_PASSWORD}

- name: app
  kind: sqlite
  config: { path: ./app.db } # :memory: allowed
  # tables: [{ name: active_users, query: "SELECT id, email FROM users WHERE active = 1" }]

- name: local_db
  kind: duckdb
  config: { path: ./analytics.duckdb }

- name: warehouse
  kind: snowflake
  config:
    account: acme.us-east-1
    user: ${secret:SNOWFLAKE_USER}
    password: ${secret:SNOWFLAKE_PASSWORD}
    database: ANALYTICS       # optional: schema, warehouse, role
    schema: PUBLIC
```

Attach-style kinds expose the live catalog read-only; equality/projection/limit push down. `tables:` is ignored except for `sqlite` (which honors a `query:`). Use a semantic model for curated views over an attached DB.

## Lakehouse formats

```yaml
- name: lake
  kind: iceberg                # or delta (alias deltalake) — tables: REQUIRED
  tables:
    - { name: orders, path: s3://bucket/warehouse/orders }   # path or location

- name: dl
  kind: ducklake
  config:
    catalog: ./metadata.ducklake     # sqlite/duckdb/postgres metadata catalog
    data_path: ./lake_data           # local or s3://… (+ optional storage: block)
```

For object-store tables, add a `storage:` block (same keys as object-store `file`); the `httpfs` extension loads automatically.