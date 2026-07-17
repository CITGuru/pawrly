# Pawrly - One SQL Dialect

> **One SQL dialect over your APIs, files, and warehouses.**
> No ETL, no warehouse, no per-source query language — just `pawrly sql`.

Pawrly gives you a single SQL interface and local execution over heterogeneous data. You can query any REST/GraphQL APIs, local files (parquet, csv, json), object storage (S3/GCS/Azure), MCP servers, relational databases (Postgres, MySQL, SQLite, DuckDB), warehouses (Snowflake), and lakehouses (Iceberg, Delta, DuckLake), and join across all of them in a single statement.

It is built for two audiences:

- **AI agents** that need a deterministic, audited query surface across the tools their humans live in. Pawrly ships a first-class MCP server so Claude Desktop, Cursor, Codex, and the rest can connect over stdio or HTTP and run `query` against the same workspace your CLI uses.
- **Data engineers** who want SQL over APIs and files without standing up a warehouse, scheduling extracts, or learning five vendor query languages.

Pawrly under the hood:

- **DataFusion** plans and executes every query — you write one SQL dialect.
- **DuckDB (in-memory)** acts as a sub-engine for sources DuckDB already supports.
- **HTTP and MCP sources** are native query providers, so an API or tool call is just another table or function in your SQL.
- **Caching** is opt-in per table and writes Parquet + a JSON manifest to disk, so it survives restarts and is shared safely between processes.

---

## Quickstart

### Installation

Install a prebuilt binary:

```bash
curl -fsSL https://raw.githubusercontent.com/CITGuru/pawrly/main/scripts/install.sh | sh
```

This installs the `pawrly` binary to `~/.local/bin` (override with `PAWRLY_INSTALL_DIR`). It detects your OS/arch, verifies the SHA-256 checksum, and falls back to building from source with `cargo` if no prebuilt binary matches your platform.

Prebuilt binaries are published for Linux (`x86_64`, `aarch64`) and macOS (Apple Silicon and Intel).

**Pin a version** or change where it lands:

```bash
curl -fsSL https://raw.githubusercontent.com/CITGuru/pawrly/main/scripts/install.sh \
  | PAWRLY_VERSION=v0.1.0 PAWRLY_INSTALL_DIR=/usr/local/bin sh
```

**Windows** (PowerShell):

```powershell
irm https://raw.githubusercontent.com/CITGuru/pawrly/main/scripts/install.ps1 | iex
```

**With Cargo**, straight from source:

```bash
cargo install --git https://github.com/CITGuru/pawrly pawrly-cli
```

**Update** in place, or **uninstall**:

```bash
pawrly update              # upgrade to the latest release
pawrly update --check      # report whether a newer version exists
pawrly uninstall           # remove the binary (--purge also deletes ~/.pawrly)
```

Re-running the install script upgrades an existing install too, skipping the download when already up to date (`PAWRLY_FORCE=1` to reinstall).

#### Building from source

Tested on macOS (Apple Silicon and Intel) and Linux (`x86_64`).

Prerequisites:

- **Rust** ≥ 1.85 with the 2024 edition (the workspace pins this via [rust-toolchain.toml](./rust-toolchain.toml); `rustup` will install it automatically the first time you run `cargo`).
- A C/C++ toolchain for DuckDB:
  - macOS: `xcode-select --install`
  - Debian/Ubuntu: `sudo apt-get install build-essential pkg-config libssl-dev cmake`
  - Fedora: `sudo dnf install @development-tools openssl-devel cmake`
- `git`.

Clone and build the release binary:

```bash
git clone https://github.com/CITGuru/pawrly.git
cd pawrly
cargo build --workspace --release
```

The binary lands at `./target/release/pawrly`. For the rest of this guide, either add `./target/release` to your `PATH` or invoke `./target/release/pawrly` directly.

To confirm you have a healthy checkout, run the same commands CI runs:

```bash
cargo build --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

### Querying

#### Run your first query

Start with the engine itself — no sources, no network, no config:

```bash
pawrly sql "SELECT 1 AS hello"
```

You should see a single-row table back. With no `pawrly.yaml` in the current directory, Pawrly runs against an empty workspace — enough to exercise the SQL engine end-to-end without credentials.

#### Query Local Files

Pawrly's `file` source exposes Parquet, CSV, and JSON files as tables. This example defines two CSV tables and joins them with SQL.

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

Now query across both files in one statement:

```bash
pawrly sql "
  SELECT c.name,
         c.plan,
         COUNT(o.id)            AS order_count,
         SUM(o.amount_cents)/100 AS total_dollars
  FROM data.customers c
  LEFT JOIN data.orders o ON o.customer_id = c.id
  GROUP BY c.name, c.plan
  ORDER BY total_dollars DESC
"
```

You should see Acme on top with two orders totalling 619, then Initech at 300, then Globex at 29. To swap parquet in for either side, change the table's `format` to `parquet` and point `path` at a `.parquet` file — the SQL stays identical.

For more sources — HTTP APIs, object storage, Postgres, DuckDB, Snowflake, Iceberg/Delta/DuckLake — see the worked examples in [examples/pawrly.yaml](./examples/pawrly.yaml) and the [sources reference](./docs/sources.md).

#### Query an HTTP API — Stripe + Intercom

Pawrly's `http` source maps REST and GraphQL API responses to typed SQL tables. A table declaration specifies the endpoint, the JSON path containing its rows, and the output columns. This example defines tables for two APIs and joins them.

Both APIs need a key. Export them, and let Pawrly read them from the environment:

```bash
export STRIPE_API_KEY=sk_live_...
export INTERCOM_ACCESS_TOKEN=...
```

Point your `pawrly.yaml` at both:

```yaml
version: 1
name: quickstart

secrets:
  - kind: env   # resolves ${secret:...} from environment variables

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
            - { name: id,         type: varchar }
            - { name: email,      type: varchar }
            - { name: name,       type: varchar }
            - { name: balance,    type: bigint }
            - { name: delinquent, type: bool }

  - name: intercom
    kind: http
    config:
      base_url: https://api.intercom.io
      headers:
        Intercom-Version: "2.11"
      auth:
        type: header
        headers:
          - name: Authorization
            bearer: ${secret:INTERCOM_ACCESS_TOKEN}
    tables:
      - name: contacts
        endpoint: /contacts
        response:
          path: $.data
          schema:
            - { name: id,           type: varchar }
            - { name: email,        type: varchar }
            - { name: name,         type: varchar }
            - { name: last_seen_at, type: bigint }
```

The query joins customers from both APIs by email and orders them by `last_seen_at`:

```bash
pawrly sql "
  SELECT c.email,
         c.name,
         i.last_seen_at
  FROM stripe.customers c
  JOIN intercom.contacts i ON i.email = c.email
  ORDER BY i.last_seen_at ASC
"
```

The API shapes are defined in `pawrly.yaml`; the query refers to them as `stripe.customers` and `intercom.contacts`.

#### Other CLI commands

- `pawrly schema` — list every table the workspace knows about (`data.customers`, `data.orders` here).
- `pawrly validate` — sanity-check the YAML without running anything.
- `pawrly status` — confirm a running daemon and that sources loaded (`sources_ok=1`).

Read more on cli commands: [cli.md](./docs/cli.md)

#### (Optional) Run as a daemon

For faster CLI invocations, start the local daemon once; subsequent `pawrly sql` invocations auto-discover it over a Unix socket and skip engine warm-up.

```bash
pawrly serve --config ./pawrly.yaml &     # background with shell job control
pawrly status                              # confirms daemon + sources_ok=1
pawrly sql "SELECT COUNT(*) FROM data.orders"
kill %1                                     # stop the backgrounded daemon
```

Local and daemon modes use the same `EngineService` API.

---

## What's in this repo

- [crates/](./crates) — the Rust workspace.
- [examples/](./examples) — reference configurations including the kitchen-sink workspace covering every source kind.
- [schemas/](./schemas) — generated JSON Schema for `pawrly.yaml`. Wire this into your editor for completion + validation.

---

## Contributing

We work in the open. The contract for every change:

```bash
cargo fmt --all -- --check
cargo build --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --no-fail-fast
cargo deny check
```

House rules (no `unwrap`/`unsafe`/`panic!` outside test code, source-spec ergonomics, local + daemon parity as a release-blocking invariant) are enforced by clippy.

Bug reports, source requests, and design feedback all welcome via GitHub Issues.

---

## License

Apache-2.0.