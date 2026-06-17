# Pawrly Documentation

One SQL dialect over your APIs, files, databases, and AI models — no ETL, no warehouse, no per-source query language. Just `pawrly sql`.

## Start here

- **[Overview](./overview.md)** — what Pawrly is, how to install it, and a five-minute quickstart.
- **[Architecture](./architecture.md)** — how the engine, sources, cache, and frontends fit together.

## Reference

- **[Configuration](./config.md)** — the `pawrly.yaml` contract: sources, secrets, caching, safety, multi-file layouts.
- **[Sources](./sources.md)** — every source kind and how to configure it.
- **[CLI](./cli.md)** — the `pawrly` command reference.
- **[Materialized tables](./materialize.md)** — persist a query, file, or URL as a named, self-backed table.
- **[MCP server](./mcp.md)** — connect AI agents (Claude Desktop, Cursor, Codex, …) over the Model Context Protocol.
- **[Semantic layer](./semantic.md)** — business-named models, dimensions, measures, and relationships for humans and agents.
- **[Observability](./observability.md)** — traces, metrics, and the queryable `system.activity` log over OpenTelemetry and Prometheus.

## At a glance

```bash
# query the built-in engine — no config, no network
pawrly sql "SELECT 1 AS hello"

# query local files
pawrly sql "SELECT * FROM data.orders LIMIT 10"

# browse the semantic layer
pawrly semantic list

# serve a daemon and connect an agent
pawrly serve &
pawrly mcp-stdio
```
