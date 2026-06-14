# Pawrly plugin

Agent skills that make it easy for AI agents to use [Pawrly](https://github.com/CITGuru/pawrly) — query federated data, persist results, and author sources and semantic models. The plugin bundles the Pawrly MCP server (`pawrly mcp-stdio`) so an agent can read your workspace the moment it's installed. It ships manifests for both Claude Code (`.claude-plugin/`) and Codex (`.codex-plugin/`).

## Prerequisites

The `pawrly` binary must be on the agent's `PATH`, and a `pawrly.yaml` must be discoverable (via `PAWRLY_CONFIG`/`--config` or the working directory). Verify with `pawrly version`.

## Skills

| Skill | Use it to… |
|---|---|
| **pawrly** | Entrypoint. Discover the catalog and query live data (semantic or raw SQL) via MCP. |
| **pawrly-add-source** | Add or fix a source in `pawrly.yaml` (files, APIs, databases, lakehouses, MCP). |
| **pawrly-materialize** | Pin a query/file/URL as a `materialized.<name>` table. |
| **pawrly-semantic-model** | Author or update semantic models (dimensions, measures, RLS, pre-aggs). |

Reads go through the MCP tools (`list_sources`, `search_tables`, `describe_table`, `query`, `semantic_query`, `materialize`, …). Authoring sources and models is a config-file + CLI task (`pawrly validate` / `pawrly check`).

## Layout

```
plugins/pawrly/
├── .claude-plugin/
│   ├── plugin.json              # Claude Code plugin manifest
│   └── marketplace.json         # Claude Code marketplace entry (source: "./")
├── .codex-plugin/
│   ├── plugin.json              # Codex plugin manifest
│   └── marketplace.json         # Codex marketplace entry (source.path: "./" + policy)
├── mcp.json                     # bundles the Pawrly MCP server
├── README.md
└── skills/
    ├── pawrly/                  # entrypoint: discover + query
    │   └── SKILL.md
    ├── pawrly-add-source/       # SKILL.md + references/
    │   ├── SKILL.md
    │   └── references/
    │       ├── source-backends.md
    │       ├── http-backend.md
    │       ├── openapi.md
    │       └── mcp-backend.md
    ├── pawrly-materialize/
    │   └── SKILL.md
    └── pawrly-semantic-model/
        └── SKILL.md
```

See [docs/](https://github.com/CITGuru/pawrly/tree/main/docs) for the full reference behind each skill.
