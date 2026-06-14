---
name: pawrly-materialize
description: Persist a Pawrly query result, a local file, or a remote URL as a pinned, self-backed table queryable as materialized.<name>. Use to cache an expensive or slow query, snapshot data at a point in time, load an external file as a table or manage/drop existing materialized tables. For ad-hoc reads use the `pawrly` skill instead; reach here when the result must persist and be reused.
version: 0.0.1
---

# Pawrly: materialize tables

A **materialized table** persists data as a named, self-backed table queryable as `materialized.<name>`. Unlike a cache entry it is **pinned** — never auto-evicted — until you drop it. Create-or-replace by name. Full reference: [docs/materialize.md](https://github.com/CITGuru/pawrly/blob/main/docs/materialize.md).

## When to use it

- A query is expensive or slow and its result is reused across questions.
- You need a **snapshot** — freeze a result or a moving API response at a point in time.
- You want to load a local file or a remote URL as a first-class queryable table.

For one-off reads, don't materialize — just `query` (see the `pawrly` skill).

## Three origins — provide exactly one

### Over MCP (preferred for agents)

`materialize` → `{ name, file_path, row_count, size_bytes }`:

```json
{ "name": "top_customers", "sql": "SELECT customer, SUM(amount) AS total FROM stripe.charges GROUP BY 1 ORDER BY 2 DESC LIMIT 100" }
{ "name": "sales",  "file": "./data/sales.csv" }
{ "name": "prices", "url":  "https://example.com/prices.parquet" }
```

- Exactly one of `sql` | `file` | `url`.
- Optional `format`: `parquet` (default for `sql`) | `csv` | `json` — inferred from the extension for `file`/`url`.
- Optional `params`: substitutes `${param:KEY}` placeholders in the `sql`.

Then query it like any table:
```json
{ "sql": "SELECT * FROM materialized.top_customers WHERE total > 1000" }
```

Drop it with `drop_materialized` → `{ dropped }`:
```json
{ "name": "top_customers" }
```

### Over CLI (operators)

```bash
pawrly materialize top_customers \
  "SELECT customer, SUM(amount) AS total FROM stripe.charges GROUP BY 1 ORDER BY 2 DESC LIMIT 10"
pawrly materialize sales  --file ./data/sales.csv --format csv
pawrly materialize prices --url https://example.com/prices.parquet
pawrly sql "SELECT * FROM materialized.top_customers"

pawrly materialize sales --drop                       # drop
pawrly cache refresh materialized.top_customers       # re-run the origin (re-query / re-read)
```
`--param KEY=VALUE` (repeatable) substitutes `${param:KEY}` in the SQL.

## Refresh semantics

A materialized table is a point-in-time snapshot; it does **not** auto-update. To re-run its origin (re-execute the query, or re-read the file/URL), refresh it explicitly: `pawrly cache refresh materialized.<name>`. If you need data that stays fresh automatically, prefer a source `cache:` mode (`ttl`/`refresh`/`cron`/`append`) on the underlying table instead — see **pawrly-add-source**.

## Rules

- Name materialized tables in `snake_case`; the name is the identifier under `materialized.<name>`.
- Bound the origin query with a `LIMIT`/aggregation — you're persisting the full result to disk.
- Re-materializing an existing name **replaces** it; there's no implicit versioning.
- Report back the `name`, `row_count`, and `size_bytes` (or `file_path`) so the user knows what was pinned and how big it is.
