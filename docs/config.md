# Configuration

A workspace is described by a single YAML file, `pawrly.yaml`. The directory holding that file *is* the workspace: relative paths in the config (`include:`, `from:`, file-source `path`s) resolve against it.

Pawrly discovers the manifest in this order (first hit wins, nothing merges):

1. `--config <path>`
2. the `PAWRLY_CONFIG` environment variable
3. `./pawrly.yaml` in the current directory
4. `$PAWRLY_HOME/pawrly.yaml` — the **default workspace**

`PAWRLY_HOME` (or `--home`) is Pawrly's data directory, `~/.pawrly` by default. Besides the default-workspace manifest it holds the cache root (`cache/`, see [Storage location](#storage-location)) and the daemon socket (`sockets/pawrly.sock`). `pawrly serve` follows the same discovery, so a daemon started with no `--config` serves the default workspace.

A JSON Schema for the file ships in `schemas/pawrly.schema.json`. Wire it into your editor for completion and inline validation:

```yaml
# yaml-language-server: $schema=./schemas/pawrly.schema.json
```

Run `pawrly validate` to check a config without executing anything.

## Top-level shape

```yaml
version: 1                 # required; only `1` is supported
name: my-workspace         # optional label

defaults:                  # optional; workspace-wide defaults
  # ...

secrets:                   # optional; where secret references resolve from
  - kind: env
  - kind: file
    path: ~/.pawrly/secrets.yaml
  - kind: keyring
    service: pawrly

include:                   # optional; splice sources from other files (see "Multi-file configs")
  - ./sources/*.yaml

sources:                   # the data sources
  - <source>

semantic:                  # optional; business models over the sources
  models:
    - <model>              # see docs/semantic.md

observability:             # optional; logging, OpenTelemetry export, activity log
  # ...                    # see "Observability"
```

Only `version` and `sources` are needed for a useful config; everything else is optional.

## Sources

Each entry under `sources:` declares one source. The common shape:

```yaml
sources:
  - name: data                 # required; used as the schema prefix in SQL (data.orders)
    kind: file                 # required; see docs/sources.md for kinds
    description: Local fixtures # optional
    config:                    # kind-specific settings
      path: ./data/*.csv
    tables:                    # optional; explicit per-table definitions
      - name: orders
        path: ./data/orders.csv
        format: csv
    cache: <cache>             # optional; per-source caching (see below)
    safety: <safety>           # optional; per-source guard rails (see below)
```

A source's `name` becomes the schema in SQL: a table `orders` under source `data` is queried as `data.orders`. Per-kind options live under `config:` (or per-table fields); see **[Sources](./sources.md)** for each kind.

## Secrets

Reference secrets in any string with `${secret:NAME}`; they resolve at load time from the backends listed under `secrets:`, tried in order (first hit wins):

```yaml
secrets:
  - kind: env                              # environment variables
  - kind: file                             # a YAML or dotenv (.env) file
    path: ~/.pawrly/secrets.yaml
    format: auto                           # auto (by extension) | yaml | dotenv
  - kind: keyring                          # the OS keychain
    service: pawrly

sources:
  - name: gh
    kind: http
    config:
      base_url: https://api.github.com
      token: ${secret:GITHUB_TOKEN}
```

Backends:

- **`env`** — process environment variables.
- **`file`** — a file of `KEY: value` (YAML) or `KEY=value` (dotenv) pairs. `format` defaults to `auto`, which picks dotenv for a `.env` extension/name and YAML otherwise. A relative `path` resolves against the config file's directory. The file must not be world-readable (mode `0600` on Unix) or it is rejected.
- **`keyring`** — the OS keychain under `service` (default `pawrly`).
- **`auto`** — convenience chain: `env`, then `keyring`, then a `.env` file in the config directory **if present**. A missing or insecure `.env` is skipped with a warning, never fatal.

When `secrets:` is **omitted entirely**, the chain defaults to a single `auto` backend — so a `.env` beside your config is picked up automatically. Pawrly does not otherwise load `.env` into the process environment; it is read only through the `file`/`auto` backends.

Two more interpolations work in any config string: `${env:NAME}` splices a plain environment variable (independent of the chain), and `${file:PATH}` inlines a file's trimmed contents (`~` expands to `$HOME`).

For a *declared, typed, scoped* layer on top of these — including OAuth-minted secrets — see **[Variables](./variables.md)** (`${var:NAME}`).

## Caching

Caching saves rows returned by a source scan as a Parquet snapshot. On a cache miss, Pawrly reads the source, returns the rows, and writes the snapshot. Later queries read the snapshot until its cache policy requires another source read.

The table name and query syntax do not change when caching is enabled. Caching is opt-in per table or source; without a `cache:` block, every query reads the source.

```yaml
cache:
  mode: ttl        # none | ttl | refresh | cron
  ttl: 1h          # for mode: ttl
```

Choose when Pawrly replaces the snapshot:

| Mode | Behaviour |
|---|---|
| `none` | No caching (the default when `cache:` is absent). |
| `ttl` | Use the snapshot until `ttl` elapses, then read the source again on the next query. |
| `refresh` | Create the snapshot on first use, then replace it in the background every `every` interval. |
| `cron` | Create the snapshot on first use, then replace it in the background on a cron schedule. |

### Storage location

The cache writes Parquet plus a JSON manifest under `defaults.cache.storage`. When unset, the root derives from the Pawrly home as `$PAWRLY_HOME/cache` (default `~/.pawrly/cache`); it is **anchored at the home, not the current directory or the workspace**, regardless of where you run `pawrly` from. An explicit value may use a leading `~`, which expands to `$HOME`.

```yaml
defaults:
  cache:
    storage: ~/.pawrly/cache   # optional; default $PAWRLY_HOME/cache
    namespace: my-project      # optional; isolates this workspace (see below)
```

Because `storage` is shared across every workspace on the machine, Pawrly inserts a **namespace** segment so two workspaces that each define, say, a `data.orders` table don't collide on the same path:

```
<storage>/<namespace>/data/<source>/<table>/part-000000.parquet
```

- **Default (automatic).** With `namespace` unset, it's derived as `<dirname>-<hash>` from the workspace's canonical absolute path. The same workspace always maps to the same directory; distinct workspaces never collide. Moving or renaming the workspace directory changes the id, so its cache starts cold. The **default workspace** (the manifest at `$PAWRLY_HOME/pawrly.yaml`) gets the literal namespace `default` instead.
- **Override.** Set `defaults.cache.namespace` to pin a stable namespace (e.g. so the cache survives moving the directory) or to deliberately **share** one cache across workspaces by giving them the same value. A blank value falls back to the derived id.

The cache is restart-safe and concurrency-safe. Inspect and manage it with the [`pawrly cache`](./cli.md#pawrly-cache) commands.

### Direct snapshot reads

There are two ways to read a cached source table:

- `source.table` uses the source normally. Pawrly checks its cache policy, reads a valid snapshot when available, and fetches the source when required.
- `<namespace>.source.table` reads the Parquet snapshot stored in that namespace. It does not contact the source or check whether the snapshot has expired.

```sql
SELECT * FROM github.issues;              -- use the source and its cache policy
SELECT * FROM my-project.github.issues;   -- read the stored snapshot directly
```

A direct read returns exactly what is on disk, even when the snapshot is past its TTL. It fails if the namespace does not contain a snapshot for that table.

## Safety

A `safety:` block sets guard rails that are enforced before a scan runs:

```yaml
safety:
  require_filters_on: [order_date]   # error unless a filter touches these columns
  require_at_least_one_filter: true  # refuse a full-table scan
  max_rows: 1000000                  # cap returned rows
  max_pages: 50                      # cap HTTP pagination
  timeout: 30s                       # per-query timeout
  required_predicates:               # predicates AND-ed into every scan (see semantic RLS)
    - "tenant_id = ${param:tenant_id}"
```

`required_predicates` is most useful with the [semantic layer](./semantic.md), where `${param:NAME}` placeholders are bound from a query's params as safe literals for row-level security.

## Multi-file configs

As a workspace grows, configuration can move out of the main `pawrly.yaml`. Choose the mechanism based on what the other file contains:

- Use top-level `include:` for files that add sources, secrets, or a source together with its semantic models.
- Use `from:` when the main file declares a source's name and kind but stores the rest of that source elsewhere.
- Use `semantic.include:` for files that contain only semantic models.

All paths are relative to the file that declares them. Pawrly loads every referenced file, merges them into one config, and then validates the result.

- **include:** adds one or more files to the top-level config. Globs are allowed and sorted lexicographically. An included file can have either shape:
  - A **fragment** looks like part of `pawrly.yaml`, with a `sources:` list and an optional `secrets:` list.
  - A **single-source file** places `name`, `kind`, and `config` at the top level without a `sources:` wrapper. This works well with one-source-per-file globs.

  ```yaml
  include:
    - ./sources/*.yaml       # may hold fragments or single-source files
    - ./team-sources.yaml
  ```

  Either form may also carry a top-level `models:` list (the semantic models defined over its sources), which is spliced into `semantic.models`. This lets one file fully describe an integration (its source *and* its models). Models still merge into the one global semantic layer, so a co-located model may relate to a model declared in another file, and duplicate model names across files are rejected with both filenames.

  ```yaml
  # sources/github.yaml — a bare single source plus the models over it
  name: gh
  kind: http
  config:
    base_url: https://api.github.com
    token: ${secret:GITHUB_TOKEN}
  raw_table: true
  models:
    - name: gh_issues
      source: gh.issues
      dimensions: [{ name: state, expr: state, type: string }]
      measures:  [{ name: issue_count, agg: count, expr: id }]
  ```

  (`include:`d `models:` is the file-level co-location convenience; for model-only files that aren't tied to one source, use `semantic.include:` below.)

- **from:** loads part of one source from another file. The main config owns `name` and `kind`; the referenced file contains fields such as `config`, `cache`, and `safety`. Fields beside `from:` in the main config override fields from the referenced file:

  ```yaml
  sources:
    - name: warehouse
      kind: snowflake
      from: ./sources/warehouse.yaml
  ```

- **semantic.include:** — splices the *models* of other files into `semantic.models`. Each referenced file contains **only** models — either a top-level `models:` list or a bare sequence of model mappings — never sources, secrets, or other config. Globs are allowed and sorted lexicographically; inline `models:` come first, then the included files. Duplicate model names (within or across files) are rejected with both filenames.

  ```yaml
  semantic:
    include:
      - ./models/*.yaml      # each file holds only models
    models:
      - name: inline_model   # optional inline models still allowed
        # ...
  ```

  ```yaml
  # models/orders.yaml — a model-only file
  models:
    - name: orders
      source: data.orders
      dimensions: [{ name: status, expr: status, type: string }]
      measures:  [{ name: revenue, agg: sum, expr: total_amount }]
  ```

Includes can chain; `from:` is not transitive. Cycles are detected and reported. Model `source:` references and relationships are validated against the **merged** config, so a model in one file may reference a source declared in another. `pawrly config show --tree` prints the assembled tree, and `pawrly source list` annotates each source with the file it came from. A runnable layout lives under `examples/multi-file/`.

## Observability

`observability:` is optional. Absent, Pawrly logs to stderr as before and exports nothing. The block has three parts; CLI flags (`--log-level`, `--log-format`, `--otel-endpoint`, `--otel-protocol`, `--prometheus-listen`) override the matching settings.

```yaml
observability:
  tracing:
    level: info            # EnvFilter directive; RUST_LOG still wins
    format: text           # text | json
  otel:
    enabled: false         # master switch for OTLP export
    endpoint: http://localhost:4317
    protocol: grpc         # grpc | http
    service_name: pawrly
    traces: true
    metrics: true
    logs: true             # bridge tracing events to OTel logs
    sample_ratio: 1.0      # parent-based ratio sampler
    prometheus:
      enabled: false       # serve a /metrics pull endpoint (independent of OTLP push)
      listen: 127.0.0.1:9090
  activity:
    enabled: false         # master switch for the activity log
    sinks: [tracing]       # any of: tracing, table
    redact_sql: false      # false | literals | true
    ring_capacity: 10000   # in-memory rows kept for the `table` sink
    store: ~/.pawrly/activity  # persist to Parquet; omit for in-memory only
    partition_hours: 4         # hr= partition width
    flush_threshold: 1000      # records buffered before a file is written
    flush_interval: 60s        # or this, whichever first
    retention: 30d             # prune files older than this; omit to keep all
```

- **Traces & logs** are emitted as `tracing` spans/events and, when `otel.enabled`, exported over OTLP. W3C `traceparent` is propagated across the gRPC and MCP boundaries, so a CLI→daemon request is a single trace.
- **Metrics** (query/cache/source counters and histograms) export over OTLP push when `otel.metrics` is on, and/or a Prometheus pull endpoint when `otel.prometheus.enabled` is on.
- **Activity log** records one row per operation. The `tracing` sink emits a structured event; the `table` sink exposes the recent rows as the `system.activity` SQL table:

  ```sql
  SELECT interface, status, count(*), avg(duration_ms)
  FROM system.activity
  WHERE at > now() - INTERVAL '1 hour'
  GROUP BY 1, 2;
  ```

  `redact_sql` controls SQL capture: `false` stores it verbatim, `literals` replaces literal values with `$REDACTED` (keeping shape), and `true` stores only the statement kind and tables. Parameter values are never stored.

  Without `store`, `system.activity` is an in-memory ring of the most recent `ring_capacity` rows, lost on restart. Set `store` to persist records as date/hour-partitioned Parquet (`dt=YYYY-MM-DD/hr=HH/…`); the table then unions the on-disk history with the not-yet-flushed buffer, so it survives restarts. `retention` prunes old files. A runnable config lives at `examples/observability.yaml`.

## Defaults

`defaults:` contains workspace settings and fallback values. It has six sections:

- `cache`: storage location, namespace, and the cache policy used when a source or table does not declare one.
- `http`: default request timeout and user agent.
- `safety`: the default maximum row count for an unfiltered source scan.
- `engine`: query timeout, memory limit, concurrency, and DuckDB pool size.
- `optimizer`: query optimizer switches.
- `materialize`: whether inline materialization directives are allowed.

Source and table settings override the corresponding `cache`, `http`, or `safety` defaults. The `engine`, `optimizer`, and `materialize` sections apply to the workspace.

```yaml
defaults:
  cache:
    mode: { mode: ttl, ttl: 1h }
  http:
    timeout: 30s
    user_agent: pawrly/0.1.0
  safety:
    max_unfiltered_rows: 1000000
  engine:
    query_timeout: 5m
    max_concurrent_queries: 16
    duckdb_pool_size: 8
```

See `examples/pawrly.yaml` for the full set of options in context.
