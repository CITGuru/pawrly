import fs from "node:fs";
import path from "node:path";
import { getPosts } from "@/lib/posts";
import { features } from "@/lib/features";

// Serves /llms-full.txt — an agent quick reference with install steps, source
// types, features, writing, and links in one fetch.
export const dynamic = "force-static";

const BASE = "https://pawrly.dev";
const REPO = "https://github.com/CITGuru/pawrly";

function installGuide(): string {
  try {
    return fs
      .readFileSync(path.join(process.cwd(), "public", "install.md"), "utf8")
      .trim();
  } catch {
    return `See ${BASE}/install.md for the full install + quickstart guide.`;
  }
}

export function GET() {
  const featureBlocks = features
    .map((f) => `### ${f.title}\n${f.description}\nMore: ${BASE}/features/${f.slug}`)
    .join("\n\n");

  const blog = getPosts()
    .map((p) => `### ${p.title}\n${p.excerpt}\nRead: ${BASE}/blog/${p.slug}`)
    .join("\n\n");

  const body = `# Pawrly — agent quick reference

> Query APIs, files, MCP servers, and databases with SQL. Describe each source once in pawrly.yaml, then query it with stable table and column names from the CLI, a local service, or any MCP client.

## When to use Pawrly
Use Pawrly when an agent or script needs data from more than one place — REST/GraphQL APIs, files (Parquet/CSV/JSON), object storage, MCP servers, databases, or warehouses — and you want one SQL question instead of a custom integration per source. It is the read and context path: reach for it before deciding or acting. For a single write to one system (create a ticket, send a message), call that system's own tool directly.

## Sources Pawrly can query
- HTTP APIs — any REST or GraphQL endpoint; point at an OpenAPI 3.0 spec to get one table per GET.
- Files & object storage — Parquet, CSV, JSON on local disk or S3 / GCS / Azure.
- MCP servers — query tools from Linear, GitHub, Notion, internal systems, or other MCP servers as tables.
- Databases — Postgres, MySQL, SQLite, DuckDB.
- Warehouses & lakehouses — Snowflake, Iceberg, Delta, DuckLake.

---

${installGuide()}

---

## Features
${featureBlocks}

## Writing
${blog}

## Reference
- Documentation / quickstart: ${REPO}#quickstart
- Sources reference: ${REPO}/blob/main/docs/sources.md
- MCP guide: ${REPO}/blob/main/docs/mcp.md
- Semantic layer: ${REPO}/blob/main/docs/semantic.md
- Functions: ${REPO}/blob/main/docs/functions.md
- JSON Schema for pawrly.yaml: ${BASE}/pawrly.schema.json
- Agent skills (Claude Code / Codex plugin): ${BASE}/skill.md
- Machine-readable index: ${BASE}/llms.txt
- Source code: ${REPO}
`;

  return new Response(body, {
    headers: { "Content-Type": "text/plain; charset=utf-8" },
  });
}
