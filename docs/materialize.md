# Materialized tables

`materialize` persists data as a **named, self-backed table** you can query later like any other table. Run a query (or point at a file or URL), get back a Parquet artifact addressable as `materialized.<name>`. Unlike the per-table [cache](./config.md#caching) — which transparently mirrors a live source and expires — a materialized table has no upstream: it is **pinned** (never auto-evicted) and changes only when you re-materialize, refresh, or drop it.

Use it from the [`pawrly materialize`](./cli.md#pawrly-materialize) CLI, the [`materialize` MCP tool](./mcp.md), or the library/gRPC API.

## Three origins

Every materialized table is produced from one of three origins:

```bash
# 1. A SQL query — persist its result
pawrly materialize top_customers \
  "SELECT customer, SUM(amount) AS total FROM stripe.charges GROUP BY 1 ORDER BY 2 DESC LIMIT 10"

# 2. A local file — CSV, Parquet, or JSON
pawrly materialize sales --file ./data/sales.csv

# 3. A remote http(s) file (read via DuckDB httpfs)
pawrly materialize prices --url https://example.com/prices.parquet
```

For `--file` and `--url` the format is inferred from the extension; pass `--format parquet|csv|json` to override or when the extension is missing. A query origin can carry parameters with `--param KEY=VALUE`, which substitute `${param:KEY}` in the SQL.

## Querying it

A materialized table lives under the reserved `materialized` schema and is addressable through the workspace namespace:

```bash
pawrly sql "SELECT * FROM materialized.top_customers ORDER BY total DESC"
```

The unqualified `materialized.<name>` form resolves within the workspace. The fully-qualified `<namespace>.materialized.<name>` form is also available — see [Direct cache reads](#direct-cache-reads) below. Materialized tables show up in `pawrly cache list` alongside cached entries, with mode `pinned`.

## Create-or-replace, refresh, and drop

Materializing a name that already exists **replaces** it:

```bash
pawrly materialize sales --file ./data/sales-2024.csv   # 12 rows
pawrly materialize sales --file ./data/sales-2025.csv   # replaced
```

Because the origin is stored with the table, you can **refresh** it — re-run the query or re-read the file/URL and overwrite:

```bash
pawrly cache refresh materialized.sales
```

**Drop** removes the table and its file:

```bash
pawrly materialize sales --drop
```

## Custom namespaces

By default a materialized table lands in the workspace namespace. Pass `--namespace` to target a different one — each namespace is a fully isolated store (its own storage directory, manifest, and SQL address), so the same table name never collides across namespaces:

```bash
pawrly materialize top_customers "SELECT …" --namespace sess_a
pawrly materialize top_customers "SELECT …" --namespace sess_b   # no clobber

pawrly sql "SELECT * FROM sess_a.materialized.top_customers"
pawrly cache list --namespace sess_a
pawrly cache refresh materialized.top_customers --namespace sess_a
pawrly materialize top_customers --drop --namespace sess_a
```

A namespace is created on first write and resolves in SQL from then on — including after a daemon restart, and when written by another process sharing the storage root. Namespace names use alphanumerics, `_`, `-`, and `.`; `pawrly`, `materialized`, `system`, and `information_schema` are reserved.

The same knob rides on every surface: `--namespace` on the CLI, a `namespace` field on the `Materialize` `DropMaterialized`/`ListEntries` RPCs, a `?namespace=` query parameter on the REST endpoints, and a `namespace` argument on the MCP tools. Omitted or empty always means the default workspace namespace. This is one shared engine's organizational boundary, not a security boundary — any caller holding the server's bearer token can address any namespace.

## Inline directive

When `defaults.materialize.allow_inline` is enabled, a leading
`-- pawrly: materialize <name>` comment on an ordinary query persists the result
*and* returns its rows — no second call:

```sql
-- pawrly: materialize big_orders
SELECT * FROM stripe.charges WHERE amount > 1000
```

The directive is recognized only in the leading comment block (before the first
non-comment token), so it never fires from a comment inside a query. It is off by
default — a `SELECT` that writes to disk is a footgun on a shared daemon — so
enable it deliberately per workspace:

```yaml
defaults:
  materialize:
    allow_inline: true
```

From the CLI, pipe the statement via stdin so the leading `--` isn't read as a
flag: `… | pawrly sql -`.

## Pinning

A materialized table is never reclaimed by TTL expiry or `pawrly cache vacuum` — it has no source to refetch from, so it stays until you drop it. This is the key difference from a cached source table, which is a disposable copy of live data.

The schema name `materialized` is reserved: a data source may not be named `materialized` (the config validator rejects it).

## Direct cache reads

Materialization is one half of a read-only **namespace catalog** that makes on-disk data SQL-addressable. The other half exposes your cached source snapshots directly, bypassing the live read-through wrapper:

```sql
SELECT * FROM github.issues;            -- live: cached-or-fetched via the source
SELECT * FROM <namespace>.github.issues; -- the cached snapshot on disk, read directly
SELECT * FROM <namespace>.materialized.sales;
```

Direct reads are **expiry-agnostic**: they return exactly what is on disk, ignoring freshness (only live reads honor TTL). The `<namespace>` segment defaults to a per-workspace id; set `defaults.cache.namespace` in [`pawrly.yaml`](./config.md#caching) for a clean, stable name like `untwine.materialized.sales`.

## With agents (MCP)

The [MCP server](./mcp.md) exposes `materialize` and `drop_materialized` tools, so an agent can persist a result it computed and query it back later:

```json
{ "name": "materialize", "arguments": { "name": "cohort", "sql": "SELECT …" } }
```

## Example

A runnable example lives in `examples/materialize/` — a config plus a CSV, with try-it commands for the query, file, refresh, and drop flows.
