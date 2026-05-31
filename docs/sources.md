# Sources

A **source** is a named set of tables backed by some external system or local files. Every source is exposed to the query engine as one or more tables, addressed in SQL as `<source>.<table>`. Sources are declared under `sources:` in [`pawrly.yaml`](./config.md).

This page covers each source kind. The common shape — `name`, `kind`, `config`, `tables`, `cache`, `safety` — is described in [Configuration](./config.md).

## Available today

### `file` — Parquet, CSV, JSON

Serves columnar and row files from disk. Point it at a glob, or define tables explicitly.

```yaml
sources:
  - name: data
    kind: file
    config:
      path: ./data/*.csv          # glob; table names derive from file names
```

Or per-table, with explicit formats and paths:

```yaml
sources:
  - name: data
    kind: file
    tables:
      - name: orders
        path: ./data/orders.parquet
        format: parquet           # parquet | csv | json
      - name: events
        path: ./data/events.json
        format: json
```

Once registered, query them like any table: `SELECT * FROM data.orders`. Files of different formats can be joined freely — the SQL is identical regardless of format.

A per-table `path` may be a single file, a **glob**, or a **directory** — a glob or directory is read as one table unioning every matching file (the natural shape for a partitioned dataset):

```yaml
    tables:
      - name: orders
        path: ./data/orders/*.parquet     # all parts → one `orders` table
        format: parquet
```

**Hive partitions** — for `key=value` directory layouts, declare `partition_cols` to expose the keys as queryable, prunable columns:

```yaml
      - name: events                        # events/dt=2026-05-31/region=us/*.parquet
        path: ./lake/events
        format: parquet
        partition_cols:
          - { name: dt,     type: date }
          - { name: region, type: varchar }
```

```sql
SELECT * FROM data.events WHERE dt = '2026-05-31'   -- only that dt= directory is read
```

**CSV options & explicit schema** — override the dialect, and (optionally) the inferred schema for headerless or mis-inferred files:

```yaml
      - name: metrics
        path: ./data/metrics.tsv
        format: csv
        csv:
          header: false          # default true
          delimiter: "\t"        # default ","
          quote: '"'
        schema:                  # optional: name + type the columns
          - { name: host,  type: varchar }
          - { name: value, type: bigint }
```

### `sqlite` — local SQLite databases

Attaches a SQLite database file read-only and exposes its tables. Equality filters are pushed down into SQLite.

```yaml
sources:
  - name: app
    kind: sqlite
    config:
      path: ./app.db
```

### HTTP — REST APIs

Turns a REST API into SQL tables. Pawrly ships **bundled specs** for common services and also supports generic, user-defined HTTP tables.

Bundled `github` (currently exposes a `pulls` table):

```yaml
sources:
  - name: gh
    kind: github
    config:
      token: ${secret:GITHUB_TOKEN}
```

```sql
SELECT number, title, state
FROM gh.pulls
WHERE owner = 'withpawrly' AND repo = 'pawrly' AND state = 'open'
LIMIT 20
```

Some columns are **required filters** (above, `owner` and `repo`) — Pawrly returns a clear error if they're missing, rather than scanning an entire API. Supported authentication modes are `bearer`, `api_key`, `basic`, and `oauth2` (client-credentials):

```yaml
    config:
      auth:
        type: oauth2
        token_url:     https://login.example.com/oauth/token
        client_id:     ${secret:CLIENT_ID}
        client_secret: ${secret:CLIENT_SECRET}
        scope:         read:data        # optional
        audience:      https://api.example.com   # optional
```

The access token is fetched on first use, cached, and re-fetched on expiry, then sent as `Authorization: Bearer <token>`.

**Generic HTTP** — point `kind: http` at any REST endpoint and declare your own
tables. Each table gives an `endpoint` and a `response` describing how to turn
the JSON into rows:

```yaml
sources:
  - name: cats
    kind: http
    config:
      base_url: https://catfact.ninja
    tables:
      - name: facts
        endpoint: /facts?limit=50        # relative to base_url
        response:
          path: $.data                   # JSONPath to the row array ($ = body is the array)
          schema:
            - { name: fact,   type: varchar }
            - { name: length, type: bigint }
```

```sql
SELECT length, fact FROM cats.facts ORDER BY length DESC LIMIT 5
```

Field reference:

- `endpoint` — path appended to `base_url`; may carry a query string and `{param}` placeholders.
- `method` — defaults to `GET`.
- `body` — request body for POST/PUT/GraphQL endpoints: `kind` (`json` — the default, sets `Content-Type: application/json` — or `form`) and `template` (body text with `{param}` placeholders filled from bound params/filters; other braces, e.g. JSON/GraphQL, are left untouched).
- `requests` — conditional request shapes for one table, tried in order; the first whose `when_filters` are all bound replaces the default `endpoint`/`method`/`body` (e.g. a get-by-id endpoint when `number` is filtered, list otherwise). Each entry has `when_filters`, `endpoint`, optional `method`, and optional `body`.
- `response.path` — JSONPath to the array of rows. `$` means the body *is* the array; `$.data` digs into a wrapper object.
- `response.schema` — columns to extract per row. `type` ∈ `varchar`, `bigint`, `int`, `double`, `float`, `bool`, `date`, `timestamp`, `timestamptz` (ISO-8601 / RFC 3339 strings are parsed), and `json` (a nested object/array kept as raw JSON text). Add `source: $.nested.field` to read a different path, `source: $` to capture the whole row element (typically into a `json` column), or `source: param` to inject a request parameter as a column.
- `response.allow_404_empty` — treat a `404` as an empty result set instead of an error.
- `response.error` — turn API failures into a clear scan error: `status` (codes or matchers like `">=400"`, `"5xx"`) and/or `path` (a JSONPath to an error message inside a `200`-with-error body).
- `params` — declared query/path parameters (`name`, `type`, `required`, `default`); a `required` param must appear as a SQL filter. Equality pushes down by default; add `accepts` (e.g. `[">=", "<="]`) plus an `emit` map (operator → query parameter, e.g. `{ ">=": since, "<=": until }`) to push comparison filters down as separate query parameters.

A `LIMIT` stops pagination early — once enough rows are collected, no further pages are fetched.

The source-level `rate_limit` block can track the API's own quota headers: `remaining_header` / `reset_header` (when remaining hits `0`, the next request waits until the reset time) and `extra_statuses` (codes besides `429`/`503` — e.g. GitHub's secondary-limit `403` — that are also treated as rate-limit signals and retried).

A runnable end-to-end example **with caching** lives at [`examples/cache-http/`](../examples/cache-http/pawrly.yaml).

There's also a raw escape hatch (`raw_table: true`) for endpoints without a typed spec, where you provide the request path as a filter and Pawrly hands back the JSON response as rows.

### `ai` — OpenAI-compatible models

Registers an AI provider so you can call a model from SQL. It exposes a `chat` function and a `models` table:

```yaml
sources:
  - name: ai
    kind: ai
    config:
      provider: openai
      base_url: https://api.openai.com/v1
      api_key: ${secret:OPENAI_API_KEY}
      default_model: gpt-5-mini
```

```sql
SELECT id,
       ai.chat('gpt-5-mini', 'Summarize in one line: ' || body) AS summary
FROM data.tickets
LIMIT 5

SELECT * FROM ai.models      -- name, model, provider
```

Any OpenAI-compatible endpoint works via `base_url`.

## Caching any source

Any source or table can opt into caching with a `cache:` block — useful for rate-limited APIs and expensive AI calls. See [Configuration → Caching](./config.md#caching).

```yaml
sources:
  - name: gh
    kind: github
    config:
      token: ${secret:GITHUB_TOKEN}
    cache:
      mode: ttl
      ttl: 10m
```

See [`examples/cache-http/`](../examples/cache-http/pawrly.yaml) for a runnable cache-over-a-public-API walkthrough.

## Planned source kinds

The following kinds are recognized by the config and on the roadmap; today they return a clear "not available in this build" error so your config stays forward-compatible:

- **Relational databases** — Postgres, MySQL, Excel.
- **Warehouses & lakehouses** — Snowflake, Iceberg, Delta.
- **Object stores** — S3, GCS, Azure.

Declaring one of these validates fine; querying it tells you the kind isn't available yet. Check `examples/pawrly.yaml` for the full kitchen-sink configuration covering every kind.

## Federation

Because every source is a table in one DataFusion plan, you can join across kinds in a single statement — a local file against a SQLite table against a GitHub query — with no import step. The SQL is the same whether the data is local or remote.
