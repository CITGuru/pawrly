# @pawrly/client

Transport-agnostic TypeScript client for the Pawrly engine — query live, federated data (files, REST/GraphQL APIs, Postgres/MySQL/SQLite/DuckDB, Snowflake, Iceberg/Delta, other MCP servers) and a governed semantic layer over one SQL surface. Pick a transport at construction — **gRPC**, **REST**, or an **in-process** managed engine — and every method after that is identical.

## Install

Not yet published; use it from this workspace. The gRPC transport needs the generated Connect-ES stubs (see [Build](#build)).

```sh
pnpm install
pnpm run build   # buf generate + tsc → dist
```

## Connecting

Choose a transport, then supply that transport's args — the compiler enforces the right args for each.

```ts
import { PawrlyClient } from "@pawrly/client";

// gRPC — highest fidelity: typed Arrow, streaming, cancel ids. Attach to `pawrly serve`.
const client = new PawrlyClient({ transport: "grpc", endpoint: "tcp://127.0.0.1:8787", bearer: process.env.PAWRLY_TOKEN });

// REST — plain JSON over HTTP; firewall/browser-friendly. Attach to `pawrly console`.
// const client = new PawrlyClient({ transport: "rest", baseUrl: "http://127.0.0.1:8787", bearer: process.env.PAWRLY_TOKEN });

// In-process — spawns and owns a `pawrly console` child; no daemon to run yourself.
// const client = await PawrlyClient.local({ config: "pawrly.yaml" });

// ... use the client, then release it ...
client.close();   // for `local`, this also stops the child process
```

| Transport | Attach to | Strengths | Args |
|---|---|---|---|
| `grpc` | `pawrly serve` | Typed Arrow, streaming, cancel ids — highest fidelity | `endpoint`, `bearer?` |
| `rest` | `pawrly console` | Plain JSON, firewall/browser-friendly | `baseUrl`, `bearer?` |
| `local` | — (spawns its own) | Zero infra; no daemon to run yourself | `config?`, `home?`, `binary?` |

## Running queries

`query()` returns a streaming `QueryHandle`: async-iterate its rows, or `.collect()` them into a `QueryResult`.

```ts
// Collect the whole result.
const res = await (await client.query("select id, name from data.orders limit 10")).collect();
console.log(res.columns);            // ["id", "name"]
console.log(res.rows);               // [{ id: 1, name: "…" }, …] — values are typed (numbers, booleans, null)
console.log(res.rowCount, res.truncated);

// Or stream rows — memory-bounded (gRPC frames / REST NDJSON).
for await (const row of await client.query("select * from data.big_table")) {
  handleRow(row);
}

// Params (`${param:KEY}` substitution) and a row cap.
const q = await client.query("select * from data.orders where status = ${param:status}", {
  params: { status: "paid" },
  limit: 1000,
});
```

### Cancelling

Over gRPC, `query()` carries the server-assigned id; pass it to `cancel()` (it's empty over REST, which has no query id).

```ts
const q = await client.query("select * from data.huge");
setTimeout(() => client.cancel(q.id), 2000);
for await (const row of q) { /* … */ }
```

## Semantic queries

Query the governed semantic layer — measures by dimensions, with filters, segments, and order.

```ts
const res = await (await client.semanticQuery({
  measures: ["orders.revenue", "orders.count"],
  dimensions: ["orders.status"],
  filters: [{ member: "orders.status", op: "in", values: ["paid", "shipped"] }],
  orderBy: [{ member: "orders.revenue", desc: true }],
  limit: 100,
})).collect();
```

Filter `op`s: `equals`, `not_equals`, `in`, `not_in`, `gt`, `gte`, `lt`, `lte`, `in_range`, `contains`, `starts_with`, `ends_with`, `is_null`, `is_not_null`.

## Materializing

Persist a query result as a self-backed table, queryable as `materialized.<name>`.

```ts
const out = await client.materialize("daily_revenue", { kind: "query", sql: "select … from data.orders" });
console.log(out.name, out.rowCount, out.filePath);
const back = await (await client.query("select * from materialized.daily_revenue")).collect();
```

## Introspection

Discover the catalog, cache, functions, and semantic layer.

```ts
const h = await client.health();                       // { ok, version }
const plan = await client.explain("select 1", true);   // optimized (or analyzed) plan text

await client.listSources();                            // SourceInfo[] — name, kind, status, tableCount, …
await client.listTables("data");                       // TableInfo[] — name: { schema, table }, kind, cached, …
await client.describeTable("data.orders");             // TableDescription — table, columns: ColumnSpec[], …
await client.schemaSnapshot(undefined, true);          // CatalogSnapshot — compact grounding overview
await client.cacheEntries();                           // CacheEntryInfo[] — name, mode, rowCount, sizeBytes, …
await client.listFunctions();                          // FunctionInfo[] — namespace, name, signature, builtin, …
await client.describeFunction("file", "glob");         // FunctionDescription — signature, args, returns, …
await client.listSemanticModels();                     // SemanticModelInfo[] — name, source, dimensionCount, …
await client.describeSemanticModel("orders");          // SemanticModelDescription — dimensions, measures, joins, segments
```

## Managing sources & cache

Register/probe/drop sources, reload config, and manage the cache.

```ts
await client.addSource({ name: "logs", kind: "file", config: { path: "./logs/*.parquet" } });  // → SourceInfo
await client.testSource("logs");                       // SourceTestReport { ok, latencyMs, detail? }
await client.removeSource("logs");                     // boolean
await client.reloadConfig();                           // ReloadReport { sourcesAdded, … }
await client.refreshCatalog();                         // RefreshCatalogOutcome — re-introspect sources

await client.refreshTable("data.orders");              // RefreshOutcome — rebuild a cached table
await client.invalidateCache("data.orders");           // boolean — drop its cache
await client.vacuumCache();                            // VacuumReport — reclaim stale space
await client.dropMaterialized("daily_revenue");        // boolean — inverse of materialize()
```

## Errors

Every failure is a `PawrlyError` with a stable `code`.

```ts
import { PawrlyError, UnsupportedByTransport } from "@pawrly/client";

try {
  await (await client.query("select * from nope")).collect();
} catch (e) {
  if (e instanceof PawrlyError) console.error(e.code, e.message);   // e.g. PAWRLY_UNKNOWN_TABLE
}
```

Where a transport can't do something it raises `UnsupportedByTransport` (a `PawrlyError` with code `PAWRLY_UNSUPPORTED`) against the published capability matrix — it does not silently degrade. For example, `shutdown()` over REST throws, because a daemon won't stop itself for a client.

## Capability matrix

| | `grpc` | `rest` / `local` |
|---|---|---|
| `query` values | typed Arrow (lossless) | typed JSON (int/float/bool/null; temporal/decimal → string) |
| `query` streaming | yes (frames) | yes (NDJSON); semantic is buffered |
| `query.id` for cancel | yes | empty |
| `shutdown` | server no-op | `UnsupportedByTransport` |
| everything else | ✓ | ✓ |

Over gRPC, 64-bit integers arrive as a JS `number` when they fit a safe integer (±2^53) and as `BigInt` beyond that, where precision needs it — so ordinary values match the REST/JSON shape.

## Method reference

- `query(sql, opts?) → Promise<QueryHandle>` — `opts: { params?, limit? }`
- `semanticQuery(q) → Promise<QueryHandle>`
- `explain(sql, analyze?) → Promise<string>`
- `cancel(queryId) → Promise<boolean>`
- `materialize(name, spec) → Promise<MaterializeOutcome>`
- `listSources() → Promise<SourceInfo[]>`
- `listTables(source?, nameGlob?) → Promise<TableInfo[]>`
- `describeTable(name) → Promise<TableDescription>` — `name` is `"schema.table"`
- `schemaSnapshot(sources?, compact?) → Promise<CatalogSnapshot>`
- `cacheEntries() → Promise<CacheEntryInfo[]>`
- `listFunctions() → Promise<FunctionInfo[]>`
- `describeFunction(namespace, name) → Promise<FunctionDescription>`
- `listSemanticModels() → Promise<SemanticModelInfo[]>`
- `describeSemanticModel(name) → Promise<SemanticModelDescription>`
- `addSource(def) → Promise<SourceInfo>` — `def` is the YAML config as an object
- `removeSource(name) → Promise<boolean>`
- `testSource(name) → Promise<SourceTestReport>`
- `reloadConfig() → Promise<ReloadReport>`
- `refreshCatalog(source?) → Promise<RefreshCatalogOutcome>`
- `refreshTable(name) → Promise<RefreshOutcome>` — `name` is `"schema.table"`
- `invalidateCache(name) → Promise<boolean>` — `name` is `"schema.table"`
- `vacuumCache() → Promise<VacuumReport>`
- `dropMaterialized(name) → Promise<boolean>`
- `health() → Promise<HealthReport>`
- `shutdown() → Promise<void>`
- `close() → void`

`QueryHandle` — `id: string`, async-iterable over rows, `collect() → Promise<QueryResult>`. `QueryResult` — `{ columns, rows, rowCount, truncated }`. `MaterializeSpec` — `{ kind: "query", sql, params? }`.

Every method returns the same shapes over every transport. This client covers the full `EngineService` surface — all 25 methods, runtime-verified over gRPC, REST, and `local`.

## Build

The gRPC transport is generated from the protos with `buf` + `protoc-gen-es` into `src/gen/` (gitignored):

```sh
pnpm run generate    # buf → src/gen (from ../../crates/pawrly-proto/proto)
pnpm run build       # tsc → dist
pnpm run typecheck   # generate + tsc --noEmit
pnpm run smoke       # runtime REST smoke vs a live `pawrly console` (needs `cargo build -p pawrly-cli`)
pnpm run smoke:grpc  # runtime gRPC smoke vs a live `pawrly serve`
pnpm run smoke:read  # runtime read-method smoke over REST + gRPC vs `examples/semantic`
pnpm run smoke:mut   # runtime mutating-method smoke over REST + gRPC (disposable workspace copies)
```

## Status

Facade + gRPC (Connect-ES / `@connectrpc/connect-node`) + REST (`fetch`) + in-process (`local`) — all runtime-verified against a live `pawrly console` / `pawrly serve`. Covers the full `EngineService` surface — all 25 methods (query/semantic, catalog & semantic introspection, cache, and source/config management) — each runtime-verified across every transport.
