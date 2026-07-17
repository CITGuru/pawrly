# Changelog

All notable changes to Pawrly are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Metric queries now use pre-aggregation rollups: a metric whose leaves are all additive measures covered by a declared rollup reads the materialized pre-agg instead of the base table (window metrics and governed-filter metrics conservatively read base).
- `DropNamespace`: tear down an entire materialize namespace (tables, manifest, storage) in one call — `pawrly cache drop-namespace <ns>`, `DELETE /v1/namespaces/{ns}`, a `DropNamespace` RPC, a `drop_namespace` MCP tool, and SDK methods. The default workspace namespace is refused.
- Metrics discovery: `pawrly semantic metrics`, `list_metrics`/`describe_metric` MCP tools, `ListMetrics`/`DescribeMetric` RPCs, and `/v1/semantic/metrics` REST routes (TS/Python SDK methods included).
- **Semantic metrics** — a workspace-level `semantic.metrics:` block composing measures into named, governed business numbers, queryable by their dot-free name through the existing `semantic query` surface (CLI, MCP, gRPC — no wire change): `pawrly semantic query aov --by orders.status`.
  - `ratio` (numerator/denominator with `NULLIF` guard and `DOUBLE` cast) and `derived` (arithmetic over `{member}` references) kinds, including cross-model ratios via the aggregate-locality compiler.
  - `cumulative` (running total / grain-to-date / trailing window), `offset` (period-over-period value/delta/growth), and `share` (percent-of-partition) kinds. Window metrics join onto a dense time axis — generated automatically or pinned to a declared `semantic.time_spine:` table — so running totals and period-over-period stay calendar-correct across gaps in the data.
  - Governed filters at three levels — metric-level, per-ratio-operand, and per-derived-token — all pushed down to leaf `FILTER (WHERE …)` clauses; metrics may reference other metrics.
  - Config validation: reference resolution, cycle detection, name collisions, window-metric time-dimension and period checks.

- **Custom materialize namespaces** — pass a namespace when creating a materialized table to target an isolated store (own storage subdir, manifest, and SQL address), so the same name never collides across callers: `pawrly materialize <name> "<sql>" --namespace <ns>`, queryable as `<ns>.materialized.<name>`.
  - Threaded through every surface: a `namespace` field on the `Materialize` / `DropMaterialized` / `ListEntries` RPCs, `?namespace=` on the REST endpoints, `--namespace` on `pawrly materialize` and `pawrly cache list`, a `namespace` argument on the MCP tools, and optional parameters in the TypeScript / Python SDKs. Empty or omitted = the default workspace namespace (fully backward compatible).
  - Namespaces are created on first write and resolve in SQL on demand — including after a daemon restart and when written by another process sharing the storage root. Reserved names (`pawrly`, `materialized`, `system`, `information_schema`) and unsafe segments are rejected.
- **Variables** — declared, typed, scoped inputs a source references with `${var:NAME}`.
  - Non-secret config values typed as string, integer, number, boolean, or enum, with defaults, required/optional, and per-machine overrides.
  - Static secrets resolved from the env / keyring / file chain.
  - OAuth-minted secrets via `client_credentials`, `device_code`, and `authorization_code` grants, with optional OIDC endpoint discovery.
  - `pawrly variables set` and `pawrly source connect` to provide values, and a `system.variables` table for introspection.
- `pawrly update` — upgrade the installed binary in place to the latest release (or a pinned `--version`), with `--check` to report availability without installing.
- `pawrly uninstall` — remove the installed binary, with `--purge` to also delete the Pawrly home directory (`$PAWRLY_HOME` / `~/.pawrly`).
- **Client SDKs** — TypeScript (`@pawrly/client`) and Python (`pawrly`) clients that expose the full `EngineService` surface identically over gRPC, REST, and an in-process managed engine (a `pawrly console` child the client owns and tears down).
  - Streaming query results with server-assigned cancel ids, semantic queries, materialized tables, and catalog / cache / source / semantic-model introspection and management.
  - `pawrly-client` gains a REST engine client alongside gRPC, dispatched by endpoint (`tcp://` / `unix://` / `http(s)://` / in-process).
- `pawrly explain` — show the optimized (or `--analyze`d) query plan for a SQL string.
- `pawrly schema snapshot` — a compact full-catalog overview for grounding and tooling.
- `pawrly config reload` — re-read the workspace config into a running engine.
- `--json` as a shorthand for `--format json` on the output-rendering commands.

### Fixed

- Security: runtime `add_source` (gRPC/REST/CLI) now runs the same validation as a config file, and rejects `kind: mcp` with `transport: stdio` outright — a remotely added source can no longer spawn processes on the server host. Declare stdio MCP sources in `pawrly.yaml` instead.
- `pawrly cache refresh materialized.<name>` accepts `--namespace` (and the RPC/REST/MCP surfaces a `namespace`), so materialized tables in custom namespaces can be refreshed; previously only the default workspace namespace was reachable.
- `pawrly validate` now honors the global `--config` / `PAWRLY_CONFIG` like every other command (it previously only looked at `./pawrly.yaml` relative to the shell's cwd).
- `cache list` now reports materialized tables as mode `pinned` instead of the misleading `ttl` fallback (they were never TTL-governed; only the label was wrong). Version-skew note: a pre-`pinned` client listing against a newer daemon omits materialized rows from `cache list` (its decoder drops entries with an unknown mode); all other operations are unaffected. New clients keep such rows and approximate the mode from expiry instead.
- Version-skew guard: the `Materialize` / `DropMaterialized` / `ListEntries` responses now echo the request's namespace, and clients (Rust, TypeScript, Python; gRPC and REST) fail loudly when a namespace-oblivious older server ignores a requested namespace — previously the operation would silently target the default namespace (a namespaced `--drop` could delete the wrong table).

### Changed

- `install.sh` / `install.ps1` now upgrade in place: re-running skips the download when already up to date (override with `PAWRLY_FORCE=1`).
- REST `/v1/sql` and `/v1/query` results return typed JSON scalars (numbers, booleans, null) instead of stringified values.

## [0.1.0](https://github.com/CITGuru/pawrly/releases/tag/v0.1.0) - 2026-06-18

First public release: one SQL dialect over APIs, files, databases, warehouses, and lakehouses, federated into a single query plan. DataFusion plans and executes; an in-process DuckDB acts as a sub-engine for the sources DuckDB already speaks. Every interface (CLI, MCP, gRPC, Flight SQL, web Console) talks the same `EngineService` trait, in-process or against a `pawrly serve` daemon.

### Added

- **Sources**
  - File — parquet, csv, and newline-delimited json, via glob or explicit tables, from the local filesystem, object storage (S3 / GCS / Azure), or plain `http(s)://` URLs.
  - HTTP — REST / GraphQL APIs as typed SQL tables: declared endpoints, params (with required/default), JSON row paths, pagination (offset, page, and row-cursor), per-request endpoint selection, auth (header / basic), and a `raw_table` JSON escape hatch.
  - OpenAPI — point a `kind: http` source at a 3.0.x spec and synthesize one table per GET operation, with `include`/`exclude` catalog filters and on-disk spec caching.
  - MCP — connect to another MCP server (stdio or streamable HTTP) and expose its read-only tools as SQL tables.
  - Databases — Postgres, MySQL, SQLite, and local DuckDB files, attached read-only with WHERE-equality predicate pushdown.
  - Warehouse — Snowflake, attached read-only through the DuckDB `snowflake` community extension.
  - Lakehouse — Iceberg, Delta, and DuckLake catalogs (local or remote).
- **Engine & query surfaces**
  - `pawrly-engine::LocalEngine` — the `EngineService` implementation on DataFusion's `SessionContext`, with cross-source federation, JSON SQL functions, and `spawn_blocking`-wrapped DuckDB calls.
  - Semantic layer — models with dimensions, measures, relationships, named segments, time grains, row-level security, and pre-aggregations (rollup acceleration), queryable via `semantic query`.
  - Materialized tables — pin a query result, file, or URL as a self-backed Parquet table addressable as `materialized.<name>`.
  - Caching — per-source `ttl`, `refresh` (interval), and `cron` modes with atomic Parquet + manifest writes guarded by a cross-process advisory lock, plus background refreshers.
  - Safety guards — row caps, required filters, pagination caps, and RLS predicates refused before any scan when unbound.
- **Transport & Interfaces**
  - `pawrly` CLI — `init`, `validate`, `check`, `config`, `sql`, `schema`, `cache`, `materialize`, `source`, `semantic`, `serve`, `stop`, `status`, `mcp-stdio`, `mcp-http`, `console`, and `version`, with `table` / `json` / `csv` output and local/daemon parity.
  - MCP server (stdio + http+sse) exposing discovery, `query`, and `semantic_query` tools over the same workspace.
  - gRPC daemon (`pawrly serve`), Arrow Flight SQL transport, and a `RemoteEngineClient` over gRPC.
  - Web Console served over gRPC-Web with an embedded SPA (behind the `console` feature).
- **Configuration & operations**
  - YAML workspace config with a generated JSON Schema, multi-file composition (`include:` / `from:`), and environment-aware secret resolution (`env` / `file` / `keyring` / `auto`).
  - Observability — `tracing` logs (text/json), OpenTelemetry (OTLP) trace and metric export, a Prometheus `/metrics` endpoint, and an activity log exposed as the queryable `system.activity` table with optional durable storage.
  - Prebuilt binaries for Linux (`x86_64`, `aarch64`) and macOS (Apple Silicon and Intel), plus `install.sh` / `install.ps1`.

