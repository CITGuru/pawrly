# MCP server

Pawrly's [Model Context Protocol](https://modelcontextprotocol.io) server exposes Pawrly operations as tools for MCP clients such as Claude Desktop, Cursor, and Codex. Clients can inspect the workspace catalog, run SQL or semantic queries, and manage stored data.

By default, the engine runs inside the MCP server process. The server can instead forward engine calls to a shared `pawrly serve` daemon.

## Running it

```bash
pawrly mcp-stdio
```

This speaks MCP over stdio — the transport assistants launch as a subprocess. It honors the global engine-selection flags, so you can point it at a shared daemon:

```bash
pawrly mcp-stdio --remote uds:///path/to/pawrly.sock
```

Run several agents against one daemon and they share one engine and one cache.

### Over HTTP

For network clients, run the HTTP transport instead:

```bash
pawrly mcp-http --addr 127.0.0.1:8090
```

It serves JSON-RPC at `POST /mcp` and a liveness probe at `GET /healthz`. Without a token it refuses to bind anything but a loopback address. To accept remote connections, require a bearer token — resolved from the config's secret backend or an environment variable of the same name:

```bash
pawrly mcp-http --addr 0.0.0.0:8090 --bearer-token-from MCP_TOKEN
```

Every `/mcp` request must then carry `Authorization: Bearer <token>`.

## Connecting Claude Desktop

Add Pawrly to your MCP client config (for Claude Desktop, `claude_desktop_config.json`):

```json
{
  "mcpServers": {
    "pawrly": {
      "command": "pawrly",
      "args": ["mcp-stdio", "--config", "/absolute/path/to/pawrly.yaml"]
    }
  }
}
```

Use an absolute path to the binary if `pawrly` isn't on the client's `PATH`. Other stdio-capable clients (Cursor, Codex, …) use the same command and args.

## Tools

The tools fall into three groups:

- **Discovery** tools inspect sources, tables, columns, functions, and semantic models before querying.
- **Query** tools execute SQL or structured semantic queries and can cancel long-running work.
- **Management** tools refresh cached tables or create and drop materialized tables.

The server exposes:

| Tool | Input | Returns |
|---|---|---|
| `query` | `{ sql, max_rows?, query_id? }` | `{ columns, rows, row_count, truncated }` |
| `cancel_query` | `{ query_id }` | `{ cancelled }` — aborts an in-flight query with that id |
| `list_sources` | `{}` | the configured sources, their kinds, status, and table counts |
| `list_tables` | `{ source? }` | the tables across configured sources |
| `search_tables` | `{ query, source?, limit? }` | tables whose name or description match the keywords, ranked; `{ tables, match_count, truncated }` |
| `list_columns` | `{ table?, source?, name?, limit? }` | columns flattened one-per-row across tables; `name` greps column name/description; `{ columns, column_count, truncated }` |
| `describe_table` | `{ table }` | one table's columns, descriptions, pushdown support, examples, and `wiki` notes |
| `get_schema` | `{ sources?, compact? }` | schemas, tables, and column names in one response |
| `refresh_table` | `{ table }` | forces a cache refresh; returns rows written, size, and expiry |
| `materialize` | `{ name, sql? \| file? \| url?, format?, params?, namespace? }` | persists a named, self-backed table; `{ name, file_path, row_count, size_bytes }` |
| `drop_materialized` | `{ name, namespace? }` | drops a materialized table; `{ dropped }` |
| `list_semantic_models` | `{}` | the semantic models with dimension/measure counts |
| `list_metrics` | `{}` | the workspace metrics (composed business numbers) |
| `describe_metric` | `{ name }` | one metric's kind, members, filter, and format |
| `describe_semantic_model` | `{ name }` | one model's full spec — dimensions, measures, relationships |
| `semantic_query` | a structured query (below) | `{ columns, rows, row_count, truncated }` |

### `query`

Run raw SQL. `max_rows` (default 1000) caps the rows returned. Pass a `query_id` so a concurrent `cancel_query` can abort a long-running scan.

```json
{ "sql": "SELECT status, COUNT(*) FROM data.orders GROUP BY status", "max_rows": 100 }
```

### `cancel_query`

Abort an in-flight `query` or `semantic_query` that was started with the same `query_id`. Returns `{ cancelled }` — `false` if no query with that id was running. The cancel arrives on a separate request, so it is effective over the HTTP transport (where a second connection can reach the server mid-query).

```json
{ "query_id": "report-42" }
```

### `list_sources`

List every configured source with its kind, connection status, and table count. Takes no arguments.

```json
{}
```

### `list_tables`

List tables across all sources, or limit to one with `source`. Each row carries the table's schema, name, kind, description, cache flag, and any required filters.

```json
{ "source": "github" }
```

### `search_tables`

Keyword discovery for large catalogs. Matches the query terms against table names and descriptions (case-insensitive; every term must appear), ranking name hits ahead of description-only hits. Returns `{ tables, match_count, truncated }`. Reach for this before `describe_table` when a source has hundreds of tables.

```json
{ "query": "pull request review", "source": "github", "limit": 20 }
```

### `list_columns`

List columns flattened to one row per column — the column-level counterpart to `list_tables`. Scope with `table` (one table), `source` (one source), and/or `name`, a case-insensitive keyword over column name and description. Use `name` to find which tables expose a column like `created_at` or `email`. Returns `{ columns, column_count, truncated }`.

```json
{ "name": "created_at", "source": "github" }
```

### `describe_table`

Full detail for one fully-qualified `<schema>.<table>`: column schema, pushdown support, example queries, and `wiki` usage notes. Pushdown support describes which filters and limits Pawrly can send to the backing source instead of applying after rows are returned.

```json
{ "table": "github.pulls" }
```

### `get_schema`

Return every schema, its tables, and a one-line column list per table. Limit the response to named sources, or set `compact: false` for fuller detail.

```json
{ "sources": ["github", "warehouse"], "compact": true }
```

### `refresh_table`

Force an immediate cache refresh of a fully-qualified table. Only valid for tables with caching enabled; returns the rows written, size on disk, and next expiry.

```json
{ "table": "github.pulls" }
```

### `materialize`

Persist data as a named, self-backed table queryable as `<namespace>.materialized.<name>` (see [materialize](./materialize.md)). Provide exactly one origin: `sql` (a query), `file` (a local CSV/Parquet/JSON path), or `url` (an http(s) file). Create-or-replace by name; the table is pinned and never auto-evicted. An optional `namespace` targets an isolated [materialize namespace](./materialize.md#custom-namespaces); omitted = the default workspace namespace. Returns `{ name, file_path, row_count, size_bytes }`.

```json
{ "name": "top_customers", "sql": "SELECT * FROM data.customers ORDER BY revenue DESC LIMIT 100" }
```

### `drop_materialized`

Drop a materialized table by name (optionally from a `namespace`). Returns `{ dropped }` — `false` if no such table existed.

```json
{ "name": "top_customers" }
```

### `list_semantic_models`

List the [semantic-layer](./semantic.md) models with their dimension and measure counts. Takes no arguments.

```json
{}
```

### `describe_semantic_model`

Full spec for one model: its dimensions, measures, relationships, named segments (reusable filter sets you can pass in `segments`), and any required filters to satisfy up front.

```json
{ "name": "orders" }
```

### `semantic_query`

Run a structured query against the [semantic layer](./semantic.md). The request names model-defined measures, dimensions, filters, ordering, and query parameters rather than supplying SQL.

```json
{
  "measures": ["orders.revenue"],
  "dimensions": ["orders.order_date.month", "orders.status"],
  "filters": [{ "member": "orders.status", "op": "equals", "values": ["paid"] }],
  "order_by": [{ "member": "orders.order_date.month", "direction": "asc" }],
  "limit": 100,
  "params": { "tenant_id": "acme" },
  "max_rows": 1000
}
```

`params` binds `${param:NAME}` placeholders used by a model's row-level-security predicates. If a model requires a param and the agent omits it, the query is refused before any scan — so an agent can't accidentally read across tenants.

## Typical discovery flow

Before querying an unfamiliar workspace:

1. `list_semantic_models` to see what's available.
2. `describe_semantic_model` to learn a model's dimensions, measures, and any required filters.
3. `semantic_query` (or `query` for ad-hoc SQL) to get results.

`describe_semantic_model` also returns required filters and RLS params that must be included in the query.
