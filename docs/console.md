# Console

The **Console** is a browser UI for a running Pawrly workspace — browse your sources and their health, the SQL catalog, the semantic layer, the cache and materialized tables, recent query activity, and run ad-hoc SQL with live-streaming results. One command, one page.

It is a static single-page app that talks to the Pawrly daemon over **gRPC-Web** — the same [gRPC contract](./architecture.md) the daemon already serves, no extra backend. v1 is **read-only**: it inspects and queries; it never mutates your workspace.

## Quick start

```bash
# Serve the Console for the workspace in the current directory.
pawrly console
# → open http://127.0.0.1:8787
```

`pawrly console` resolves the workspace exactly like the rest of the [CLI](./cli.md) (`--config`, `$PAWRLY_CONFIG`, `./pawrly.yaml`, or `~/.pawrly/pawrly.yaml`) — so launch it from a project directory and it shows *that* workspace. On loopback it needs no token and no extra setup.

You can also fold it into a daemon you're already running:

```bash
pawrly serve --console --addr 127.0.0.1:8787
```

## What you'll see

A sidebar groups the panels; the toggle in the top bar collapses it to an icon rail.

| Panel | Shows |
|---|---|
| **Sources** | Every registered source, its `kind` (+ a sub-kind flag like `openapi` / `object_storage`), health, and table count. Click a row for its config and tables. |
| **Catalog** | Every SQL table across all sources. Click a table for its columns, types, filter-pushdown flags, examples, and notes. |
| **Semantic** | Governed [semantic models](./semantic.md) — dimensions and measures. Click a model to inspect it **and run it**: pick measures + dimensions and stream the result. |
| **Cache** | [Cached](./config.md#caching) table entries — mode, rows, size, freshness. |
| **Materialized** | [Materialized tables](./materialize.md) (`materialized.*`) — rows, size, files. |
| **Activity** | Recent query activity from [`system.activity`](./observability.md#activity-log). Click a row for the full record. |
| **SQL** | An ad-hoc SQL runner. Results stream in, are row-capped, and can be cancelled mid-flight. |

### SQL runner

Type SQL, press **Run** (or ⌘/Ctrl+Enter). Results stream as they arrive, capped at the row limit you set; **Cancel** stops a long query. Errors are shown verbatim. Every run is recorded in **Activity** (when the activity log is on).

### Activity & traces

The **Activity** panel reads `system.activity`, which exists only when the activity **table sink** is enabled. If it's off, the panel tells you how to turn it on:

```yaml
observability:
  activity:
    enabled: true
    sinks: [tracing, table]
```

See [Observability](./observability.md#activity-log) for the full block. Click any activity row to open its full record — operation, status, timing, rows, the executed SQL (including the compiled SQL for a `semantic_query`), and `trace_id`. Two shortcuts in the detail view:

- **Open in SQL runner** — drops the recorded SQL into the runner to re-run or tweak.
- **Trace ID** becomes a deep link when you set a **Trace URL** template in the sidebar (e.g. `https://jaeger.example.com/trace/{traceId}`), so you can jump straight to the span tree in your tracing backend.

## Connecting

The Console talks to **one daemon at a time**, set by the **Endpoint** field in the sidebar:

- **Embedded (default).** When the daemon serves the Console itself (`pawrly console`), the UI defaults its endpoint to that same origin — zero config.
- **Standalone.** Point the Endpoint field (or a runtime `config.json`) at any daemon's gRPC-Web address to use one UI build against many daemons.

The **Token** field holds a bearer token, kept in memory and sent as gRPC-Web metadata on every call — required whenever the daemon enforces auth (see below).

## Security

The Console inherits the daemon's security posture; nothing new is exposed.

- **Loopback by default.** `pawrly console` binds `127.0.0.1:8787`. A loopback bind needs no token.
- **Token for non-loopback.** Binding a public address without a bearer token is refused at startup. Set one with `--bearer-token-from NAME` (resolved from the config's [secret backend](./config.md#secrets) or an environment variable), and enter it in the UI's Token field.

  ```bash
  pawrly serve --console --addr 0.0.0.0:8787 \
    --bearer-token-from PAWRLY_TOKEN --cors-origin https://console.example.com
  ```

- **TLS for remote.** A non-loopback Console must sit behind a TLS-terminating proxy (or be fronted with TLS) — otherwise the token and results cross the network in cleartext.
- **CORS** is opt-in via `--cors-origin <ORIGIN>`, only needed when the UI is hosted on a different origin than the daemon; scope it to the exact origin.
- **The SQL runner can read anything the engine can.** Treat Console access like database access.

## Command reference

```bash
pawrly console        [--addr 127.0.0.1:8787] [--bearer-token-from NAME] [--cors-origin ORIGIN]
pawrly serve --console [--addr host:port]     [--bearer-token-from NAME] [--cors-origin ORIGIN]
```

Both honor the global workspace flags (`--config`, `--home`, `--remote`, `--no-remote`); `pawrly console --remote <endpoint>` runs a local Console that proxies to a remote daemon. See the [CLI reference](./cli.md).

## Building from source

The official binaries bundle the Console. To build it yourself, build the frontend first (it embeds into the binary), then compile with the `console` feature:

```bash
pnpm --dir apps/console install
pnpm --dir apps/console build      # runs codegen (buf) + vite build → apps/console/dist
cargo build --release -p pawrly-cli --features console
```

Without the `console` feature the `pawrly console` / `serve --console` paths still serve the gRPC-Web endpoint (useful for a separately-hosted SPA in standalone mode) — they just don't bundle the UI assets.

## Related

- [Architecture](./architecture.md) — the engine and gRPC contract the Console consumes.
- [CLI](./cli.md) — `pawrly console` / `serve` and workspace resolution.
- [Configuration](./config.md) — sources, secrets, caching, safety.
- [Semantic layer](./semantic.md) — the models the Semantic panel inspects and runs.
- [Observability](./observability.md) — the `system.activity` table the Activity panel reads.
