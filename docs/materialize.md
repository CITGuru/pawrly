# Materialized tables

Materializing data saves the result of a query, local file, or remote file as a named SQL table. Pawrly writes the rows to Parquet and makes them available as `materialized.<name>`.

Reads come from the saved Parquet file, not from the original query, file, or URL. Its data changes only when you replace, refresh, or drop it.

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

A materialized table lives under the reserved `materialized` schema:

```bash
pawrly sql "SELECT * FROM materialized.top_customers ORDER BY total DESC"
```

`materialized.<name>` reads from the current workspace's storage namespace. Use `<namespace>.materialized.<name>` to read from another namespace; [Custom namespaces](#custom-namespaces) explains when that is useful.

List stored tables with `pawrly cache list`. Materialized entries appear with mode `pinned`.

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

Pawrly separates on-disk data into storage namespaces. Every workspace has a default namespace for materialized tables. A materialize command without `--namespace` writes there.

Pass `--namespace` to write in a custom namespace. Each namespace has its own tables and SQL pointer, so the same table name can exist in more than one namespace:

```bash
pawrly materialize top_customers "SELECT …" --namespace sess_a
pawrly materialize top_customers "SELECT …" --namespace sess_b   # no clobber

pawrly sql "SELECT * FROM sess_a.materialized.top_customers"
pawrly cache list --namespace sess_a
pawrly cache refresh materialized.top_customers --namespace sess_a
pawrly materialize top_customers --drop --namespace sess_a
pawrly cache drop-namespace sess_a       # or tear down the whole namespace at once
```

A namespace is created on its first write and remains available after a daemon restart. Namespace names may contain letters, numbers, `_`, `-`, and `.`; `pawrly`, `materialized`, `system`, and `information_schema` are reserved.

The same option is available as `--namespace` on the CLI, a `namespace` field on the `Materialize`, `DropMaterialized`, and `ListEntries` RPCs, a `?namespace=` REST query parameter, and a `namespace` argument on the MCP tools. An omitted or empty value means the current workspace's default namespace.

Namespaces organize stored data; they are not a security boundary. A caller with the server's bearer token can address any namespace.

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

## Persistence

A materialized table remains available until you drop it. Refreshing or materializing the same name replaces its contents but keeps the table.

The schema name `materialized` is reserved: a data source may not be named `materialized` (the config validator rejects it).

## With agents (MCP)

The [MCP server](./mcp.md) exposes `materialize` and `drop_materialized` tools, so an agent can persist a result it computed and query it back later:

```json
{ "name": "materialize", "arguments": { "name": "cohort", "sql": "SELECT …" } }
```

## Example

A runnable example lives in `examples/materialize/` — a config plus a CSV, with try-it commands for the query, file, refresh, and drop flows.
