# Changelog

All notable changes to Pawrly are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- `pawrly update` — upgrade the installed binary in place to the latest release (or a pinned `--version`), with `--check` to report availability without installing.
- `pawrly uninstall` — remove the installed binary, with `--purge` to also delete the Pawrly home directory (`$PAWRLY_HOME` / `~/.pawrly`).

### Changed

- `install.sh` / `install.ps1` now upgrade in place: re-running skips the download when already up to date (override with `PAWRLY_FORCE=1`).

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

