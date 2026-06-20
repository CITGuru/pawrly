# Installing Pawrly

> Query APIs, files, MCP tools, and databases with SQL. Describe each
> source once, then use the same table and column names from the CLI, scripts, or
> MCP clients. Apache-2.0.

This document is written for LLMs and agents: it is the fastest path from nothing
to a working `pawrly` binary and a first query. Everything here is POSIX-friendly
and copy-pasteable.

## Install (macOS / Linux)

Download a prebuilt binary for your platform:

```sh
curl -fsSL https://pawrly.dev/install.sh | sh
```

This installs the `pawrly` binary to `~/.local/bin` (override with
`PAWRLY_INSTALL_DIR`). It detects your OS/arch, verifies the SHA-256 checksum, and
falls back to building from source with `cargo` if no prebuilt binary matches.

Prebuilt binaries are published for Linux (`x86_64`, `aarch64`) and macOS (Apple
Silicon and Intel).

Pin a version or change the install location:

```sh
curl -fsSL https://pawrly.dev/install.sh \
  | PAWRLY_VERSION=v0.1.0 PAWRLY_INSTALL_DIR=/usr/local/bin sh
```

## Install (Windows, PowerShell)

```powershell
irm https://pawrly.dev/install.ps1 | iex
```

## Install with Cargo

Straight from source, no install script:

```sh
cargo install --git https://github.com/CITGuru/pawrly pawrly-cli
```

Requires Rust ≥ 1.85 (2024 edition) and a C/C++ toolchain for DuckDB
(`xcode-select --install` on macOS; `build-essential pkg-config libssl-dev cmake`
on Debian/Ubuntu).

## Verify

Run the engine with no sources, no network, no config:

```sh
pawrly sql "SELECT 1 AS hello"
```

A single-row table confirms a healthy install.

## First query — join two local files

Create `pawrly.yaml`:

```yaml
version: 1
name: quickstart

sources:
  - name: data
    kind: file
    tables:
      - name: customers
        path: ./data/customers.csv
        format: csv
      - name: orders
        path: ./data/orders.csv
        format: csv
```

Then query across both files in one statement:

```sh
pawrly sql "
  SELECT c.name, COUNT(o.id) AS orders, SUM(o.amount_cents)/100 AS total
  FROM data.customers c
  LEFT JOIN data.orders o ON o.customer_id = c.id
  GROUP BY c.name
  ORDER BY total DESC
"
```

## First query — join two live APIs

Describe each API once, then join them in plain SQL — no SDKs, no pagination loops:

```yaml
version: 1
name: quickstart
secrets:
  - kind: env   # resolves ${secret:NAME} from environment variables

sources:
  - name: stripe
    kind: http
    config:
      base_url: https://api.stripe.com
      auth:
        type: header
        headers:
          - name: Authorization
            bearer: ${secret:STRIPE_API_KEY}
    tables:
      - name: customers
        endpoint: /v1/customers
        response:
          path: $.data
          schema:
            - { name: email,      type: varchar }
            - { name: delinquent, type: bool }
```

```sh
pawrly sql "SELECT email FROM stripe.customers WHERE delinquent = true"
```

Point an `http` source at an OpenAPI 3.0 spec and Pawrly synthesizes one table per
`GET` operation automatically — no hand-written schema.

## Connect Pawrly to an agent (MCP)

Pawrly ships an MCP server, so Claude Desktop, Cursor, Codex, and other clients can
query the same workspace your CLI uses, over stdio or HTTP:

```sh
pawrly mcp-stdio --config /absolute/path/to/pawrly.yaml
```

Pawrly also *consumes* other MCP servers as sources — their tools become tables you
can query and join.

## Useful CLI commands

- `pawrly sql "<query>"` — run a query.
- `pawrly schema` — list every table the workspace knows about.
- `pawrly validate` — sanity-check the YAML without running anything.
- `pawrly serve --config ./pawrly.yaml` — run a local daemon for faster invocations.
- `pawrly status` — confirm a running daemon and that sources loaded.

## Environment overrides for the install script

- `PAWRLY_VERSION` — tag to install (e.g. `v0.1.0`). Default: latest release.
- `PAWRLY_INSTALL_DIR` — directory to install into. Default: `$HOME/.local/bin`.
- `PAWRLY_REPO` — `owner/repo` to pull releases from. Default: `CITGuru/pawrly`.
- `PAWRLY_NO_VERIFY` — set to `1` to skip SHA-256 checksum verification.
- `PAWRLY_BUILD_FROM_SOURCE` — set to `1` to `cargo install` instead of a prebuilt.

## Links

- Source: https://github.com/CITGuru/pawrly
- Docs: https://github.com/CITGuru/pawrly#quickstart
- Sources reference: https://github.com/CITGuru/pawrly/blob/main/docs/sources.md
- MCP guide: https://github.com/CITGuru/pawrly/blob/main/docs/mcp.md
- Semantic layer: https://github.com/CITGuru/pawrly/blob/main/docs/semantic.md
