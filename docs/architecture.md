# Architecture

Pawrly has one query engine and several **interfaces**: the CLI, MCP server, REST API, Console, and Flight SQL. An interface is how a person or client uses Pawrly.

Every interface calls the same engine API, `EngineService`. The engine can run inside the interface's process or in a separate daemon without changing the available operations.

## The big picture

```
   CLI    MCP server        browser / BI / scripts        ← interfaces
    │         │                      │
   in-process or       ┌─────────────┼──────────────┐
    │  gRPC   │     REST · gRPC-Web · Console   Flight SQL
    │         │       (pawrly-server, HTTP)   (pawrly-flight)   ← transports
    └────┬────┴──────────────┬───────────────────┬──┘
         ▼                   ▼                   ▼
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

`EngineService` is the common API for running queries, browsing the catalog, managing sources and stored data, and using semantic models. In Rust, it is a **trait**: a contract that lists the operations an engine must provide without prescribing where they run.

Pawrly has two implementations:

- **`LocalEngine`** — runs everything in-process. This is the default; a bare `pawrly sql` spins one up, queries, and exits.
- **`RemoteEngineClient`** — forwards each call to a `pawrly serve` daemon over gRPC.

Both implementations use the same request and result types, so interfaces do not need separate local and remote behavior. A new interface translates its own protocol into `EngineService` calls rather than reimplementing the engine.

### Sources

A source is a named set of tables backed by some external system or local files. Each kind plugs into the engine as a DataFusion table provider. See **[Sources](./sources.md)** for the catalogue and configuration. The whole workspace is described by one [`pawrly.yaml`](./config.md).

### Cache

Caching is **opt-in per table**. When enabled, a table's results are written to Parquet on disk with a JSON manifest, so:

- the cache survives process restarts,
- it's engine-neutral (plain Parquet),
- and concurrent processes (e.g. the CLI and a running daemon) share it safely — writes are atomic and the manifest is merged under a cross-process lock rather than overwritten.

A corrupt cache file is quarantined and the query transparently re-fetches, so a bad file never fails a read. See the cache modes in **[Configuration](./config.md)**.

### Materialized tables

A **materialized table** saves the result of a query, local file, or remote URL as a named SQL table. Reads use the saved Parquet file, and the table remains available until you replace, refresh, or drop it. Materialized tables use the reserved `materialized` schema and are addressed as `materialized.<name>`. See **[Materialized tables](./materialize.md)**.

### The semantic layer

On top of raw tables, you can define **business models** — named dimensions, measures, relationships, and reusable **segments** (named filter sets) — and query them structurally instead of writing SQL. The semantic compiler turns a structured query into SQL the engine already knows how to run, including cross-model joins and row-level security. Models can also declare **pre-aggregations** whose stored rollups answer matching queries without scanning the source table again. See **[Semantic layer](./semantic.md)**.

## Transports

A **transport** carries an interface's requests to an engine running in another process. Choosing a transport changes how requests travel, not what the engine can do. An in-process interface needs no transport; a daemon can expose several at once.

- **gRPC (`pawrly-server`)** — the default daemon transport. Local clients normally connect through a Unix domain socket, a filesystem endpoint available only on the same machine. Remote clients connect over TCP. The CLI uses a running local daemon when it finds the socket and otherwise starts an in-process engine.
- **HTTP (`pawrly-server`)** — one TCP port serves the **[REST/JSON API](./api.md)**, gRPC-Web for browser clients, and the embedded **[Console](./console.md)** application. Start it with `pawrly console` or `pawrly serve --console`. All three call the same engine services.
- **Arrow Flight SQL (`pawrly-flight`)** — exposes the engine over the Flight SQL protocol so BI and analytics clients (`pyarrow.flight`, ADBC drivers, Dremio, Tableau, Power BI) can talk to Pawrly like any Flight SQL database.

An operator can run several at once — Flight SQL for BI tools, gRPC for the CLI/MCP, HTTP for the browser — all backed by one `EngineService`.

A Unix domain socket uses file permissions (mode 0600) to restrict local access. A TCP listener refuses to bind a non-loopback address without **bearer-token authentication**.

The gRPC transport can terminate **TLS** directly using a PEM certificate and key. The HTTP transport has no built-in TLS, so public deployments must put it behind a TLS-terminating reverse proxy.

The [MCP server](./mcp.md) is an interface, so it can run the engine in-process or proxy to a daemon — letting several agents share one engine and one cache.

## Design principles

- **One SQL dialect.** You learn DataFusion SQL once; it works over every source.
- **One config file.** `pawrly.yaml` is the single source of truth (splittable across files when it grows — see [Configuration](./config.md)).
- **Local/daemon parity.** Identical results in every mode is a hard invariant, not a goal.
- **Open formats on disk.** Parquet for the cache, JSON for the manifest, YAML for config — nothing proprietary.
