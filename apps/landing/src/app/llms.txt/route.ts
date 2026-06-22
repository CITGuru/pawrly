import { getPosts } from "@/lib/posts";

// Serves /llms.txt — the llmstxt.org convention: a concise, link-rich map of the
// project for LLMs and agents. Generated so the blog list stays in sync.
export const dynamic = "force-static";

const BASE = "https://pawrly.dev";
const REPO = "https://github.com/CITGuru/pawrly";

export function GET() {
  const posts = getPosts();

  const blog = posts
    .map((p) => `- [${p.title}](${BASE}/blog/${p.slug}): ${p.excerpt}`)
    .join("\n");

  const body = `# Pawrly

> Query APIs, files, MCP servers, and databases with SQL.

Pawrly lets a team describe each source once in pawrly.yaml, then query APIs, files, MCP servers, and databases with stable table and column names. It works from the CLI, can run as a local service, and can expose the same workspace to MCP clients such as Claude Desktop, Cursor, and Codex.

## When to use Pawrly
Use Pawrly when an agent or script needs data from more than one place — REST/GraphQL APIs, files (Parquet/CSV/JSON), object storage, MCP servers, databases, or warehouses — and you want one SQL question instead of a custom integration per source. It is the read and context path: reach for it before deciding or acting. For a single write to one system (create a ticket, send a message), call that system's own tool directly.

## Agent skills
Pawrly ships a Claude Code + Codex plugin with skills for reading data, connecting sources, saving useful results, and defining approved metrics. See [skill.md](${BASE}/skill.md).

## Install
- [Install guide for LLMs (Markdown)](${BASE}/install.md): the fastest path from nothing to a working binary and a first query, copy-pasteable.
- [install.sh](${BASE}/install.sh): macOS/Linux installer — \`curl -fsSL https://pawrly.dev/install.sh | sh\`.
- [install.ps1](${BASE}/install.ps1): Windows PowerShell installer — \`irm https://pawrly.dev/install.ps1 | iex\`.
- [pawrly.schema.json](${BASE}/pawrly.schema.json): JSON Schema for pawrly.yaml — reference it with \`# yaml-language-server: $schema=https://pawrly.dev/pawrly.schema.json\` for editor completion + validation.

## Documentation
- [Docs home](${BASE}/docs): all guides and reference, with a copyable markdown view per page.
- [Docs index for LLMs](${BASE}/docs/llms.txt): every doc page linked as raw Markdown (.md).
- [Overview](${BASE}/docs/overview): what Pawrly is and how the pieces fit.
- [CLI](${BASE}/docs/cli): sql, schema, validate, serve, status, mcp-stdio.
- [Configuration](${BASE}/docs/config): the pawrly.yaml schema, secrets, and caching.
- [Sources reference](${BASE}/docs/sources): configure APIs, files, MCP servers, databases, warehouses, and lakehouses.
- [Semantic layer](${BASE}/docs/semantic): approved fields, metrics, joins, access rules, and required filters.
- [Materialized tables](${BASE}/docs/materialize): save a query result, file, or URL as a reusable table.
- [MCP](${BASE}/docs/mcp): run Pawrly as an MCP server and consume other MCP servers as sources.
- [Observability](${BASE}/docs/observability): metrics and query auditing.
- [Architecture](${BASE}/docs/architecture): implementation details and local vs daemon behavior.

## Writing
${blog}

## Optional
- [GitHub repository](${REPO}): source, issues, and releases.
- [Example configurations](${REPO}/tree/main/examples): a kitchen-sink workspace covering every source kind.
`;

  return new Response(body, {
    headers: { "Content-Type": "text/plain; charset=utf-8" },
  });
}
