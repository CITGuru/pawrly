import { features } from "@/lib/features";

// Serves /index.md — a markdown representation of the site root, with a
// text/markdown content-type, so a "cold-arrival" agent that lands on the
// homepage from search can fetch one canonical markdown URL instead of HTML.
export const dynamic = "force-static";

const BASE = "https://pawrly.dev";
const REPO = "https://github.com/CITGuru/pawrly";

export function GET() {
  const feats = features
    .map((f) => `- **[${f.label}](${BASE}/features/${f.slug})** — ${f.tagline}`)
    .join("\n");

  const body = `# Pawrly

> Query APIs, files, MCP servers, and databases with SQL.

Pawrly lets teams connect APIs, files, MCP servers, and databases once, then query them with stable table and column names from the CLI, scripts, or agents.

## When to use Pawrly
Use Pawrly when an agent or script needs data from more than one place and you want one SQL question instead of a custom integration per source. It is the read and context path. For a single write to one system, call that system's own tool directly.

## Install
\`\`\`sh
curl -fsSL https://pawrly.dev/install.sh | sh
\`\`\`

Then run your first query:

\`\`\`sh
pawrly sql "SELECT 1 AS hello"
\`\`\`

## Features
${feats}

## For agents & LLMs
- Setup guide (Markdown): ${BASE}/install.md
- Agent skills for reads, sources, saved results, and metrics: ${BASE}/skill.md
- Machine-readable index: ${BASE}/llms.txt
- Full machine-readable reference: ${BASE}/llms-full.txt
- Config JSON Schema: ${BASE}/pawrly.schema.json

## Links
- Documentation: ${BASE}/docs
- Source code: ${REPO}
- Blog: ${BASE}/blog
`;

  return new Response(body, {
    headers: { "Content-Type": "text/markdown; charset=utf-8" },
  });
}
