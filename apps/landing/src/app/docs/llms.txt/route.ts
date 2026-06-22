import { docGroups } from "@/lib/docs-config";

// Docs-scoped llms.txt — the same grouping as the docs nav, but every link
// points at the raw Markdown (.md) of each page so an agent can fetch sources
// directly. Static route; takes precedence over /docs/[slug].
export const dynamic = "force-static";

const BASE = "https://pawrly.dev";

export function GET() {
  const sections = docGroups
    .map((g) => {
      const items = g.items
        .map((it) => `- [${it.title}](${BASE}/docs/${it.slug}.md): ${it.blurb}`)
        .join("\n");
      return `## ${g.heading}\n${items}`;
    })
    .join("\n\n");

  const body = `# Pawrly documentation

> Query APIs, files, MCP servers, and databases with SQL — one governed SQL surface for agents and data teams.

This is the documentation index for LLMs. Every link points to the raw Markdown source of a page (the .md endpoints); their internal links also resolve to .md, so you can crawl the docs as Markdown. The human-readable docs live at ${BASE}/docs.

${sections}

## More
- [Install guide](${BASE}/install.md): install + first query, copy-pasteable.
- [Full machine-readable reference](${BASE}/llms-full.txt): overview, install, sources, features in one fetch.
- [Site index](${BASE}/llms.txt): the whole site, not just docs.
- [JSON Schema for pawrly.yaml](${BASE}/pawrly.schema.json): editor completion + validation.
`;

  return new Response(body, {
    headers: { "Content-Type": "text/plain; charset=utf-8" },
  });
}
