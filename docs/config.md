# Configuration

A workspace is described by a single YAML file, `pawrly.yaml`. By default Pawrly looks for it in the current directory (override with `--config <path>` or the `PAWRLY_CONFIG` environment variable).

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
    kind: github
    config:
      token: ${secret:GITHUB_TOKEN}
```

Backends:

- **`env`** — process environment variables.
- **`file`** — a file of `KEY: value` (YAML) or `KEY=value` (dotenv) pairs. `format` defaults to `auto`, which picks dotenv for a `.env` extension/name and YAML otherwise. A relative `path` resolves against the config file's directory. The file must not be world-readable (mode `0600` on Unix) or it is rejected.
- **`keyring`** — the OS keychain under `service` (default `pawrly`).
- **`auto`** — convenience chain: `env`, then `keyring`, then a `.env` file in the config directory **if present**. A missing or insecure `.env` is skipped with a warning, never fatal.

When `secrets:` is **omitted entirely**, the chain defaults to a single `auto` backend — so a `.env` beside your config is picked up automatically. Pawrly does not otherwise load `.env` into the process environment; it is read only through the `file`/`auto` backends.

Two more interpolations work in any config string: `${env:NAME}` splices a plain environment variable (independent of the chain), and `${file:PATH}` inlines a file's trimmed contents (`~` expands to `$HOME`).

## Caching

Caching is **opt-in per table** (or per source). Add a `cache:` block; with no block, reads always go live.

```yaml
cache:
  mode: ttl        # none | ttl | refresh | cron
  ttl: 1h          # for mode: ttl
```

Modes:

| Mode | Behaviour |
|---|---|
| `none` | No caching (the default when `cache:` is absent). |
| `ttl` | Cache the result; serve it until `ttl` elapses, then re-fetch on the next read. |
| `refresh` | Keep the cache warm with a background loop on a fixed interval (`every: 1h`). |
| `cron` | Like `refresh`, but the schedule is a cron expression (`cron: "0 * * * *"`). |

### Storage location

The cache writes Parquet plus a JSON manifest under `defaults.cache.storage` (default `~/.pawrly/cache`). `~` expands to `$HOME`, so cached data lives under your home directory — **anchored at `$HOME`, not the current directory or the workspace**, regardless of where you run `pawrly` from.

```yaml
defaults:
  cache:
    storage: ~/.pawrly/cache   # cache root (default)
    namespace: my-project      # optional; isolates this workspace (see below)
```

Because `storage` is shared across every workspace on the machine, Pawrly inserts a **namespace** segment so two workspaces that each define, say, a `data.orders` table don't collide on the same path:

```
<storage>/<namespace>/data/<source>/<table>/part-000000.parquet
```

- **Default (automatic).** With `namespace` unset, it's derived as `<dirname>-<hash>` from the workspace's canonical absolute path. The same workspace always maps to the same directory; distinct workspaces never collide. Moving or renaming the workspace directory changes the id, so its cache starts cold.
- **Override.** Set `defaults.cache.namespace` to pin a stable namespace (e.g. so the cache survives moving the directory) or to deliberately **share** one cache across workspaces by giving them the same value. A blank value falls back to the derived id.

The cache is restart-safe and concurrency-safe. Inspect and manage it with the [`pawrly cache`](./cli.md#pawrly-cache) commands.

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

As a workspace grows, split `pawrly.yaml` so sources can live in their own files. Both primitives resolve paths relative to the **declaring file** and are assembled before validation, so the rest of the pipeline sees one merged tree.

- **`include:`** (top-level) — splices the `sources:` (and optional `secrets:`) of other files into this one. Globs are allowed and sorted lexicographically.

  ```yaml
  include:
    - ./sources/*.yaml
    - ./team-sources.yaml
  ```

- **`from:`** (on a source) — loads one source's body from a sibling file:

  ```yaml
  sources:
    - name: warehouse
      from: ./sources/warehouse.yaml
  ```

- **`semantic.include:`** — splices the *models* of other files into `semantic.models`. Each referenced file contains **only** models — either a top-level `models:` list or a bare sequence of model mappings — never sources, secrets, or other config. Globs are allowed and sorted lexicographically; inline `models:` come first, then the included files. Duplicate model names (within or across files) are rejected with both filenames.

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

## Defaults

`defaults:` sets workspace-wide values inherited by sources that don't override them — for example HTTP client settings and baseline safety caps. See the worked configurations in `examples/pawrly.yaml` for the full set of options in context.
