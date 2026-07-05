# Architecture

Pawrly is one binary split between a query engine and frontends (CLI, MCP server, REST/HTTP + Console, Flight SQL). They talk through a single trait, so the same engine runs in-process or behind a daemon with no behavioural difference.

## The big picture

```
   CLI    MCP server        browser / BI / scripts        ← surfaces
    │         │                      │
    │         │        ┌─────────────┼──────────────┐
    │         │     REST · gRPC-Web · Console   Flight SQL
    │         │       (pawrly-server, HTTP)   (pawrly-flight)   ← transports
    └────┬────┴──────────────┬───────────────────┬──┘
         ▼                    ▼                   ▼
                     EngineService                       ← one trait, two impls
                 ┌────────┴────────┐
                 ▼                 ▼
           LocalEngine      RemoteEngineClient
          (in-process)      (gRPC to a daemon)
                 │
                 ▼
            DataFusion  ──►  sources: files, HTTP, SQLite, Postgres,
                 │           MySQL, DuckDB, Snowflake, Iceberg/Delta,
                 │           DuckLake, MCP servers
                 │           (each a DataFusion table provider)
                 ▼
            cache (Parquet + JSON manifest, opt-in per table)
```

## Core pieces

### The query engine

DataFusion plans and executes every query. Sources are exposed to it as tables, so a join across a CSV file and a REST API is a single DataFusion plan. For sources that DuckDB already speaks, an in-memory DuckDB instance acts as a sub-engine; everything still flows through one DataFusion plan and one SQL dialect.

### `EngineService`

Every frontend programs against `EngineService`. It's satisfied by either:

- **`LocalEngine`** — runs everything in-process. This is the default; a bare `pawrly sql` spins one up, queries, and exits.
- **`RemoteEngineClient`** — forwards each call to a `pawrly serve` daemon over gRPC.

Because both implement the same trait, **local mode and daemon mode produce byte-for-byte identical output**. A frontend never needs to know which one it's holding. This is also why a new frontend (today the CLI and the MCP server) is a thin translation layer, not a re-implementation.

### Sources

A source is a named set of tables backed by some external system or local files. Each kind plugs into the engine as a DataFusion table provider. See **[Sources](./sources.md)** for the catalogue and configuration. The whole workspace is described by one [`pawrly.yaml`](./config.md).

### Cache

Caching is **opt-in per table**. When enabled, a table's results are materialized to Parquet on disk with a JSON manifest, so:

- the cache survives process restarts,
- it's engine-neutral (plain Parquet),
- and concurrent processes (e.g. the CLI and a running daemon) share it safely — writes are atomic and the manifest is merged under a cross-process lock rather than overwritten.

A corrupt cache file is quarantined and the query transparently re-fetches, so a bad file never fails a read. See the cache modes in **[Configuration](./config.md)**.

### Materialized tables

Distinct from the cache, a **materialized table** is a named, self-backed table with no upstream. You produce one from a query result, a local file, or a remote URL; it lands as a Parquet artifact addressable as `materialized.<name>` (the reserved `materialized` schema). Unlike a cache entry — which mirrors a live source and expires — a materialized table is **pinned** and changes only when you re-materialize, refresh, or drop it. See **[Materialized tables](./materialize.md)**.

### The semantic layer

On top of raw tables, you can define **business models** — named dimensions, measures, relationships, and reusable **segments** (named filter sets) — and query them structurally instead of writing SQL. The semantic compiler turns a structured query into SQL the engine already knows how to run, including cross-model joins and row-level security. Models can also declare **pre-aggregations** (rollups) that the compiler matches a query against to serve it from a smaller materialized table. See **[Semantic layer](./semantic.md)**.

## Transports

Transports are pluggable: each one wraps an `EngineService` and exposes it over a wire protocol, so the same engine can serve multiple surfaces at once.

- **gRPC (`pawrly-server`)** — the default daemon transport, reachable over a Unix domain socket (the default for local use) or TCP. The CLI auto-discovers a running daemon over its socket and falls back to in-process execution if none is found.
- **HTTP (`pawrly-server`)** — one TCP port fronting three browser-friendly surfaces off the same engine: the **[REST/JSON API](./api.md)** (`/v1/sql`, `/v1/query`, catalog reads, materialized-table management), **gRPC-Web** for the browser client, and the embedded **[Console](./console.md)** SPA. Started with `pawrly console` or `pawrly serve --console`; gRPC-Web and REST share the six service definitions with the machine wire so the two can't drift.
- **Arrow Flight SQL (`pawrly-flight`)** — exposes the engine over the Flight SQL protocol so BI and analytics clients (`pyarrow.flight`, ADBC drivers, Dremio, Tableau, Power BI) can talk to Pawrly like any Flight SQL database.

An operator can run several at once — Flight SQL for BI tools, gRPC for the CLI/MCP, HTTP for the browser — all backed by one `EngineService`.

A UDS leans on file permissions (mode 0600) as its trust boundary. A TCP listener refuses to bind a non-loopback address without **bearer-token auth**. The gRPC transport can also terminate **TLS** directly (PEM cert + key); the HTTP transport runs through axum and carries no built-in TLS, so a public deployment terminates TLS in front (e.g. a reverse proxy) to keep the token off the wire in cleartext.

The [MCP server](./mcp.md) is itself a frontend, so it can run the engine in-process or proxy to a daemon — letting several agents share one engine and one cache.

## Design principles

- **One SQL dialect.** You learn DataFusion SQL once; it works over every source.
- **One config file.** `pawrly.yaml` is the single source of truth (splittable across files when it grows — see [Configuration](./config.md)).
- **Local/daemon parity.** Identical results in every mode is a hard invariant, not a goal.
- **Open formats on disk.** Parquet for the cache, JSON for the manifest, YAML for config — nothing proprietary.
