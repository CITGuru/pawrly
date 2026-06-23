# REST API

Pawrly exposes a JSON-over-HTTP API so any client — curl, a JavaScript app, a webhook, a no-code tool — can query your workspace without a gRPC or MCP client. Like the [CLI](./cli.md) and [MCP server](./mcp.md), it's a **frontend**: it runs the same engine, so it sees exactly the data, caching, and [semantic models](./semantic.md) you do.

The two primary endpoints are `POST /v1/sql` (raw SQL) and `POST /v1/query` (a governed [semantic](./semantic.md) query); the rest read the catalog or manage [materialized tables](./materialize.md).

## Running it

The REST API rides the same address as the [Console](./console.md):

```bash
pawrly console --addr 127.0.0.1:8787
```

That one process serves the Console UI, gRPC-Web, **and** REST on the same port. It honors the global engine-selection flags (`--config`, `--home`, `--remote`), so you can point it at a shared daemon. `pawrly serve --console` does the same.

Without a token it refuses to bind anything but a loopback address. To accept remote connections, require a bearer token — resolved from the config's secret backend or an environment variable of the same name:

```bash
pawrly console --addr 0.0.0.0:8787 --bearer-token-from API_TOKEN
```

Every request (except `/healthz` and the spec endpoints) must then carry `Authorization: Bearer <token>`. Terminate TLS in front for a public deployment so the token never crosses the wire in cleartext.

## Quick start

```bash
# raw SQL
curl -s localhost:8787/v1/sql \
  -H 'content-type: application/json' \
  -d '{"sql":"SELECT status, COUNT(*) AS n FROM data.orders GROUP BY status"}'

# with a bearer token
curl -s localhost:8787/v1/sql \
  -H 'authorization: Bearer '"$API_TOKEN" \
  -H 'content-type: application/json' \
  -d '{"sql":"SELECT 1 AS hello"}'
```

```json
{ "columns": ["hello"], "rows": [{ "hello": "1" }], "row_count": 1, "truncated": false }
```

## Endpoints

| Method + path | Purpose |
|---|---|
| `POST /v1/sql` | Run raw SQL |
| `POST /v1/query` | Run a governed semantic query |
| `POST /v1/explain` | Optimized / analyzed plan for a SQL string |
| `GET /v1/sources` | List sources |
| `GET /v1/sources/{name}` | One source's info |
| `GET /v1/tables` | List tables |
| `GET /v1/tables/{schema}.{table}` | Describe a table |
| `GET /v1/schema` | Full catalog snapshot |
| `GET /v1/semantic/models` | List semantic models |
| `GET /v1/semantic/models/{name}` | Describe a semantic model |
| `GET /v1/cache` | Cache inventory |
| `PUT /v1/materialized/{name}` | Create or replace a materialized table |
| `DELETE /v1/materialized/{name}` | Drop a materialized table |
| `GET /v1/health` | Engine health report |
| `GET /healthz` | Liveness (unauthenticated) |
| `GET /v1/openapi.json` / `.yaml` | OpenAPI 3.0 spec (unauthenticated) |

### `POST /v1/sql`

Run raw SQL. Body fields:

| Field | Default | Meaning |
|---|---|---|
| `sql` | — | the query (required) |
| `params` | `{}` | substitutions for `${param:KEY}` placeholders |
| `format` | `json` | `json`, `ndjson`, or `csv` |
| `limit` | `1000` | row cap |

`json` returns `{ columns, rows, row_count, truncated }` with `rows` as objects. `ndjson` streams one JSON object per line (`application/x-ndjson`); `csv` is RFC 4180.

```bash
curl -s localhost:8787/v1/sql -H 'content-type: application/json' \
  -d '{"sql":"SELECT * FROM data.orders LIMIT 2","format":"ndjson"}'
```

```
{"id":"1","customer":"acme","amount_cents":"1000"}
{"id":"2","customer":"ben","amount_cents":"2500"}
```

### `POST /v1/query`

Run a structured query against the [semantic layer](./semantic.md) — the curated business vocabulary (`orders.revenue` by `orders.status`) rather than raw columns. The body is the semantic query; the result is the same `{ columns, rows, row_count, truncated }` shape.

```bash
curl -s localhost:8787/v1/query -H 'content-type: application/json' -d '{
  "measures": ["orders.revenue"],
  "dimensions": ["orders.order_date.month", "orders.status"],
  "filters": [{ "member": "orders.status", "op": "equals", "values": ["paid"] }],
  "order_by": [{ "member": "orders.order_date.month", "direction": "asc" }],
  "limit": 100,
  "params": { "tenant_id": "acme" }
}'
```

`params` binds `${param:NAME}` placeholders used by a model's row-level-security predicates. If a model requires one and you omit it, the query is refused before any scan.

### `POST /v1/explain`

Return the query plan for a SQL string. By default it plans without scanning any data; with `analyze` it runs the query and annotates the plan with runtime metrics. Body fields:

| Field | Default | Meaning |
|---|---|---|
| `sql` | — | the query to plan (required) |
| `analyze` | `false` | when `true`, execute and annotate the plan with runtime metrics (`EXPLAIN ANALYZE`) |

Responds with `{ "plan": "<text>" }`.

```bash
curl -s localhost:8787/v1/explain -H 'content-type: application/json' \
  -d '{"sql":"SELECT status, COUNT(*) FROM data.orders GROUP BY status"}'
```

```json
{ "plan": "Projection: data.orders.status, count(*)\n  Aggregate: groupBy=[[data.orders.status]], aggr=[[count(*)]]\n    TableScan: data.orders" }
```

### Reading the catalog

```bash
curl -s localhost:8787/v1/sources                 # all sources
curl -s localhost:8787/v1/sources/github          # one source
curl -s localhost:8787/v1/tables                  # all tables
curl -s localhost:8787/v1/tables/github.pulls     # one table's schema + wiki
curl -s 'localhost:8787/v1/schema?sources=github,data&compact=true'   # whole catalog in one call
curl -s localhost:8787/v1/semantic/models         # semantic models
curl -s localhost:8787/v1/cache                   # cached tables
curl -s localhost:8787/v1/health                  # { ok, version, active_queries, sources_ok, ... }
```

### Materialized tables

Persist a query, file, or URL as a named, self-backed table queryable as `materialized.<name>` (see [materialized tables](./materialize.md)). `PUT` is create-or-replace by name; the body is the spec, tagged by `kind`:

```bash
# materialize a query result
curl -s -X PUT localhost:8787/v1/materialized/top_customers \
  -H 'content-type: application/json' \
  -d '{"kind":"query","sql":"SELECT * FROM data.customers ORDER BY revenue DESC LIMIT 100"}'

# or a local / remote file
# {"kind":"file","path":"./snapshots/q3.parquet"}
# {"kind":"url","url":"https://example.com/data.csv","format":"csv"}

# drop it
curl -s -X DELETE localhost:8787/v1/materialized/top_customers
```

`PUT` returns `{ name, file_path, row_count, size_bytes }`. `DELETE` returns `{ "dropped": true, "name": "<name>" }`, or `404` if no such table exists.

## Errors

Every error is a JSON envelope with a stable `PAWRLY_*` code and an HTTP status:

```json
{ "error": { "code": "PAWRLY_SAFETY_REQUIRED_FILTER",
             "message": "refusing to scan `tvmaze` without a filter on `request_path`" } }
```

| Status | When |
|---|---|
| `400` | invalid SQL, plan error, or a safety violation |
| `401` | missing or invalid bearer token |
| `404` | unknown table, source, or materialized table |
| `408` | query timeout |
| `499` | client cancelled the request |
| `500` | internal error |
| `503` | engine out of memory |

The same source-level [safety](./config.md) policies (`require_filters_on`, `max_unfiltered_rows`, query timeouts) apply over REST, and the API is **read-only** for data — `INSERT`/`UPDATE`/`DELETE`/DDL are refused.

## OpenAPI spec

The full contract is published as an OpenAPI 3.0 document, served live:

```bash
curl -s localhost:8787/v1/openapi.json    # JSON
curl -s localhost:8787/v1/openapi.yaml    # YAML
```

Point Swagger UI, Postman, or a client generator at it to explore the API or generate a typed client.
