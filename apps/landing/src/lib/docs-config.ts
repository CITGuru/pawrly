// Pure data — safe to import from client components (no node:fs / shiki here).
// The server-only rendering lives in ./docs.ts.

export type DocMeta = { slug: string; title: string; blurb: string };
export type DocGroup = { heading: string; items: DocMeta[] };
export type TocEntry = { id: string; text: string; depth: 2 | 3 };

// Curated order + grouping for the sidebar. Slugs match docs-content/<slug>.md
// (synced from the repo's docs/ by scripts/sync-install.mjs).
export const docGroups: DocGroup[] = [
  {
    heading: "Start here",
    items: [
      { slug: "overview", title: "Overview", blurb: "What Pawrly is and how the pieces fit together." },
      { slug: "cli", title: "CLI", blurb: "Every command: sql, schema, validate, serve, status, mcp-stdio." },
      { slug: "config", title: "Configuration", blurb: "The pawrly.yaml schema — sources, secrets, caching, safety." },
    ],
  },
  {
    heading: "Connect data",
    items: [
      { slug: "sources", title: "Sources", blurb: "Wire up APIs, files, databases, warehouses, lakehouses, and MCP servers." },
    ],
  },
  {
    heading: "Model & serve",
    items: [
      { slug: "semantic", title: "Semantic layer", blurb: "Dimensions, measures, relationships, segments, RLS, and pre-aggregations." },
      { slug: "functions", title: "Functions", blurb: "Reusable, table-valued functions called as FROM ns.fn(args) — builtin or declared over http, mcp, and files." },
      { slug: "materialize", title: "Materialized tables", blurb: "Pin a query, file, or URL as a self-backed table." },
      { slug: "api", title: "REST API", blurb: "Query and manage a workspace over JSON-over-HTTP, with an OpenAPI spec." },
    ],
  },
  {
    heading: "For agents",
    items: [
      { slug: "mcp", title: "MCP server", blurb: "Run Pawrly as an MCP server and consume other MCP servers as sources." },
    ],
  },
  {
    heading: "Operate",
    items: [
      { slug: "observability", title: "Observability", blurb: "Traces, metrics, and a queryable activity log." },
      { slug: "architecture", title: "Architecture", blurb: "Engine internals and local vs daemon behavior." },
      { slug: "console", title: "Console", blurb: "The web console for browsing sources and running queries." },
    ],
  },
];

export const docList: DocMeta[] = docGroups.flatMap((g) => g.items);
export const validSlugs = new Set(docList.map((d) => d.slug));
