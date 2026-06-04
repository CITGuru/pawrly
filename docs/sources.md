# Sources

A **source** is a named connection to some external system or set of files, exposed to the query engine as one or more tables. Every table is addressed in SQL as `<source>.<table>` — the source `name` is the schema prefix. Sources are declared under `sources:` in your workspace `pawrly.yaml` (see [Configuration](./config.md), and the [examples/pawrly.yaml](../examples/pawrly.yaml) worked file).

```yaml
sources:
  - name: data
    kind: file
    config:
      path: ./data/*.parquet
```

```sql
SELECT * FROM data.orders          -- table `orders` in source `data`
```

Pawrly has **two foundational backends** — `file` (local files, or object storage) and `http` (any REST/GraphQL API) — plus a small set of **first-class database / lakehouse builtins** that run through an in-process DuckDB engine.

This page is the reference for the **source block** (every top-level field) and the **per-kind config** for each kind. For the surrounding config — secrets, caching internals, safety semantics, defaults, and multi-file assembly — see [Configuration](./config.md).

---

## The source block

Every entry under `sources:` is one source. These are the top-level fields:


| Field              | Type    | Required | Default         | Notes                                                                                                |
| ------------------ | ------- | -------- | --------------- | ---------------------------------------------------------------------------------------------------- |
| `name`             | string  | **yes**  | —               | SQL identifier; becomes the schema prefix (`name.table`). Must be unique.                            |
| `kind`             | enum    | **yes**  | —               | The source kind — see [Source kinds](#source-kinds). Case-insensitive; some kinds have aliases.      |
| `description`      | string  | no       | —               | Free text; surfaced in `pawrly source list`.                                                         |
| `config`           | mapping | no¹      | `{}`            | Per-kind settings (connection, auth, paths, storage, …). Shape depends on `kind`.                    |
| `tables`           | list    | no¹      | `[]`            | Explicit per-table declarations. Required for some kinds, optional for others (which auto-discover). |
| `cache`            | mapping | no       | `mode: none`    | Per-source caching. See [the cache block](#the-cache-block).                                          |
| `safety`           | mapping | no       | permissive      | Per-source guard rails. See [the safety block](#the-safety-block).                                    |
| `raw_table`        | bool    | no       | `false`         | `http` only: register a raw-HTTP escape-hatch table named after the source.                          |
| `raw_table_safety` | mapping | no       | filter-required | Safety policy for the raw table when `raw_table: true`.                                              |


¹ Whether `config` or `tables` is required depends on the kind (see each kind below).

The config-layer source block also accepts `from:` (load the body from another file) — see [Configuration → Multi-file configs](./config.md#multi-file-configs).

### `name`

A valid SQL identifier (letter or `_`, then alphanumerics/`_`). It's the schema under which the source's tables are registered, so `SELECT … FROM <name>.<table>`. Names must be unique across the merged config.

### `kind`

The kind selects the backend and the shape of `config`/`tables`. The list is closed (adding a kind is a code change). Matching is case-insensitive; the aliases below resolve to the same kind.


| Kind        | Aliases            | Backend                                                                                   |
| ----------- | ------------------ | ----------------------------------------------------------------------------------------- |
| `file`      | —                  | DataFusion native readers (local), or DuckDB object-store reads (with a `storage:` block) |
| `http`      | —                  | native HTTP table provider                                                                |
| `sqlite`    | —                  | read-only attach                                                                          |
| `postgres`  | `pg`, `postgresql` | DuckDB `ATTACH` (read-only)                                                               |
| `mysql`     | —                  | DuckDB `ATTACH` (read-only)                                                               |
| `duckdb`    | —                  | DuckDB `ATTACH` of a local `.duckdb` file (read-only)                                     |
| `snowflake` | —                  | DuckDB `ATTACH` (community extension)                                                     |
| `iceberg`   | —                  | DuckDB `iceberg_scan`                                                                     |
| `delta`     | `deltalake`        | DuckDB `delta_scan`                                                                       |
| `ducklake`  | —                  | DuckDB `ATTACH 'ducklake:…'`                                                              |


> The DuckDB-backed kinds load a DuckDB extension on first use (e.g. `postgres`, `iceberg`, `ducklake`, `httpfs`; Snowflake's is a community extension). The first registration in a fresh environment may need network access to fetch the extension.

### `description`

Optional human-readable text. No effect on behavior; shown by `pawrly source list`.

### `config`

A per-kind mapping, opaque to the config layer and interpreted by the kind's builder (so each kind documents its own keys below). Strings here may use `${secret:NAME}`, `${env:NAME}`, and `${file:PATH}` interpolation (see [Configuration → Secrets](./config.md#secrets)).

### `tables`

Explicit per-table declarations. **Per-table fields are written flat** — the kind-specific keys (`path`, `format`, `endpoint`, `params`, `response`, `query`, …) sit directly under the table entry, not under a nested `config:`:

```yaml
tables:
  - name: orders                 # required; the SQL table name
    description: Daily orders     # optional
    path: ./data/orders.parquet   # ← kind-specific fields, flat
    format: parquet
    cache:  { mode: ttl, ttl: 1h }  # optional; overrides the source-level cache
    safety: { max_rows: 100000 }    # optional; overrides the source-level safety
```

Only `name`, `description`, `cache`, and `safety` are common; everything else is kind-specific. Some kinds **auto-discover** tables when `tables:` is omitted (`file` globs, `sqlite`/`postgres`/`mysql`/`duckdb`/`snowflake`/`ducklake` enumerate). Others **require** `tables:` (`iceberg`, `delta`, and object-store `file`).

### The `cache` block

Opt-in caching for a source (or an individual table). With no block, reads always go live.

```yaml
cache:
  mode: ttl        # none | ttl | refresh | cron | append
  ttl: 10m
```


| `mode`    | Extra field            | Behaviour                                                                                |
| --------- | ---------------------- | ---------------------------------------------------------------------------------------- |
| `none`    | —                      | No caching (default when `cache:` is absent).                                            |
| `ttl`     | `ttl: <dur>`           | Serve the cached result until `ttl` elapses, then re-fetch on the next read.             |
| `refresh` | `every: <dur>`         | Always read the cache; a background loop re-fetches every `every`.                       |
| `cron`    | `cron: "<expr>"`       | Like `refresh`, scheduled by a cron expression.                                          |
| `append`  | `cursor_column: <col>` | Incremental: only rows newer than the cached `cursor_column` max are fetched on refresh. |


Durations use humantime (`30s`, `10m`, `1h`). Storage location, namespacing, and cache-management commands are covered in [Configuration → Caching](./config.md#caching).

### The `safety` block

Guard rails enforced before a scan runs. All fields are optional and default to permissive.

```yaml
safety:
  require_filters_on: [order_date]   # error unless a filter touches each of these columns
  require_at_least_one_filter: true  # refuse a full-table scan
  max_rows: 1000000                  # hard cap on returned rows
  max_pages: 50                      # cap on HTTP pagination calls
  timeout: 30s                       # per-query timeout
  required_predicates:               # predicates AND-ed into every scan
    - "tenant_id = ${param:tenant_id}"
```

`required_predicates` is most useful with the [semantic layer](./semantic.md), where `${param:NAME}` placeholders are bound from query params as safe literals (row-level security). See [Configuration → Safety](./config.md#safety).

### `raw_table` / `raw_table_safety`

For `kind: http` only, `raw_table: true` registers an escape-hatch table named after the source for endpoints with no typed spec. You provide the request as filters; Pawrly returns the raw response as rows. Columns:


| Column            | Type    | Notes                                 |
| ----------------- | ------- | ------------------------------------- |
| `request_method`  | varchar | defaults to `GET` if not filtered     |
| `request_path`    | varchar | **filter required** (`=` or `IN (…)`) |
| `request_query`   | varchar | optional query string                 |
| `response_status` | int     | HTTP status code                      |
| `response_body`   | varchar | raw response body                     |


```sql
SELECT response_status, response_body
FROM gh                                   -- the source itself is the raw table
WHERE request_path = '/rate_limit'
```

`raw_table_safety` overrides the default policy, which requires a filter on `request_path` (so a bare `SELECT *` can't fan out arbitrarily).

---

## Source kinds

### File Backend (`file)` — local files & object storage

The `file` backend serves columnar and row files. **Local** files use DataFusion's native readers; **object storage** (S3/GCS/Azure) is expressed with a `storage:` block and read through DuckDB. A `file` source needs **either** a top-level `config.path` glob **or** at least one `tables:` entry.

```yaml
sources:
  - name: data
    kind: file
    config:
      path: ./data/*.csv          # glob; one table per file, named by file stem
```

Per-table fields are written **flat** under each `tables:` entry: `path`, `format`, `csv`, `json`, `schema`, `partition_cols`. The three big topics — formats, globs/partitioning, and object storage — follow.

#### File formats

`format` is one of `parquet`, `csv`, `json`. It's **inferred from the file extension** when omitted (`.parquet` → parquet; `.csv` → csv; `.json` / `.jsonl` / `.ndjson` → json). Specify it explicitly for extensionless paths or directories.

**CSV** — override the dialect with a `csv:` block (all optional):


| Key         | Default | Notes                                                                                     |
| ----------- | ------- | ----------------------------------------------------------------------------------------- |
| `header`    | `true`  | First row is a header. Set `false` for headerless files (pair with an explicit `schema`). |
| `delimiter` | `,`     | Single character. `"\t"` is accepted for tab.                                             |
| `quote`     | `"`     | Single quote character.                                                                   |


```yaml
    tables:
      - name: metrics
        path: ./data/metrics.tsv
        format: csv
        csv: { header: false, delimiter: "\t" }
        schema:                       # name + type the columns for a headerless file
          - { name: host,  type: varchar }
          - { name: value, type: bigint }
```

**JSON/JSONL** — files may be newline-delimited (NDJSON) or a single `[ … ]` array. The layout is auto-detected from the first non-whitespace byte; force it with a `json:` block:

```yaml
      - name: facts
        path: ./data/facts.json
        format: json
        json: { format: array }       # array | ndjson | auto (default)
```

**Explicit schema** — a `schema:` list of `{ name, type }` overrides inference (useful for headerless CSV or mis-inferred columns). Column `type` values (here and in `partition_cols`): `bool`/`boolean`, `int`/`int32`, `bigint`/`int64`, `float`/`float32`, `double`/`float64`, `date`, and `varchar` (the default for anything else).

#### File Partitions

A per-table `path` may be:

- a **single file** — `./data/orders.parquet`;
- a **glob** — `./data/orders/*.parquet` (all matches unioned into one table);
- a **directory** — `./lake/events` (every file beneath it, read as one table).

For partitioned datasets, declare `partition_cols` so the partition keys become queryable columns. This applies to all three formats (`parquet`, `csv`, `json`). Two styles, **one per table**:

**Hive Partition** — `key=value` directories (e.g. `events/dt=2026-05-31/region=us/*.parquet`). The keys are exposed as columns and **prune by directory** (a filter on `dt` skips non-matching folders). Streams through the file reader.

```yaml
      - name: events                  # events/dt=…/region=…/*.parquet
        path: ./lake/events
        format: parquet
        partition_cols:
          - { name: dt,     type: date }
          - { name: region, type: varchar }
```

```sql
SELECT * FROM data.events WHERE dt = '2026-05-31'   -- only that dt= directory is read
```

**Segment** — positional partitions for layouts that *aren't* `key=value`. Each column takes its value from the directory name at a zero-based `index` beneath the glob base. Segment-partitioned tables are materialized in memory, so they don't prune.

```yaml
      - name: sessions                # projects/<project>/*.jsonl
        path: ./projects/*/*.jsonl
        format: json
        partition_cols:
          - { name: project, type: varchar, kind: segment, index: 0 }
```

Each `partition_cols` entry is `{ name, type (default varchar), kind: hive | segment (default hive), index (required for segment) }`.

#### Object storage (S3 / GCS / Azure)

Add a `storage:` block to read from a bucket. `storage.type` selects the provider; `storage.region` and the bucket URLs are the location; credentials live under a typed `storage.auth` block. Object-store `file` sources **require explicit `tables:`**, each pointing at a remote URL.

```yaml
sources:
  - name: lake
    kind: file
    config:
      storage:
        type: s3                      # s3 | gcs | azure
        region: us-east-1             # location, not a credential
        auth:
          type: access_key            # access_key | credential_chain
          access_key_id: ${secret:AWS_KEY_ID}
          secret_access_key: ${secret:AWS_SECRET}
          # endpoint: ${env:AWS_ENDPOINT}   # S3-compatible stores (MinIO, R2, …)
    tables:
      - name: events
        path: s3://my-bucket/events/*.parquet
        format: parquet               # parquet (default) | csv | json
```

`auth.type` selects the method (default `access_key`); each provider supports more than one:


| `type`  | `auth.type`        | Fields                                                                                                                      |
| ------- | ------------------ | --------------------------------------------------------------------------------------------------------------------------- |
| `s3`    | `access_key`       | `access_key_id`, `secret_access_key`, `session_token`, `endpoint`, `url_style`                                              |
| `gcs`   | `access_key`       | `access_key_id`, `secret_access_key` (HMAC keys)                                                                            |
| `azure` | `access_key`       | `connection_string`, `account_name`                                                                                         |
| any     | `credential_chain` | none — resolve from the ambient chain (env / instance profile / `gcloud` / `az login`); optional `endpoint`, `account_name` |


With no `auth` block, the ambient credential chain is used.

> Remote files are read by DuckDB's `read_parquet`/`read_csv`/`read_json`, so the local-file `csv`/`json`/`partition_cols`/`schema` options do **not** apply to object-store tables — DuckDB infers the schema and reader from the URL and `format`.

### Http Backend (`http)` — REST & GraphQL APIs

Turns an HTTP API into SQL tables: you declare each table's request and how to shape its JSON response into rows. Source-level `config` carries `base_url` (**required**), auth, retries, and rate limiting; each `tables:` entry maps one request shape to rows.

```yaml
sources:
  - name: gh
    kind: http
    config:
      base_url: https://api.github.com     # joined with each table's endpoint
      token: ${secret:GITHUB_TOKEN}
    raw_table: true
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

```sql
SELECT number, title FROM gh.pulls
WHERE owner = 'withpawrly' AND repo = 'pawrly' AND state = 'open' LIMIT 20
```

#### Authentication

Set source-level auth with the `config.token` shorthand or a full `auth:` block (the block wins if both are present). The block is tagged by `type` — `header`, `basic`, `custom`, or `oauth2`:

`header` — attach one or more headers (bearer tokens *and* API keys live here). `headers` is a list; each entry gives a `name` plus exactly one of `bearer` (sent as `Bearer <value>`) or `value` (sent verbatim). Multiple entries cover APIs that need several auth headers at once (e.g. Datadog).

```yaml
    config:
      base_url: https://api.example.com
      auth:
        type: header
        headers:
          - { name: Authorization, bearer: "${secret:GITHUB_TOKEN}" }   # → "Bearer …"
          - { name: X-Api-Key,     value: "${secret:API_KEY}" }         # literal
```

`basic` — base64-encodes `username:password` into `Authorization: Basic …`.

```yaml
      auth:
        type: basic
        username: "${secret:API_USER}"
        password: "${secret:API_PASSWORD}"
```

`custom` — credentials carried in the query string (the many `?api_key=…` APIs). `query` is a list of `{ name, value }` appended to every request.

```yaml
      auth:
        type: custom
        query:
          - { name: api_key, value: "${secret:API_KEY}" }
```

`oauth2` — client-credentials grant: a token is fetched on first use, cached, re-fetched before expiry, then sent as `Authorization: Bearer <token>`. Fields: `token_url`, `client_id`, `client_secret`, optional `scope`, `audience`.

```yaml
      auth:
        type: oauth2
        token_url:     https://login.example.com/oauth/token
        client_id:     ${secret:CLIENT_ID}
        client_secret: ${secret:CLIENT_SECRET}
        scope:         read:data       # optional
```

**Shorthand** — `config.token` is the dead-simple single-bearer case, equivalent to a `header` block with one `Authorization: Bearer <token>` entry:

```yaml
    config:
      base_url: https://api.github.com
      token: ${secret:GITHUB_TOKEN}
```

#### Request

Each table's request is built from these flat fields:

- **endpoint** (required) — path appended to `base_url`. May carry a query string and `{param}` placeholders; a param whose name matches a `{placeholder}` fills the URL **path**, the rest become query parameters.
- **method** — defaults to `GET`.
- **headers** — a per-table map of extra request headers.
- **body** — for POST/PUT/GraphQL: `kind` (`json`, the default, sets `Content-Type: application/json`; or `form` for `application/x-www-form-urlencoded`) and `template` (body text with `{param}` placeholders; other braces — JSON/GraphQL syntax — are left untouched).
- **requests** — conditional request shapes tried in order; the first whose `when_filters` are **all** bound replaces the default `endpoint`/`method`/`body`. Each entry is `{ when_filters: [...], endpoint, method?, body? }`. The classic use is a get-by-id endpoint when an id filter is present, falling back to a list endpoint otherwise.

```yaml
      - name: search
        endpoint: /graphql
        method: POST
        params:
          - { name: q, required: true }
        body:
          kind: json
          template: '{"query": "{ search(q: \"{q}\") { id name } }"}'
        response:
          path: $.data.search
          schema:
            - { name: id,   type: varchar }
            - { name: name, type: varchar }
```

#### Query parameters

`params` declares the columns a table accepts as filters. Each is `{ name, type (default varchar), required (default false), default, accepts, emit }`:

- `required: true` — the param must appear as a SQL filter, or the scan fails with a clear error (rather than fetching an unbounded result).
- `default` — value used when the user doesn't filter on it.
- **Equality** pushes down by default: `WHERE state = 'open'` → `?state=open`.
- **Comparisons** — to push `>=` / `<=` etc., list them in `accepts` and map each to a query-parameter name in `emit`:

```yaml
        params:
          - name: created
            accepts: [">=", "<="]
            emit: { ">=": since, "<=": until }   # WHERE created >= X → ?since=X
```

A param can also be surfaced as an output column with `source: param` on a `response.schema` entry (see below).

#### Response

`response` describes how to turn the JSON payload into rows:

- `path` — JSONPath to the array of rows. `$` (the default) means the body *is* the array; `$.data` digs into a wrapper object.
- `schema` — the columns to extract per row, each `{ name, type, source? }`:
  - `type` ∈ `varchar`/`string`/`text`, `bigint`/`int64`, `int`/`int32`, `double`, `float`, `bool`/`boolean`, `date`, `timestamp`, `timestamptz` (ISO-8601 / RFC 3339 strings are parsed), and `json` (a nested object/array kept as raw JSON text).
  - `source` — defaults to the row's top-level field of the same name. Set `$.nested.field` to read a different path, `$` to capture the whole row element (usually into a `json` column), or `param` to inject a request parameter as a column.
- `allow_404_empty` — treat a `404` as an empty result set instead of an error.
- `error` — surface API failures as a clear scan error: `status` (a list of codes or matchers like `">=400"`, `"5xx"`, `"<500"`) and/or `path` (a JSONPath to an error message inside a `200`-with-error body).

```yaml
        response:
          path: $.data
          allow_404_empty: true
          schema:
            - { name: id,       type: bigint }
            - { name: author,   type: varchar, source: $.user.login }
            - { name: payload,  type: json,    source: $ }
            - { name: repo,     type: varchar, source: param }
          error:
            status: [">=400"]
            path: $.message
```

#### Pagination

Set `pagination` to keep fetching pages; absent means a single request. A SQL `LIMIT` stops pagination early once enough rows are collected, and `safety.max_pages` caps the loop. The strategy is tagged by `type`:


| `type`        | Fields                                               | Behaviour                                                                                                                                               |
| ------------- | ---------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `link_header` | —                                                    | Follows the RFC 5988 `Link:` header's `rel="next"` URL until absent.                                                                                    |
| `cursor`      | `next_path`, `param`                                 | Reads an opaque cursor from the body at `next_path` (`$.a.b`) and echoes it back as the `param` query parameter; stops when the cursor is absent/empty. |
| `page`        | `param`, `start` (default 1), `size_param?`, `size?` | Increments the page number in `param` until a page returns zero rows; optionally sends a page size via `size_param`/`size`.                             |
| `offset`      | `param`, `size_param`, `size`                        | Increments `param` by `size` each page until a short page (fewer than `size` rows).                                                                     |


```yaml
        pagination: { type: cursor, next_path: $.response_metadata.next_cursor, param: cursor }
```

#### Rate limiting & retries

`rate_limit` keeps requests within the API's quota:


| Field                 | Notes                                                                                                                                    |
| --------------------- | ---------------------------------------------------------------------------------------------------------------------------------------- |
| `requests_per_second` | Local token-bucket ceiling shared across scans. Omit/zero to disable.                                                                    |
| `remaining_header`    | Response header carrying remaining quota (e.g. `x-ratelimit-remaining`); when it reads `0`, the next request waits until the reset time. |
| `reset_header`        | Response header carrying the reset time as an epoch-seconds timestamp.                                                                   |
| `extra_statuses`      | Status codes besides `429`/`503` also treated as rate-limit signals (e.g. GitHub's secondary-limit `403`).                               |


`retry` governs transient failures (transport errors, 5xx, 429, 503, and any `extra_statuses`): `max_retries` (default 3), `base_backoff_ms` (default 200, doubles each attempt), `max_backoff_ms` (default 5000). Backoff honours a `Retry-After` header when present.

```yaml
    config:
      base_url: https://api.github.com
      rate_limit:
        remaining_header: x-ratelimit-remaining
        reset_header:     x-ratelimit-reset
        extra_statuses:   [403]            # GitHub secondary limit
      retry:
        max_retries: 5
```

A runnable cache-over-API walkthrough lives at [examples/cache-http/](../examples/cache-http/pawrly.yaml).

### `sqlite` — local SQLite databases

Attaches a SQLite file read-only and exposes its tables; equality filters push down. When `tables:` is omitted, every user table is auto-registered.

```yaml
sources:
  - name: app
    kind: sqlite
    config:
      path: ./app.db
    # tables:                       # optional: restrict / reshape
    #   - name: active_users
    #     query: SELECT id, email FROM users WHERE active = 1
```

### `postgres`, `mysql` — foreign databases

DuckDB `ATTACH`es the database **read-only** and exposes its tables lazily (`<source>.<table>`); equality predicates, projection, and limits push down. No `tables:` needed.


| Key                        | Notes                                           |
| -------------------------- | ----------------------------------------------- |
| `dsn`                      | Full connection string. If present, used as-is. |
| `host`                     | Required if no `dsn`.                           |
| `database` / `dbname`      | Database name.                                  |
| `port`, `user`, `password` | Optional.                                       |


```yaml
sources:
  - name: oltp
    kind: postgres            # aliases: pg, postgresql
    config:
      host: db.internal
      database: app
      user: readonly
      password: ${secret:PG_PASSWORD}
```

### `duckdb` — local DuckDB database file

Attaches a `.duckdb` database file read-only and exposes its tables lazily.

```yaml
sources:
  - name: local_db
    kind: duckdb
    config:
      path: ./analytics.duckdb
```

### `snowflake`

DuckDB `ATTACH` via the Snowflake community extension (installed on first use). Requires `account`, `user`, `password`; optional `database`, `schema`, `warehouse`, `role`.

```yaml
sources:
  - name: warehouse
    kind: snowflake
    config:
      account: acme.us-east-1
      user: ${secret:SNOWFLAKE_USER}
      password: ${secret:SNOWFLAKE_PASSWORD}
      database: ANALYTICS
      schema: PUBLIC
```

### `iceberg`, `delta` — table formats

Each declared table maps to a DuckDB scan function over a table location. `tables:` is required, each with a `path` (or `location`).

```yaml
sources:
  - name: lake
    kind: iceberg                 # or: delta (alias deltalake)
    tables:
      - name: orders
        path: s3://bucket/warehouse/orders
```

For tables on an object store, provide credentials with a `storage:` block (same keys as object-store `file`); the `httpfs` extension loads automatically.

### `ducklake` — DuckLake lakehouse catalog

Attaches a [DuckLake](https://ducklake.select) catalog (a metadata database plus a data path) and exposes its tables lazily.

```yaml
sources:
  - name: lake
    kind: ducklake
    config:
      catalog: ./metadata.ducklake     # sqlite/duckdb/postgres catalog
      data_path: ./lake_data            # local or s3://… (+ optional storage block)
```

---

## Federation

Every source is a table in one DataFusion plan, so you can join across kinds in a single statement — a local Parquet file against a Postgres table against an HTTP query — with no import step:

```sql
SELECT u.email, COUNT(p.number) AS open_prs
FROM oltp.users u
JOIN gh.pulls p ON p.user = u.github_login
WHERE p.owner = 'pawrly' AND p.repo = 'pawrly' AND p.state = 'open'
GROUP BY u.email
```

See `examples/pawrly.yaml` for a kitchen-sink configuration covering every kind.