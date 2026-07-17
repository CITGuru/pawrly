# Overview

## Introduction

Pawrly is a SQL query engine that exposes local files, databases, REST APIs, and MCP tools as tables and functions in one workspace, where they can be queried separately or joined in one statement. No ETL pipelines, no warehouse to stand up, no learning a different query language per source.

The engine can run inside a command process or as a daemon and is available through the CLI, MCP and HTTP interfaces, Console, TypeScript and Python clients, and Rust library. 

Pawrly under the hood:

- **DataFusion** plans and executes every query — you write one SQL dialect.
- **DuckDB (in-memory)** acts as a sub-engine for sources DuckDB already supports.
- **HTTP and MCP sources** are native query providers, so an API or tool call is just another table or function in your SQL.
- **Caching** is opt-in per table and writes Parquet + a JSON manifest to disk, so it survives restarts and is shared safely between processes.

Pawrly is built for two audiences:

- **Data engineers** who want SQL over APIs and files without scheduling extracts or running a warehouse.
- **AI agents** that need a deterministic, audited query surface. Pawrly ships an [MCP server](./mcp.md) so assistants can query the same workspace your CLI uses.

The same engine is reachable three ways: in-process (the default), over a local daemon (`pawrly serve`), or over the network — and every frontend produces identical results.

## Installation

Tested on macOS (Apple Silicon and Intel) and Linux (`x86_64`, `aarch64`).

### Install a prebuilt binary

```bash
curl -fsSL https://raw.githubusercontent.com/CITGuru/pawrly/main/scripts/install.sh | sh
```

This installs the `pawrly` binary to `~/.local/bin` (override with `PAWRLY_INSTALL_DIR`). It detects your OS/arch, verifies the SHA-256 checksum, and falls back to building from source with `cargo` if no prebuilt binary matches your platform.

Pin a version or change where it lands:

```bash
curl -fsSL https://raw.githubusercontent.com/CITGuru/pawrly/main/scripts/install.sh \
  | PAWRLY_VERSION=v0.1.0 PAWRLY_INSTALL_DIR=/usr/local/bin sh
```

On Windows (PowerShell):

```powershell
irm https://raw.githubusercontent.com/CITGuru/pawrly/main/scripts/install.ps1 | iex
```

With Cargo, straight from source:

```bash
cargo install --git https://github.com/CITGuru/pawrly pawrly-cli
```

### Update and uninstall

`pawrly update` upgrades the binary in place to the latest release:

```bash
pawrly update              # upgrade to the latest release
pawrly update --check      # report whether a newer version exists
pawrly update --version v0.1.0   # pin a specific tag
```

Re-running the install script upgrades an existing install too, skipping the download when already up to date (`PAWRLY_FORCE=1` to reinstall).

`pawrly uninstall` removes the binary. Add `--purge` to also delete the Pawrly home directory (`$PAWRLY_HOME` / `~/.pawrly` — cache, materialized tables, and daemon state); your project `pawrly.yaml` files are never touched.

```bash
pawrly uninstall           # remove the binary (prompts to confirm)
pawrly uninstall --purge   # also delete cached/materialized data
```

### Build from source

Build the full workspace with Cargo when you want to hack on Pawrly itself.

### Prerequisites

- **Rust ≥ 1.85** with the 2024 edition. The repository pins the toolchain, so `rustup` installs the right version automatically the first time you run `cargo`.
- A **C/C++ toolchain** (DuckDB builds from source):
  - macOS: `xcode-select --install`
  - Debian/Ubuntu: `sudo apt-get install build-essential pkg-config libssl-dev cmake`
  - Fedora: `sudo dnf install @development-tools openssl-devel cmake`
- `git`.

### Build

```bash
git clone https://github.com/CITGuru/pawrly.git
cd pawrly
cargo build --workspace --release
```

The binary lands at `./target/release/pawrly`. Add `./target/release` to your `PATH`, or invoke the binary directly. The rest of the docs assume `pawrly` is on your `PATH`.

## Quickstart

### 1. Run a query with no setup

Start with the engine itself — no sources, no network, no config:

```bash
pawrly sql "SELECT 1 AS hello"
```

You get a single-row table back. With no `pawrly.yaml` in the current directory, Pawrly runs against an empty workspace — enough to exercise the SQL engine end-to-end without credentials.

### 2. Query local files

Create a tiny dataset:

```bash
mkdir -p data
cat > data/customers.csv <<'CSV'
id,name,plan
1,Acme Corp,enterprise
2,Globex,starter
3,Initech,growth
CSV

cat > data/orders.csv <<'CSV'
id,customer_id,amount_cents
100,1,49900
101,1,12000
102,2,2900
103,3,15000
104,3,15000
CSV
```

Drop a `pawrly.yaml` in the same directory:

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

Now join across both files in one statement:

```bash
pawrly sql "
  SELECT c.name,
         c.plan,
         COUNT(o.id)             AS order_count,
         SUM(o.amount_cents)/100 AS total_dollars
  FROM data.customers c
  LEFT JOIN data.orders o ON o.customer_id = c.id
  GROUP BY c.name, c.plan
  ORDER BY total_dollars DESC
"
```

Acme comes out on top with two orders totalling 619. Swap `format: parquet` and point `path` at a `.parquet` file and the SQL stays identical.

Inspect the workspace or validate its config:

```bash
pawrly schema      # list every table the workspace knows about
pawrly validate    # sanity-check pawrly.yaml without running anything
```

### 3. (Optional) Run as a daemon

For faster CLI invocations, start the local daemon once; subsequent commands auto-discover it over a Unix socket and skip engine warm-up:

```bash
pawrly serve &                                  # background daemon
pawrly status                                   # confirms it's up
pawrly sql "SELECT COUNT(*) FROM data.orders"   # auto-discovers the daemon
```

Commands use the same engine API in local and daemon modes.

### 4. (Optional) Open the Console

Start the [Console](./console.md) for the current workspace:

```bash
pawrly console        # → http://127.0.0.1:8787
```

The default address is loopback and does not require a token.

## Where to next

- Add more sources — see **[Sources](./sources.md)**.
- Shape `pawrly.yaml` — see **[Configuration](./config.md)**.
- Define business models and metrics — see **[Semantic layer](./semantic.md)**.
- Connect an MCP client — see **[MCP server](./mcp.md)**.
- Browse and query in the browser — see **[Console](./console.md)**.
- Full command reference — see **[CLI](./cli.md)**.
