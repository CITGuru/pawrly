# Architecture

Pawrly is one binary with a clean seam down the middle: a **query engine** on one side, and **frontends** (CLI, MCP server) on the other. They talk through a single trait, so the same engine runs in-process or behind a daemon with no behavioural difference.

## The big picture

```
        ┌─────────────┐   ┌─────────────┐
        │     CLI      │   │  MCP server │     ← frontends
        └──────┬──────┘   └──────┬──────┘
               │                 │
               └────────┬────────┘
                        ▼
                 EngineService            ← one trait, two implementations
                 ┌──────┴───────┐
                 ▼              ▼
           LocalEngine    RemoteEngineClient
          (in-process)    (gRPC to a daemon)
                 │
                 ▼
            DataFusion  ──►  sources: files, HTTP, AI, SQLite, …
                 │           (each a DataFusion table/UDF)
                 ▼
            cache (Parquet + JSON manifest, opt-in per table)
```

## Core pieces

### The query engine

DataFusion plans and executes every query. Sources are exposed to it as tables (and AI models as SQL functions), so a join across a CSV file and a REST API is a single DataFusion plan. For sources that DuckDB already speaks, an in-memory DuckDB instance acts as a sub-engine; everything still flows through one DataFusion plan and one SQL dialect.

### The `EngineService` seam

Every frontend programs against one trait — `EngineService`. It's satisfied by either:

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

### The semantic layer

On top of raw tables, you can define **business models** — named dimensions, measures, and relationships — and query them structurally instead of writing SQL. The semantic compiler turns a structured query into SQL the engine already knows how to run, including cross-model joins and row-level security. See **[Semantic layer](./semantic.md)**.

## Transports

The daemon can be reached over a Unix domain socket (the default for local use) or TCP. The CLI auto-discovers a running daemon over its socket and falls back to in-process execution if none is found. The [MCP server](./mcp.md) is itself a frontend, so it can run the engine in-process or proxy to a daemon — letting several agents share one engine and one cache.

## Design principles

- **One SQL dialect.** You learn DataFusion SQL once; it works over every source.
- **One config file.** `pawrly.yaml` is the single source of truth (splittable across files when it grows — see [Configuration](./config.md)).
- **Local/daemon parity.** Identical results in every mode is a hard invariant, not a goal.
- **Open formats on disk.** Parquet for the cache, JSON for the manifest, YAML for config — nothing proprietary.
