# pawrly (Python)

Transport-agnostic client for the Pawrly engine — query live, federated data (files, REST/GraphQL APIs, Postgres/MySQL/SQLite/DuckDB, Snowflake, Iceberg/Delta, other MCP servers) and a governed semantic layer over one SQL surface. Pick a transport at construction — **gRPC**, **REST**, or an **in-process** managed engine — and every method after that is identical.

## Install

Not yet published; use it from this workspace. The REST and in-process paths need only `requests`; the gRPC transport needs the `grpc` extra plus generated stubs (see [gRPC transport](#grpc-transport)).

```sh
pip install -e .            # REST + local (requests only)
pip install -e '.[grpc]'    # + gRPC (grpcio, protobuf, pyarrow)
```

## Connecting

Construct with a factory classmethod.

```python
from pawrly import PawrlyClient

# gRPC — highest fidelity: typed Arrow, streaming, cancel ids. Attach to `pawrly serve`.
client = PawrlyClient.grpc("tcp://127.0.0.1:8787", bearer="…")

# REST — plain JSON over HTTP; firewall-friendly. Attach to `pawrly console`.
client = PawrlyClient.rest("http://127.0.0.1:8787", bearer="…")

# In-process — spawns and owns a `pawrly console` child; no daemon to run yourself.
with PawrlyClient.local(config="pawrly.yaml") as client:
    ...
# Outside a `with`, call client.close() — for `local` it also stops the child.
```

| Transport | Attach to | Strengths | Factory |
|---|---|---|---|
| gRPC | `pawrly serve` | Typed Arrow, streaming, cancel ids | `PawrlyClient.grpc(endpoint, bearer=…)` |
| REST | `pawrly console` | Plain JSON; only `requests` | `PawrlyClient.rest(base_url, bearer=…)` |
| local | — (spawns its own) | Zero infra; no daemon to run yourself | `PawrlyClient.local(config=…)` |

## Running queries

`query()` returns a streaming `QueryHandle`: iterate its rows, or `.collect()` them into a `QueryResult`.

```python
# Collect the whole result.
res = client.query("select id, name from data.orders limit 10").collect()
print(res.columns)          # ["id", "name"]
print(res.rows)             # [{"id": 1, "name": "…"}, …] — values are typed
print(res.row_count, res.truncated)

# Or stream rows — memory-bounded (gRPC frames / REST NDJSON).
for row in client.query("select * from data.big_table"):
    handle_row(row)

# Params (`${param:KEY}` substitution) and a row cap.
q = client.query(
    "select * from data.orders where status = ${param:status}",
    params={"status": "paid"},
    limit=1000,
)
```

### Cancelling

Over gRPC, `query()` carries the server-assigned id (empty over REST, which has no query id).

```python
q = client.query("select * from data.huge")
client.cancel(q.id)   # e.g. from another thread / on a timeout
```

## Semantic queries

```python
from pawrly import SemanticQuery, SemanticFilter, SemanticOrder

res = client.semantic_query(SemanticQuery(
    measures=["orders.revenue", "orders.count"],
    dimensions=["orders.status"],
    filters=[SemanticFilter(member="orders.status", op="in", values=["paid", "shipped"])],
    order_by=[SemanticOrder(member="orders.revenue", desc=True)],
    limit=100,
)).collect()
```

Filter `op`s: `equals`, `not_equals`, `in`, `not_in`, `gt`, `gte`, `lt`, `lte`, `in_range`, `contains`, `starts_with`, `ends_with`, `is_null`, `is_not_null`.

## Materializing

Persist a query result as a self-backed table, queryable as `materialized.<name>`.

```python
from pawrly import MaterializeSpec

out = client.materialize("daily_revenue", MaterializeSpec(sql="select … from data.orders"))
print(out.name, out.row_count, out.file_path)
back = client.query("select * from materialized.daily_revenue").collect()
```

## Introspection

Discover the catalog, cache, functions, and semantic layer.

```python
h = client.health()                        # HealthReport(ok, version)
plan = client.explain("select 1", analyze=True)

client.list_sources()                      # [SourceInfo(name, kind, status, table_count, ...)]
client.list_tables(source="data")          # [TableInfo(name=TableName(schema, table), kind, cached, ...)]
client.describe_table("data.orders")       # TableDescription(table, columns=[ColumnSpec(...)], ...)
client.schema_snapshot(compact=True)       # CatalogSnapshot(schemas=[SchemaSummary(...)]) — compact grounding
client.cache_entries()                     # [CacheEntryInfo(name, mode, row_count, size_bytes, ...)]
client.list_functions()                    # [FunctionInfo(namespace, name, signature, builtin, ...)]
client.describe_function("file", "glob")   # FunctionDescription(signature, args, returns, ...)
client.list_semantic_models()              # [SemanticModelInfo(name, source, dimension_count, measure_count)]
client.describe_semantic_model("orders")   # SemanticModelDescription(dimensions, measures, relationships, segments)
```

## Managing sources & cache

Register/probe/drop sources, reload config, and manage the cache.

```python
client.add_source({"name": "logs", "kind": "file", "config": {"path": "./logs/*.parquet"}})  # → SourceInfo
client.test_source("logs")                 # SourceTestReport(ok, latency_ms, detail)
client.remove_source("logs")               # bool
client.reload_config()                     # ReloadReport(sources_added, …)
client.refresh_catalog()                   # RefreshCatalogOutcome — re-introspect sources

client.refresh_table("data.orders")        # RefreshOutcome — rebuild a cached table
client.invalidate_cache("data.orders")     # bool — drop its cache
client.vacuum_cache()                      # VacuumReport — reclaim stale space
client.drop_materialized("daily_revenue")  # bool — inverse of materialize()
```

## Errors

Every failure is a `PawrlyError` with a stable `code`.

```python
from pawrly import PawrlyError, UnsupportedByTransport

try:
    client.query("select * from nope").collect()
except PawrlyError as e:
    print(e.code, e.message)              # e.g. PAWRLY_UNKNOWN_TABLE
```

Where a transport can't do something it raises `UnsupportedByTransport` (a `PawrlyError`, code `PAWRLY_UNSUPPORTED`) rather than degrading silently — e.g. `shutdown()` over REST, because a daemon won't stop itself for a client.

## Capability matrix

| | gRPC | REST / local |
|---|---|---|
| `query` values | typed Arrow (lossless) | typed JSON (int/float/bool/null; temporal/decimal → string) |
| `query` streaming | yes (frames) | yes (NDJSON); semantic is buffered |
| `query.id` for cancel | yes | empty |
| `shutdown` | server no-op | UnsupportedByTransport |
| everything else | ✓ | ✓ |

## Method reference

- `query(sql, params=None, limit=None) -> QueryHandle`
- `semantic_query(SemanticQuery) -> QueryHandle`
- `explain(sql, analyze=False) -> str`
- `cancel(query_id) -> bool`
- `materialize(name, MaterializeSpec) -> MaterializeOutcome`
- `list_sources() -> list[SourceInfo]`
- `list_tables(source=None, name_glob=None) -> list[TableInfo]`
- `describe_table(name) -> TableDescription`  — `name` is `"schema.table"`
- `schema_snapshot(sources=None, compact=False) -> CatalogSnapshot`
- `cache_entries() -> list[CacheEntryInfo]`
- `list_functions() -> list[FunctionInfo]`
- `describe_function(namespace, name) -> FunctionDescription`
- `list_semantic_models() -> list[SemanticModelInfo]`
- `describe_semantic_model(name) -> SemanticModelDescription`
- `add_source(definition) -> SourceInfo`  — `definition` is the YAML config as a dict
- `remove_source(name) -> bool`
- `test_source(name) -> SourceTestReport`
- `reload_config() -> ReloadReport`
- `refresh_catalog(source=None) -> RefreshCatalogOutcome`
- `refresh_table(name) -> RefreshOutcome`  — `name` is `"schema.table"`
- `invalidate_cache(name) -> bool`  — `name` is `"schema.table"`
- `vacuum_cache() -> VacuumReport`
- `drop_materialized(name) -> bool`
- `health() -> HealthReport`
- `shutdown() -> None`
- `close() -> None`

`QueryHandle` — `id`, iterable over rows, `collect() -> QueryResult(columns, rows, row_count, truncated)`. `MaterializeSpec(sql, params=None)`.

Every method returns the same shapes over every transport. This client covers the full `EngineService` surface — all 25 methods, runtime-verified over REST, gRPC, and `local`.

## gRPC transport

The gRPC stubs are generated from the protos with `grpc_tools.protoc`:

```sh
pip install -e '.[grpc,dev]'
./scripts/generate.sh          # → src/pawrly/v1/*_pb2*.py
./test/run-grpc-smoke.sh       # runtime smoke vs a live `pawrly serve`
```

## Status

Facade + REST (`requests`) + in-process (`local`) + gRPC (`grpcio` + `pyarrow`, a lazy optional), all runtime-verified: REST and `local` against a live `pawrly console`, gRPC against `pawrly serve` (on Python 3.12 — the system's 3.14 still lacks prebuilt grpcio/pyarrow wheels, so install under 3.12 for the gRPC transport). Covers the full `EngineService` surface — all 25 methods (query/semantic, catalog & semantic introspection, cache, and source/config management) — each runtime-verified across every transport.