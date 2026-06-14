---
name: pawrly
description: Query live, federated data through Pawrly's MCP tools — files, REST/GraphQL APIs, Postgres/MySQL/SQLite/DuckDB, Snowflake, Iceberg/Delta, and other MCP servers, joined in one SQL plan, plus a governed semantic layer. Use this as the entrypoint whenever a task needs real data from connected sources before answering from assumptions or editing code. For authoring config, see pawrly-add-source and pawrly-semantic-model; to pin results, see pawrly-materialize.
version: 0.0.1
---

# Pawrly

## Overview

Pawrly exposes connected data sources as SQL tables (`<source>.<table>`) and federates them into one query engine, so a single statement can join a Parquet file against Postgres against a live API. Reach for Pawrly before answering from assumptions whenever live external state matters.

- **Use the Pawrly MCP tools** for discovery and querying. Prefer them over the `pawrly` CLI; the CLI is for operators setting the workspace up.
- **Two query surfaces:** governed `semantic_query` (preferred when a model covers the question) and ad-hoc `query` (raw SQL).
- Don't fabricate table or column names — discover them first.

## Support checks

- Confirm the Pawrly MCP tools are available before making claims about external systems.
- If Pawrly is unavailable, state the blocker and stop — there is no local recovery.
- Distinguish these failure modes plainly: source not configured, missing credentials, query error, required filter/param unbound, and an empty-but-valid result.

## Discover before you query

Never guess. Ground yourself with these read tools:

| Tool | Input | Use |
|---|---|---|
| `list_sources` | `{}` | what's connected: kinds, status, table counts |
| `search_tables` | `{ query, source?, limit? }` | keyword search — reach for this first on large catalogs |
| `list_tables` | `{ source? }` | tables with schema, description, required filters |
| `list_columns` | `{ table?, source?, name? }` | which tables expose a column (e.g. `name: "created_at"`) |
| `describe_table` | `{ table }` | one `<schema>.<table>`: columns, pushdown affordances, **example queries**, `wiki` notes |
| `get_schema` | `{ sources?, compact? }` | compact whole-catalog overview in one call |

Keep discovery bounded: search or scope to one source/table rather than dumping everything. `describe_table` returns the source's known-good `examples:` and required filters — read them before writing a query.

## Querying

### Semantic layer (prefer when a model exists)

A model gives a curated vocabulary (`orders.revenue` by `orders.status`) with built-in governance — row-level security, row caps, and fan-out protection. Flow:

1. `list_semantic_models` → `{}`
2. `describe_semantic_model` → `{ name }` — learn dimensions, measures, relationships, named **segments**, and any **required filters / RLS params**.
3. `semantic_query`:
   ```json
   {
     "measures": ["orders.revenue"],
     "dimensions": ["orders.order_date.month", "orders.status"],
     "filters": [{ "member": "orders.status", "op": "equals", "values": ["paid"] }],
     "order_by": [{ "member": "orders.order_date.month", "direction": "asc" }],
     "limit": 100,
     "params": { "tenant_id": "acme" }
   }
   ```
   A **member** is `model.dimension` (optionally `.grain`) or `model.measure`. `params` binds `${param:NAME}` RLS placeholders — omit a required one and the query is **refused before any scan**, never silently run across tenants.

### Raw SQL

```json
{ "sql": "SELECT status, COUNT(*) FROM data.orders GROUP BY status", "max_rows": 100, "query_id": "report-42" }
```
`query` returns `{ columns, rows, row_count, truncated }`; `max_rows` defaults to 1000. Pass a `query_id` so a concurrent `cancel_query` (`{ query_id }`) can abort a long scan.

## Query rules

- Address tables as `data.orders` (or `"data"."orders"`), never `"data.orders"`.
- **Set required filters.** Many sources require them (HTTP `params: {required: true}`, `safety.require_filters_on`); `describe_table` lists them. Missing them errors or fans out.
- Add a `LIMIT` unless the user wants complete output.
- Cross-source joins work — each source is scanned, then joined locally.
- Lead with the answer or the blocker. Include SQL only when it helps the user trust or reuse the result; don't dump exhaustive column lists.

## Persisting results

To pin an expensive result, snapshot, or load a file as a queryable table, use the `materialize` MCP tool → `materialized.<name>`. See the **pawrly-materialize** skill.

## Authoring config

Connecting a new source or adding/changing a semantic model is a config-file + CLI task, not an MCP call:

- **pawrly-add-source** — add or fix a source in `pawrly.yaml`.
- **pawrly-semantic-model** — author or update semantic models.

## Deliverable

Name the source, table, required filters, and query shape you used. Report evidence, gaps, and the next action. If editing code, let the Pawrly result drive the change.
