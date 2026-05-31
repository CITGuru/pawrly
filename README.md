# Pawrly

> **One SQL dialect over your APIs, files, warehouses, and AI models.**
> No ETL, no warehouse, no per-source query language — just `pawrly sql`.

Pawrly is a Rust binary (and embeddable library) that gives you a single SQL interface over heterogeneous data: REST APIs (GitHub, Linear, Stripe), local files (parquet, csv, json, excel), relational databases (Postgres, MySQL, SQLite), warehouses (Snowflake), lakehouses (Iceberg, Delta, Ducklake), and OpenAI-compatible models — all joinable in one statement, all served from one config file.

It is built for two audiences:

- **AI agents** that need a deterministic, audited query surface across the tools their humans live in. Pawrly ships a first-class MCP server so Claude Desktop, Cursor, Codex, and the rest can connect over stdio or HTTP and run `query` against the same workspace your CLI uses.
- **Data engineers** who want SQL over APIs and files without standing up a warehouse, scheduling extracts, or learning five vendor query languages.

Under the hood: **DataFusion** plans and executes; **DuckDB (in-memory)** acts as a sub-engine for the sources DuckDB already speaks (Postgres, MySQL, Snowflake, Iceberg, Delta, file formats). HTTP and AI sources are pure-Rust DataFusion `TableProvider`s. Every frontend talks the same `EngineService` trait — in-process via `LocalEngine` or over gRPC against a `pawrly serve` daemon.

---

## Quickstart

Tested on macOS (Apple Silicon and Intel) and Linux (x86_64). Should take under 15 minutes on a warm Cargo cache, longer on the first build.

### 1. Prerequisites

- **Rust** ≥ 1.85 with the 2024 edition (the workspace pins this via [`rust-toolchain.toml`](./rust-toolchain.toml); `rustup` will install it automatically the first time you run `cargo`).
- A C/C++ toolchain for DuckDB:
  - macOS: `xcode-select --install`
  - Debian/Ubuntu: `sudo apt-get install build-essential pkg-config libssl-dev cmake`
  - Fedora: `sudo dnf install @development-tools openssl-devel cmake`
- `git`.

### 2. Clone

```bash
git clone https://github.com/withpawrly/pawrly.git
cd pawrly
```

### 3. Build the binary

```bash
cargo build --workspace --release
```

The binary lands at `./target/release/pawrly`. For the rest of this guide, either add `./target/release` to your `PATH` or invoke `./target/release/pawrly` directly.

### 4. Smoke-test the workspace

The same commands CI runs. If these are clean on a fresh clone, you have a healthy checkout:

```bash
cargo build --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

### 5. Run your first query

Start with the engine itself — no sources, no network, no config:

```bash
./target/release/pawrly sql "SELECT 1 AS hello"
```

You should see a single-row table back. With no `pawrly.yaml` in the current directory, Pawrly runs against an empty workspace — enough to exercise the SQL engine end-to-end without credentials.

### 6. Query a real source — local files

Pawrly's `kind: file` source serves parquet, csv, and json from disk through DataFusion's `ListingTable`. Drop in two CSVs and you can join them with SQL — no warehouse, no ETL, no separate import step.

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
./target/release/pawrly sql "
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

`pawrly schema` will list every table the workspace knows about (`data.customers`, `data.orders` here). `pawrly validate` will sanity-check the YAML without running anything.

For more sources — GitHub, Linear, Postgres, Snowflake, AI models — see the worked examples in [`examples/pawrly.yaml`](./examples/pawrly.yaml). Source coverage tracks the roadmap below; start with files (M3) and add sources as their milestones land.

### 7. (Optional) Run as a daemon

For faster CLI invocations, start the local daemon once; subsequent `pawrly sql` invocations auto-discover it over a Unix socket and skip engine warm-up.

```bash
./target/release/pawrly serve --config ./pawrly.yaml &     # background with shell job control
./target/release/pawrly status                              # confirms daemon + sources_ok=1
./target/release/pawrly sql "SELECT COUNT(*) FROM data.orders"
kill %1                                                     # stop the backgrounded daemon
```

Same query, same result — local mode and daemon mode are identical-output by design. Frontends (CLI, MCP, future web UI) all talk the same `EngineService` trait.

---

## What's in this repo

- [`crates/`](./crates) — the Rust workspace.
- [`examples/`](./examples) — reference configurations including the kitchen-sink workspace covering every source kind.
- [`schemas/`](./schemas) — generated JSON Schema for `pawrly.yaml`. Wire this into your editor for completion + validation.

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

Adding a new HTTP source should be a YAML edit + a bundled spec file, not a new Rust crate. See the existing `github` bundle for a reference.

Bug reports, source requests, and design feedback all welcome via GitHub Issues.

---

## License

Apache-2.0.