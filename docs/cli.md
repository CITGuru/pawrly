# CLI

A single binary, `pawrly`. Every command runs against the same engine — in-process by default, or against a `pawrly serve` daemon when one is discovered.

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
| `--log-level <LEVEL>` | Log verbosity (env: `PAWRLY_LOG`; `RUST_LOG` is also honored). |

**Engine selection.** With neither `--remote` nor `--no-remote`, Pawrly auto-discovers a daemon over its Unix socket under `$PAWRLY_HOME` and falls back to in-process if none is healthy. For a TCP endpoint, a bearer token can be supplied via `PAWRLY_API_TOKEN`.

## Commands

| Command | What it does |
|---|---|
| [`sql`](#pawrly-sql) | Run a SQL query. |
| [`semantic`](#pawrly-semantic) | Browse and query the semantic layer. |
| [`schema`](#pawrly-schema) | List the catalog or describe a table. |
| [`source`](#pawrly-source) | Manage workspace sources. |
| [`cache`](#pawrly-cache) | Inspect and manage the cache. |
| [`config`](#pawrly-config) | Inspect the assembled config. |
| [`init`](#pawrly-init) / [`validate`](#pawrly-validate) | Create / check a `pawrly.yaml`. |
| [`serve`](#pawrly-serve) / `stop` / `status` | Run and manage the daemon. |
| [`mcp-stdio`](#pawrly-mcp-stdio) | Run the MCP server over stdio. |
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

### `pawrly semantic`

Browse and query the [semantic layer](./semantic.md).

```
pawrly semantic list                       # list models (--json)
pawrly semantic describe <MODEL>           # dimensions, measures, relationships (--json)
pawrly semantic query <MEASURE>...         # run a structured query
    --by <MEMBER>                          # group-by dimension (repeatable)
    --where '<MEMBER> <OP> <VALUE>'        # filter (repeatable)
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
pawrly schema                 # all tables
pawrly schema data.orders     # columns + types for one table
```

---

### `pawrly source`

Manage the sources in your workspace.

```
pawrly source add ...        # add a source (flag-driven; e.g. --name, --kind, --url, --token, --set k=v)
pawrly source list           # list sources (annotated with the file each came from)
pawrly source remove <NAME>
pawrly source refresh <NAME> # re-discover a source's tables
pawrly source test <NAME>    # check a source is reachable
```

---

### `pawrly cache`

Inspect and manage the per-table cache (see [Configuration → Caching](./config.md#caching)).

```
pawrly cache list                       # entries with mode, freshness, rows, size (--json)
pawrly cache show <SOURCE>.<TABLE>       # detailed view of one entry
pawrly cache refresh <SOURCE>.<TABLE>    # force a re-fetch + write-through
pawrly cache invalidate <SOURCE>.<TABLE> # drop the entry and its files
pawrly cache vacuum                      # reclaim expired entries, orphaned files, stale temp (--json)
```

---

### `pawrly config`

Inspect the assembled configuration (after `include:`/`from:` resolution and secret masking).

```bash
pawrly config show          # the effective config
pawrly config show --raw    # verbatim, secrets unmasked
pawrly config show --tree   # show which file each piece came from
```

---

### `pawrly init`

Write a starter `pawrly.yaml`.

```bash
pawrly init            # writes ./pawrly.yaml
pawrly init --force    # overwrite an existing file
```

---

### `pawrly validate`

Validate a config without running anything. Reports every problem at once.

```bash
pawrly validate
pawrly validate path/to/pawrly.yaml
```

---

### `pawrly serve`

Run the daemon. Subsequent CLI commands auto-discover it over its socket.

```
pawrly serve [--config <PATH>] [--addr <HOST:PORT>] [--socket <PATH>]
```

Use `pawrly status` to check a running daemon and `pawrly stop` to shut it down.

```bash
pawrly serve &
pawrly status
pawrly stop
```

---

### `pawrly mcp-stdio`

Run the [MCP server](./mcp.md) over stdio so an AI assistant can connect. Honors the global `--remote` / `--no-remote` flags, so it can run the engine in-process or proxy to a daemon.

```bash
pawrly mcp-stdio
pawrly mcp-stdio --remote uds:///path/to/pawrly.sock
```
