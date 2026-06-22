// Serves /skill.md — a markdown pointer to Pawrly's agent skills. Pawrly ships a
// Claude Code + Codex plugin (plugins/pawrly/) bundling these skills and the
// Pawrly MCP server, so an agent can discover and load them.
export const dynamic = "force-static";

const BASE = "https://pawrly.dev";
const REPO = "https://github.com/CITGuru/pawrly";
const PLUGIN = `${REPO}/tree/main/plugins/pawrly`;

const SKILLS = [
  {
    name: "pawrly",
    use: "Entrypoint for reads. Find sources, inspect tables, run SQL, ask approved business questions, and save useful results for later.",
    path: "skills/pawrly/SKILL.md",
  },
  {
    name: "pawrly-add-source",
    use: "Connect a new API, file, MCP server, database, warehouse, or lakehouse by adding it to pawrly.yaml.",
    path: "skills/pawrly-add-source/SKILL.md",
  },
  {
    name: "pawrly-semantic-model",
    use: "Define approved business metrics, fields, joins, and required filters so agents do not invent business logic.",
    path: "skills/pawrly-semantic-model/SKILL.md",
  },
  {
    name: "pawrly-materialize",
    use: "Save a query result, local file, or remote URL as a table the agent can reuse later.",
    path: "skills/pawrly-materialize/SKILL.md",
  },
];

export function GET() {
  const list = SKILLS.map(
    (s) => `### ${s.name}\n${s.use}\n\nSKILL.md: ${PLUGIN}/${s.path}`
  ).join("\n\n");

  const body = `# Pawrly agent skills

> Agent skills for using Pawrly: inspect available data, run SQL, connect new sources, save useful results, and define approved business metrics.

Pawrly ships a plugin for Claude Code and Codex that bundles these skills plus the Pawrly MCP server (\`pawrly mcp-stdio\`), so an agent can read your workspace the moment it's installed.

## Prerequisites
The \`pawrly\` binary must be on the agent's PATH, and a \`pawrly.yaml\` must be discoverable (via \`PAWRLY_CONFIG\` / \`--config\` or the working directory). Install the binary with \`curl -fsSL ${BASE}/install.sh | sh\` — see ${BASE}/install.md.

## Skills
${list}

The \`pawrly\` skill is the entrypoint for read tasks. It can list sources, search tables, describe columns, run SQL, ask approved metric questions, and save results. The other skills help edit \`pawrly.yaml\` when a source or business definition is missing.

## Install the plugin
The plugin and its manifests live in the repository — add it as a plugin to load all four skills and the bundled MCP server:
- Plugin directory: ${PLUGIN}
- Claude Code manifest: ${PLUGIN}/.claude-plugin/
- Codex manifest: ${PLUGIN}/.codex-plugin/

## Links
- Repository: ${REPO}
- Install guide: ${BASE}/install.md
- Machine-readable index: ${BASE}/llms.txt
`;

  return new Response(body, {
    headers: { "Content-Type": "text/markdown; charset=utf-8" },
  });
}
