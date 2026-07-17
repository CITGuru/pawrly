# Observability

Pawrly uses the same observability pipeline across its interfaces and transports. Pawrly provides four kinds of operational data:

- **Logs** record individual process events and errors.
- **Traces** break one operation into timed steps, including work across processes.
- **Metrics** aggregate counts, durations, and current state over many operations.
- The **activity log** records one entry per query for history or auditing.

Traces, metrics, and logs can be exported through [OpenTelemetry](https://opentelemetry.io/) (OTLP), metrics can also be scraped by Prometheus, and activity records can be queried as a SQL table.

Telemetry export and the activity log are off by default. Without configuration, Pawrly writes ordinary logs to stderr but sends nothing to OpenTelemetry or Prometheus. Enable individual signals with [CLI flags](./cli.md) for ad-hoc runs or the `observability:` block in [`pawrly.yaml`](./config.md#observability) for persistent settings.

## Quick start

```bash
# JSON logs instead of text
pawrly --log-format json sql "SELECT 1"

# Export traces + metrics + logs to a local OpenTelemetry collector
pawrly --otel-endpoint http://localhost:4317 serve

# Scrape metrics with Prometheus (no collector needed)
pawrly --prometheus-listen 127.0.0.1:9090 serve &
curl -s localhost:9090/metrics | grep pawrly_
```

Or persist it in `pawrly.yaml` (see [Configuration](./config.md#observability)):

```yaml
observability:
  otel:
    enabled: true
    endpoint: http://localhost:4317
    prometheus: { enabled: true, listen: 127.0.0.1:9090 }
  activity:
    enabled: true
    sinks: [tracing, table]
    redact_sql: literals
    store: ~/.pawrly/activity
```

## Logging

Logs go to **stderr** in `text` (default) or `json` form. The level is an [EnvFilter](https://docs.rs/tracing-subscriber/latest/tracing_subscriber/filter/struct.EnvFilter.html) directive; `RUST_LOG` always wins.

| Setting | Flag / env | Config |
|---|---|---|
| Level | `--log-level` / `PAWRLY_LOG` / `RUST_LOG` | `tracing.level` |
| Format | `--log-format` / `PAWRLY_LOG_FORMAT` | `tracing.format` |

The subscriber is unified across the CLI, the daemon, and the MCP server, so all three log the same way.

## Tracing

A trace follows one operation from start to finish. It contains **spans**, where each span measures one part of the work, such as compiling a semantic query, calling an HTTP source, or refreshing a cached table.

When OTLP trace export is enabled, Pawrly creates a span tree for each operation. Span names use `pawrly.<subsystem>.<op>`:

| Span | Covers |
|---|---|
| `pawrly.engine.query` / `pawrly.engine.semantic_query` | query execution |
| `pawrly.engine.explain` / `pawrly.engine.materialize` | explain / materialize |
| `pawrly.semantic.compile` | semantic-model → SQL compilation |
| `pawrly.cache.refresh` | cache write-through |
| `pawrly.source.http.request` | an outbound REST/GraphQL request |
| `pawrly.server.query` | the gRPC transport hop |
| `pawrly.mcp.tool` | an MCP tool call |

Trace context is propagated as W3C `traceparent` across the **gRPC** (CLI → daemon) and **MCP HTTP** boundaries, so a request that crosses processes is a single trace. SQL text and parameter values are never put on spans (cardinality + secrets); they live in the activity log, subject to redaction.

Configure under `otel:` — `endpoint`, `protocol` (`grpc` | `http`), `service_name` (the OTel resource name, default `pawrly`), `sample_ratio` (parent-based), and the `traces` / `logs` toggles.

## Metrics

Metrics summarize behavior across operations rather than preserving individual query records. Counters track totals, histograms record value distributions such as query duration, and an up/down counter records a value that can increase or decrease, such as active queries.

Metrics export over **OTLP push** (`otel.metrics`) and/or a **Prometheus pull** endpoint (`otel.prometheus`) — independently; enable either, both, or neither. The instruments:

| Instrument | Type | Key attributes |
|---|---|---|
| `pawrly.query.total` | counter | `status`, `error_code` |
| `pawrly.query.duration` | histogram (ms) | `status` |
| `pawrly.query.rows_returned` | histogram | |
| `pawrly.query.active` | up/down counter | |
| `pawrly.semantic.compile.duration` | histogram (ms) | |
| `pawrly.cache.refresh.duration` | histogram (ms) | `source`, `status` |
| `pawrly.source.request.total` / `.duration` | counter / histogram | `kind`, `status`, `http.response.status_code` |
| `pawrly.activity.dropped` | counter | |
| `pawrly.activity.redaction_failed` | counter | |

`active_queries` is also surfaced by [`pawrly status`](./cli.md) and the daemon health check.

## Activity log

Unlike metrics, the activity log keeps a separate structured record for each SQL or semantic query. A record includes who ran the query, how long it took, how many rows it returned, and whether it failed.

Enable it under `activity:` and choose one or more destinations:

- **`tracing`** — emits each record as a structured `tracing` event (target `pawrly.activity`), so it flows to your logs and, with OTLP, to your log pipeline.
- **`table`** — exposes the records as the **`system.activity`** SQL table. This table exists only when the `table` destination is enabled:

  ```sql
  SELECT interface, status, count(*) AS n, avg(duration_ms) AS avg_ms
  FROM system.activity
  WHERE at > now() - INTERVAL '1 hour'
  GROUP BY 1, 2
  ORDER BY n DESC;
  ```

### `system.activity` columns

| Column | Notes |
|---|---|
| `id` | operation id |
| `at` | completion time (UTC) |
| `interface` | how it entered: `cli`, `grpc`, `mcp`, `flight`, `in_process` |
| `principal` | authenticated identity, when known |
| `operation` | `query` / `semantic_query` |
| `sql` | redacted per `redact_sql` |
| `param_keys` | parameter **keys** only — never values |
| `status` | `ok` / `error` |
| `error_code` | stable error code on failures |
| `duration_ms`, `rows_returned`, `bytes` | |
| `trace_id` | OTel trace id, to cross-reference a trace |

### SQL redaction

`redact_sql` controls how much of the query text is stored:

| Mode | Stored |
|---|---|
| `false` | the SQL verbatim |
| `literals` | the SQL with literal values replaced by `$REDACTED` (shape kept) |
| `true` | only the statement kind and referenced tables |

Parameter values are never stored under any mode. Redaction is **leak-safe**: if a statement can't be parsed it degrades (literals → tables → the bare leading keyword like `SELECT`) and never falls back to raw text. Redaction parses with the same grammar the engine runs.

### Durability

Without `store`, `system.activity` is a bounded in-memory ring of the most recent `ring_capacity` records, lost on restart. Set `store` to persist records as date/hour-partitioned Parquet (`dt=YYYY-MM-DD/hr=HH/…`), so the table survives restarts — it unions the on-disk history with the not-yet-flushed buffer. Records flush on a `flush_threshold`, a `flush_interval` timer, and on clean shutdown. `retention` prunes files older than its window; omit it to keep everything.

```yaml
activity:
  enabled: true
  sinks: [table]
  store: ~/.pawrly/activity
  partition_hours: 4        # hr= bucket width
  flush_threshold: 1000     # records buffered before a file is written
  flush_interval: 60s       # or this, whichever comes first
  retention: 30d            # prune older files; omit to keep all history
```

## Configuration reference

The full `observability:` block — every field and default — is documented in [Configuration → Observability](./config.md#observability). A runnable config lives at `examples/observability.yaml`.
