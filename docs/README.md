# Pawrly Documentation

This directory contains the Pawrly guides and reference documentation.

## Start here

- **[Overview](./overview.md)** — what Pawrly is, how to install it, and a five-minute quickstart.
- **[Architecture](./architecture.md)** — how the engine, sources, interfaces, and transports fit together.

## Reference

- **[Configuration](./config.md)** — the `pawrly.yaml` contract: sources, secrets, caching, safety, multi-file layouts.
- **[Sources](./sources.md)** — every source kind and how to configure it.
- **[Variables](./variables.md)** — declared, typed, scoped `${var:NAME}` inputs: non-secret config, static secrets, and OAuth-minted credentials.
- **[CLI](./cli.md)** — the `pawrly` command reference.
- **[REST API](./api.md)** — query and manage a workspace over JSON-over-HTTP; OpenAPI spec included.
- **[Client SDKs](./clients.md)** — TypeScript and Python clients over gRPC, REST, or a managed local process.
- **[Materialized tables](./materialize.md)** — persist a query, file, or URL as a named, self-backed table.
- **[MCP server](./mcp.md)** — connect AI agents (Claude Desktop, Cursor, Codex, …) over MCP
- **[Console](./console.md)** — run queries and inspect a workspace through the browser interface.
- **[Semantic layer](./semantic.md)** — models, dimensions, measures, relationships, metrics, and pre-aggregations.
- **[Functions](./functions.md)** — reusable, table-valued functions called as `FROM ns.fn(args)` (builtin + declared http/mcp/file).
- **[Observability](./observability.md)** — traces, metrics, and the queryable `system.activity` log over OpenTelemetry and Prometheus.

## At a glance

```bash
# run SQL without a workspace config
pawrly sql "SELECT 1 AS hello"

# query local files
pawrly sql "SELECT * FROM data.orders LIMIT 10"

# browse the semantic layer
pawrly semantic list

# start a daemon and an MCP server
pawrly serve &
pawrly mcp-stdio
```
