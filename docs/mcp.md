# MCP server

Pawrly ships a [Model Context Protocol](https://modelcontextprotocol.io) server so AI assistants — Claude Desktop, Cursor, Codex, and others — can query your workspace directly. The MCP server is a **frontend**: it runs the same engine as the CLI (in-process by default, or proxied to a `pawrly serve` daemon), so an agent sees exactly the data and semantic models you do.

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

The server exposes these tools:

| Tool | Input | Returns |
|---|---|---|
| `query` | `{ sql, max_rows? }` | `{ columns, rows, row_count, truncated }` |
| `list_sources` | `{}` | the configured sources, their kinds, status, and table counts |
| `list_tables` | `{ source? }` | the tables across configured sources |
| `describe_table` | `{ table }` | one table's columns, descriptions, pushdown affordances, and examples |
| `get_schema` | `{ sources?, compact? }` | a compact catalog overview for grounding an LLM |
| `refresh_table` | `{ table }` | forces a cache refresh; returns rows written, size, and expiry |
| `list_semantic_models` | `{}` | the semantic models with dimension/measure counts |
| `describe_semantic_model` | `{ name }` | one model's full spec — dimensions, measures, relationships |
| `semantic_query` | a structured query (below) | `{ columns, rows, row_count, truncated }` |

### `query`

Run raw SQL. `max_rows` (default 1000) caps the rows returned.

```json
{ "sql": "SELECT status, COUNT(*) FROM data.orders GROUP BY status", "max_rows": 100 }
```

### `semantic_query`

Run a structured query against the [semantic layer](./semantic.md) — the recommended surface for agents, because models give them a curated business vocabulary instead of raw column names.

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

## Grounding agents

The intended flow for an assistant:

1. `list_semantic_models` to see what's available.
2. `describe_semantic_model` to learn a model's dimensions, measures, and any required filters.
3. `semantic_query` (or `query` for ad-hoc SQL) to get results.

Because `describe_semantic_model` advertises required filters and RLS params up front, an agent can satisfy them in the very next call.

## Notes

- Two transports ship: **stdio** (`mcp-stdio`) and **HTTP** (`mcp-http`).
- `describe_table` and `refresh_table` take a fully-qualified `<schema>.<table>` name.
- `cancel_query`, MCP resources, and MCP prompts are planned.
