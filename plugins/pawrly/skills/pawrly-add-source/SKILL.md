---
name: pawrly-add-source
description: Author or repair a Pawrly source in pawrly.yaml so its data becomes queryable SQL tables — local/object-store files, REST/GraphQL & OpenAPI HTTP APIs, Postgres/MySQL/SQLite/DuckDB, Snowflake, Iceberg/Delta/DuckLake, or another MCP server. Use when adding, connecting, editing, or troubleshooting a source, wiring credentials via secrets, or setting caching and safety guards. After it resolves, query through the `pawrly` skill.
version: 0.0.1
---

# Pawrly: add a source

Use this skill to author or fix a Pawrly **source** — a named connection exposed as SQL tables addressed `<source>.<table>` (the source `name` is the schema prefix). Sources live under `sources:` in `pawrly.yaml`.

## Goal

A valid, queryable source that passes:

```bash
pawrly validate                 # static check; reports every problem at once
pawrly check --source <name>    # runs the source's examples: as live probes
pawrly schema <name>.<table>    # confirm columns + types
```

## Workflow

1. **Read the upstream first** — the API docs, the file layout, or the database catalog.
2. **Start small.** One source, one or two tables, a few columns. Expand after it queries.
3. **Pick the kind** and fill the source block (see *The source block* below and [references/source-backends.md](references/source-backends.md) for per-kind config).
4. **Wire credentials as secrets** — never inline them. Use `${secret:NAME}` and declare a backend under `secrets:`.
5. **Add `examples:`** — one or two known-good SQL probes. They are health checks for `pawrly check` and hints returned to agents by `describe_table`.
6. **Validate, then probe live**, then inspect the shape with `pawrly schema`.
7. **Iterate** until columns, filters, and pagination are right. Add `wiki:`, `cache:`, `safety:` as needed.

Add the schema header so editors validate as you type:
```yaml
# yaml-language-server: $schema=./schemas/pawrly.schema.json
version: 1
```

> **Strict keys.** A typo'd or misplaced top-level field fails the load — it is not ignored. Kind-specific keys go under `config:` (source-level) or **flat** under a `tables:` entry, nowhere else.

## The source block

```yaml
sources:
  - name: data            # required; SQL schema prefix; valid SQL identifier; unique
    kind: file            # required; closed enum, case-insensitive
    description: "..."     # optional one-liner; shown in `pawrly source list`
    wiki: |               # optional agent-facing notes; surfaced by describe_table
      Which filters to set, id quirks, how to decode a column.
    examples:             # optional; live probes + agent hints
      - SELECT COUNT(*) FROM data.orders
    config: { ... }       # per-kind connection/auth/paths — see references/source-backends.md
    tables: [ ... ]       # explicit table declarations (required for some kinds)
    cache: { mode: ttl, ttl: 10m }   # optional; per-source caching
    safety: { max_rows: 1000000 }    # optional; guard rails
```

Only `name`, `description`, `wiki`, `cache`, `safety` are common across kinds; everything else is kind-specific. Split a large workspace with top-level `include: ["./sources/*.yaml"]`.

## Kind selector

| Want to query… | `kind` | `tables:` needed? |
|---|---|---|
| local Parquet/CSV/JSON | `file` | optional (globs auto-discover) |
| files in S3/GCS/Azure | `file` + `storage:` | **required** |
| a REST/GraphQL API | `http` | required (or `config.type: openapi` to synthesize) |
| another MCP server's tools | `mcp` | optional (tools auto-expose) |
| Postgres / MySQL | `postgres`/`pg`/`postgresql`, `mysql` | no — live catalog |
| SQLite / local DuckDB | `sqlite`, `duckdb` | optional |
| Snowflake | `snowflake` | no — live catalog |
| Iceberg / Delta tables | `iceberg`, `delta`/`deltalake` | **required** |
| a DuckLake catalog | `ducklake` | no — live catalog |

Per-kind `config:`/`tables:` keys and recipes live in [references/source-backends.md](references/source-backends.md). Three backends have a dedicated deep-dive page: [references/http-backend.md](references/http-backend.md) (request/response shaping, auth styles, pagination, computed columns), [references/openapi.md](references/openapi.md) (synthesize tables from an OpenAPI spec), and [references/mcp-backend.md](references/mcp-backend.md) (expose another MCP server's tools as tables).

## Secrets

```yaml
secrets:
  - kind: file            # a gitignored .env beside the config
    path: .env
    format: auto          # auto | yaml | dotenv
  - kind: env             # fall back to process env
  - kind: keyring
    service: pawrly
```
Reference anywhere a `config:` string appears: `${secret:NAME}` (also `${env:NAME}`, `${file:PATH}`). Name secrets with a service prefix (`GITHUB_TOKEN`, not `TOKEN`).

## Caching & safety (optional)

```yaml
cache:
  mode: ttl        # none | ttl(ttl:) | refresh(every:) | cron(cron:) | append(cursor_column:)
  ttl: 10m
safety:
  require_filters_on: [order_date]    # error unless each is filtered
  require_at_least_one_filter: true   # refuse a full-table scan
  max_rows: 1000000
  max_pages: 50                       # cap http pagination
  timeout: 30s
  required_predicates:                # AND-ed into every scan; ${param:NAME} = RLS
    - "tenant_id = ${param:tenant_id}"
```
Mark a filter required only when the upstream truly requires it.

## Authoring rules

- Prefer `snake_case`, SQL-friendly, stable table names, unique within the source.
- For attach-style kinds (`postgres`/`mysql`/`duckdb`/`snowflake`/`ducklake`), `tables:` entries do **not** rename or restrict the live catalog — use a semantic model for curated views (see **pawrly-semantic-model**).
- Verify pagination by fetching real rows across pages, not `COUNT(*)`.
- Keep `description` capability-first ("orders, customers, refunds…"); put setup/scope detail in `wiki:` or secret naming, not the description.

## Managing sources from the CLI

```bash
pawrly source list                    # list sources, annotated with their file
pawrly source add --name gh --kind http --url https://api.github.com --token ... --set k=v
pawrly source refresh <name>          # re-discover tables
pawrly source test <name>             # reachability check
pawrly source remove <name>
```

## Deliverable

Report: the config path edited, the kind chosen, `validate`/`check`/`schema` output, any assumptions, and any endpoint left unverified (e.g. needed live credentials).
