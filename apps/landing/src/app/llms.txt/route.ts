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

> Query APIs, files, MCP tools, and databases with SQL.

Pawrly lets a team describe each source once in pawrly.yaml, then query APIs, files, MCP tools, and databases with stable table and column names. It works from the CLI, can run as a local service, and can expose the same workspace to MCP clients such as Claude Desktop, Cursor, and Codex. Use it when an agent or script needs data from more than one system without writing a custom integration for each one. Open source under Apache-2.0.

## Install
- [Install guide for LLMs (Markdown)](${BASE}/install.md): the fastest path from nothing to a working binary and a first query, copy-pasteable.
- [install.sh](${BASE}/install.sh): macOS/Linux installer — \`curl -fsSL https://pawrly.dev/install.sh | sh\`.
- [install.ps1](${BASE}/install.ps1): Windows PowerShell installer — \`irm https://pawrly.dev/install.ps1 | iex\`.
- [pawrly.schema.json](${BASE}/pawrly.schema.json): JSON Schema for pawrly.yaml — reference it with \`# yaml-language-server: $schema=https://pawrly.dev/pawrly.schema.json\` for editor completion + validation.

## Documentation
- [Quickstart](${REPO}#quickstart): install, first query over files, first query over live APIs.
- [Overview](${REPO}/blob/main/docs/overview.md): what Pawrly is and how the pieces fit.
- [Sources reference](${REPO}/blob/main/docs/sources.md): configure APIs, files, MCP servers, databases, warehouses, and lakehouses.
- [Configuration](${REPO}/blob/main/docs/config.md): the pawrly.yaml schema, secrets, and caching.
- [MCP](${REPO}/blob/main/docs/mcp.md): run Pawrly as an MCP server and consume other MCP servers as sources.
- [Semantic layer](${REPO}/blob/main/docs/semantic.md): approved fields, metrics, joins, access rules, and required filters.
- [Materialize](${REPO}/blob/main/docs/materialize.md): pin a query result, file, or URL as a self-backed table.
- [CLI](${REPO}/blob/main/docs/cli.md): sql, schema, validate, serve, status, mcp-stdio.
- [Architecture](${REPO}/blob/main/docs/architecture.md): engine internals and local vs daemon behavior.
- [Observability](${REPO}/blob/main/docs/observability.md): metrics and query auditing.

## Writing
${blog}

## Optional
- [GitHub repository](${REPO}): source, issues, and releases (Apache-2.0).
- [Example configurations](${REPO}/tree/main/examples): a kitchen-sink workspace covering every source kind.
`;

  return new Response(body, {
    headers: { "Content-Type": "text/plain; charset=utf-8" },
  });
}
