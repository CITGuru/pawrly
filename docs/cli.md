# CLI

A single binary, `pawrly`. Every command runs against the same engine: in-process by default, or against a `pawrly serve` daemon when one is discovered.

```
pawrly [OPTIONS] <COMMAND>
```

## Global options

| Option | Description |
|---|---|
| `-c, --config <PATH>` | Path to `pawrly.yaml` (env: `PAWRLY_CONFIG`). |
| `--home <PATH>` | Pawrly home directory (env: `PAWRLY_HOME`). |
| `--remote <ENDPOINT>` | Talk to a daemon, e.g. `uds:///path/to/sock` or `tcp://host:port` (env: `PAWRLY_REMOTE`). `--remote off` forces in-process. |
| `--no-remote` | Force in-process execution; never look for a daemon (env: `PAWRLY_NO_REMOTE`). |
| `--log-level <LEVEL>` | Log verbosity: `error` \| `warn` \| `info` \| `debug` \| `trace` (default `info`; env: `PAWRLY_LOG`; `RUST_LOG` is also honored). |

**Engine selection.** With neither `--remote` nor `--no-remote`, Pawrly auto-discovers a daemon over its Unix socket under `$PAWRLY_HOME` and falls back to in-process if none is healthy. For a TCP endpoint, a bearer token can be supplied via `PAWRLY_API_TOKEN`.

## Commands

| Command | What it does |
|---|---|
| [sql](#pawrly-sql) | Run a SQL query. |
| [semantic](#pawrly-semantic) | Browse and query the semantic layer. |
| [schema](#pawrly-schema) | List the catalog or describe a table. |
| [source](#pawrly-source) | Manage workspace sources. |
| [cache](#pawrly-cache) | Inspect and manage the cache. |
| [config](#pawrly-config) | Inspect the assembled config. |
| [init](#pawrly-init) / [validate](#pawrly-validate) | Create / check a `pawrly.yaml`. |
| [materialize](#pawrly-materialize) | Persist data as a named table. |
| [check](#pawrly-check) | Run each source's `examples:` as live probes. |
| [serve](#pawrly-serve) / [stop](#pawrly-stop) / [status](#pawrly-status) | Run and manage the daemon. |
| [console](#pawrly-console) | Serve the web [Console](./console.md) (gRPC-Web + embedded UI). |
| [mcp-stdio](#pawrly-mcp-stdio) | Run the MCP server over stdio. |
| [mcp-http](#pawrly-mcp-http) | Run the MCP server over HTTP. |
| `version` | Print the engine version and health. |

---

### `pawrly sql`

Run a one-shot SQL query.

```
pawrly sql [SQL] [--file <PATH>] [--format <FMT>] [--max-rows <N>]
```

- `[SQL]` — the query; use `-` to read from stdin.
- `-f, --file <PATH>` — read the query from a file instead.
- `--format <FMT>` — `table` (default), `json`, `ndjson`, or `csv`.
- `--max-rows <N>` — cap rows shown (`0` = unlimited).

```bash
pawrly sql "SELECT * FROM data.orders LIMIT 5"
pawrly sql --file report.sql --format csv > report.csv
```

---

### `pawrly explain`

Show the query plan for a SQL string. By default it plans without scanning; with `--analyze` it runs the query and annotates the plan with runtime metrics.

```bash
pawrly explain "SELECT status, COUNT(*) FROM data.orders GROUP BY status"
pawrly explain --analyze "SELECT * FROM data.orders WHERE status = 'paid'"
```

- `[SQL]` — the query to plan; use `-` to read from stdin, or `--file <PATH>`.
- `--analyze` — execute and annotate the plan with runtime metrics.
- `--json` — emit `{ "plan": "..." }` instead of plain text.

---

### `pawrly semantic`

Browse and query the [semantic layer](./semantic.md).

```
pawrly semantic list                       # list models (--json)
pawrly semantic describe <MODEL>           # dimensions, measures, relationships (--json adds segments)
pawrly semantic query <MEASURE>...         # run a structured query
    --by <MEMBER>                          # group-by dimension (repeatable)
    --where '<MEMBER> <OP> <VALUE>'        # filter (repeatable)
    --segment <MODEL.SEGMENT>              # apply a named filter set (repeatable)
    --order-by <MEMBER[:desc]>             # ordering (repeatable)
    --param <NAME=VALUE>                   # bind a ${param:NAME} placeholder (repeatable)
    --limit <N>
    --time-zone <TZ>                       # e.g. America/New_York, for grain truncation
    --format table|json|ndjson|csv
```

```bash
pawrly semantic query orders.revenue --by orders.status --where 'orders.status = paid'
pawrly semantic query orders.revenue --by orders.order_date.month --order-by orders.order_date.month:desc
```

`<OP>` accepts `=`, `!=`, `>`, `>=`, `<`, `<=`, `in`, `not_in`, `in_range`, `contains`, `starts_with`, `ends_with`, `is_null`, `is_not_null`. Values for `in`/`not_in`/`in_range` are comma-separated.

---

### `pawrly schema`

List every registered table, or describe one.

```bash
pawrly schema                 # all tables (--json)
pawrly schema data.orders     # columns + types for one table (--json)
pawrly schema snapshot        # compact full-catalog overview for grounding/tooling
```

- `--json` — emit JSON instead of a table.
- `snapshot` accepts `--sources <a,b>` to scope it and `--compact` for a terser form.

---

### `pawrly source`

Manage the sources in your workspace. Mutating subcommands edit `pawrly.yaml` and propagate the change to the running engine.

```
pawrly source add <KIND> --name <NAME> [flags]   # add a source and append it to pawrly.yaml
pawrly source list                               # list sources (annotated with the file each came from) (--json)
pawrly source remove <NAME>
pawrly source refresh <NAME>                     # re-discover a source's tables
pawrly source test <NAME>                        # check a source is reachable (exits 2 on failure)
```

For `source add`, `<KIND>` is positional and must be one of `file`, `http`, `sqlite`, `postgres`, `mysql`, `duckdb`, `snowflake`, `iceberg`, `ducklake`, `delta`. Flags:

- `--name <NAME>` — logical source name, a valid SQL identifier (required).
- `--description <TEXT>` — optional human-readable description.
- `--path <PATH>` — file path / glob (kinds: `file`, `iceberg`, `delta`, …).
- `--url <URL>` — base URL (HTTP-shaped sources).
- `--token <TOKEN>` — auth token (HTTP-shaped); pass `${secret:NAME}` to indirect through the secret store.
- `--dsn <DSN>` — DSN / URL for SQL-engine sources (`postgres`, `mysql`, `snowflake`, …).
- `--set <KEY=VALUE>` — generic per-kind config field (repeatable). `VALUE` is parsed as JSON when possible, else a string. (Named `--set` to avoid colliding with the global `--config`.)
- `--raw-table` — HTTP-shaped only: register a raw-HTTP table named after the source.

```bash
pawrly source add http --name gh --url https://api.github.com --token '${secret:GH_TOKEN}'
pawrly source add file --name data --path './data/*.parquet'
pawrly source add postgres --name pg --dsn '${secret:PG_DSN}'
```

---

### `pawrly cache`

Inspect and manage the per-table cache (see [Configuration → Caching](./config.md#caching)).

```
pawrly cache list                       # entries with mode, freshness, rows, size (--json)
pawrly cache show <SOURCE>.<TABLE>       # detailed view of one entry
pawrly cache refresh <SOURCE>.<TABLE>    # force a re-fetch + write-through (or pass a bare source name to refresh its catalog)
pawrly cache invalidate <SOURCE>.<TABLE> # drop the entry and its files
pawrly cache vacuum                      # reclaim expired entries, orphaned files, stale temp (--json)
```

---

### `pawrly materialize`

Persist data as a named, self-backed table queryable as `materialized.<NAME>` (see [Materialized tables](./materialize.md)). The source can be a SQL query, a local file, or an `http(s)` URL.

```
pawrly materialize <NAME> "<SQL>"          # persist a query result
pawrly materialize <NAME> --file <PATH>    # persist a local CSV/Parquet/JSON file
pawrly materialize <NAME> --url <URL>      # persist a remote http(s) file
pawrly materialize <NAME> --drop           # drop a materialized table
```

```bash
pawrly materialize top_customers \
  "SELECT customer, SUM(amount) AS total FROM stripe.charges GROUP BY 1 ORDER BY 2 DESC LIMIT 10"

pawrly materialize sales --file ./data/sales.csv --format csv
pawrly sql "SELECT * FROM materialized.top_customers"

# re-run a materialized table's origin (re-query / re-read the file or URL):
pawrly cache refresh materialized.top_customers
```

Options: `--format parquet|csv|json` (inferred from the extension for `--file`/`--url`), `--param KEY=VALUE` (repeatable, substitutes `${param:KEY}` in the SQL), `--json`.

---

### `pawrly config`

Inspect the assembled configuration (after `include:`/`from:` resolution and secret masking).

```bash
pawrly config show          # the effective config
pawrly config show --raw    # verbatim, secrets unmasked
pawrly config show --tree   # show which file each piece came from
pawrly config reload        # re-read pawrly.yaml into a running engine (--json)
```

---

### `pawrly init`

Write a starter `pawrly.yaml`.

```bash
pawrly init                  # writes ./pawrly.yaml
pawrly init path/to/file.yaml # write to a custom path
pawrly init --force          # overwrite an existing file
```

- `[PATH]` — where to write (default `./pawrly.yaml`).
- `--force` — overwrite an existing file.

---

### `pawrly validate`

Validate a config without running anything. Reports every problem at once.

```bash
pawrly validate
pawrly validate path/to/pawrly.yaml
```

---

### `pawrly check`

Run every source's [`examples:`](./sources.md#examples) statements as live probes, so a broken endpoint or credential is caught now rather than at first query. Exits non-zero if any example fails.

```
pawrly check [--source <NAME>]
```

```bash
pawrly check
pawrly check --source gh
```

---

### `pawrly serve`

Run the daemon. Subsequent CLI commands auto-discover it over its socket.

```
pawrly serve [--addr <ADDR>] [--socket <PATH>] [--bearer-token-from <NAME>]
             [--tls-cert <PEM> --tls-key <PEM>] [--idle-timeout <DUR>] [--pid-file <PATH>]
             [--console [--cors-origin <ORIGIN>]]
```

- `--addr <ADDR>` — bind address; accepts `unix:///path` (or `uds://`) and `tcp://host:port`. Defaults to `unix://$PAWRLY_HOME/sockets/pawrly.sock`.
- `--socket <PATH>` — override the UDS path directly; equivalent to `--addr unix://<path>`.
- `--bearer-token-from <NAME>` — require a bearer token, resolved from the config's secret backend or an env var of the same name. Required for non-loopback TCP.
- `--tls-cert <PEM>` / `--tls-key <PEM>` — serve TLS; both must be given together.
- `--idle-timeout <DUR>` — shut down after idle (humantime, e.g. `30m`; `0` = never).
- `--pid-file <PATH>` — write the daemon PID here.
- `--console` — serve the web [Console](./console.md) (gRPC-Web + embedded UI) over TCP instead of the machine wire; with `--addr` use `tcp://host:port` or `host:port` (default `127.0.0.1:8787`). `--cors-origin <ORIGIN>` allows a cross-origin browser origin. Equivalent to `pawrly console`.

(The workspace config comes from the global `-c, --config`.)

```bash
pawrly serve &
pawrly status
pawrly stop
```

---

### `pawrly stop`

Signal a running daemon to shut down (Unix only).

```
pawrly stop [--pid-file <PATH>] [--force]
```

- `--pid-file <PATH>` — path to the daemon's PID file. Default: `$PAWRLY_HOME/sockets/pawrly.pid`.
- `--force` — send `SIGKILL` instead of `SIGTERM`.

---

### `pawrly status`

Probe a running daemon and print its health.

```
pawrly status [--endpoint <ENDPOINT>] [--json]
```

- `--endpoint <ENDPOINT>` — endpoint to probe. Defaults to the default UDS path under `$PAWRLY_HOME`.
- `--json` — emit machine-readable JSON.

---

### `pawrly console`

Serve the web [Console](./console.md) for the discovered workspace (gRPC-Web plus the embedded UI), so a browser can inspect sources, the catalog, semantic models, the cache, and run SQL. A convenience for the same path as `serve --console`; honors the global `--remote` / `--config` / `--home`.

```
pawrly console [--addr <ADDR>] [--bearer-token-from <NAME>] [--cors-origin <ORIGIN>]
```

- `--addr <ADDR>` — TCP address to bind (default `127.0.0.1:8787`). Loopback needs no token; a non-loopback bind requires `--bearer-token-from`.
- `--bearer-token-from <NAME>` — require a bearer token, resolved from the config's secret backend or an env var of the same name; the browser sends it as gRPC-Web metadata.
- `--cors-origin <ORIGIN>` — allow this browser origin for standalone (cross-origin) hosting, e.g. `https://console.example.com`. Omit for same-origin (embedded) use.

```bash
pawrly console                                   # http://127.0.0.1:8787
pawrly console --remote uds:///path/to/pawrly.sock   # local UI, proxied to a remote daemon
```

A non-loopback Console must be fronted with TLS (the token and results otherwise cross the wire in cleartext); the UI assets are bundled only when the binary is built with the `console` feature. See the [Console guide](./console.md).

---

### `pawrly mcp-stdio`

Run the [MCP server](./mcp.md) over stdio so an AI assistant can connect. Honors the global `--remote` / `--no-remote` flags, so it can run the engine in-process or proxy to a daemon.

```bash
pawrly mcp-stdio
pawrly mcp-stdio --remote uds:///path/to/pawrly.sock
```

---

### `pawrly mcp-http`

Run the [MCP server](./mcp.md) over HTTP, for assistants that connect over the network. Honors the global `--remote` / `--no-remote` flags.

```
pawrly mcp-http [--addr <HOST:PORT>] [--bearer-token-from <NAME>]
```

- `--addr <HOST:PORT>` — bind address (default `127.0.0.1:8090`). A non-loopback address requires `--bearer-token-from`.
- `--bearer-token-from <NAME>` — require a bearer token, resolved from the config's secret backend or an env var of the same name; enforced on every request.

```bash
pawrly mcp-http
pawrly mcp-http --addr 0.0.0.0:8090 --bearer-token-from PAWRLY_MCP_TOKEN
```
