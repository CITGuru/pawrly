// The prompt a user copies and pastes into their coding agent so it installs and
// sets up Pawrly for them. It leans on the agent-facing guide at /install.md as
// the source of truth, and adapts to whether the target agent can run a shell.

export type AgentId =
  | "any"
  | "claude-code"
  | "cursor"
  | "codex"
  | "windsurf"
  | "claude-desktop";

export const AGENTS: { id: AgentId; label: string; shell: boolean }[] = [
  { id: "any", label: "Any agent", shell: true },
  { id: "claude-code", label: "Claude Code", shell: true },
  { id: "cursor", label: "Cursor", shell: true },
  { id: "codex", label: "Codex", shell: true },
  { id: "windsurf", label: "Windsurf", shell: true },
  { id: "claude-desktop", label: "Claude Desktop", shell: false },
];

export function buildAgentPrompt(id: AgentId): string {
  const agent = AGENTS.find((a) => a.id === id) ?? AGENTS[0];
  const lines: string[] = [];

  if (agent.id !== "any") {
    lines.push(`You are running inside ${agent.label}.`, "");
  }

  lines.push(
    "Install and set up Pawrly in this project.",
    "",
    "Pawrly is one SQL interface over APIs, files, databases, warehouses, and MCP servers. Its install + quickstart guide for agents is at https://pawrly.dev/install.md — read that first, then do the following:",
    ""
  );

  if (agent.shell) {
    lines.push(
      "1. Detect my OS and install the pawrly binary yourself by running the right command:",
      "   - macOS/Linux:          curl -fsSL https://pawrly.dev/install.sh | sh",
      "   - Windows (PowerShell):  irm https://pawrly.dev/install.ps1 | iex"
    );
  } else {
    lines.push(
      "1. You can't run shell commands, so give me the exact command to run myself and wait for me:",
      "   - macOS/Linux:          curl -fsSL https://pawrly.dev/install.sh | sh",
      "   - Windows (PowerShell):  irm https://pawrly.dev/install.ps1 | iex"
    );
  }

  lines.push(
    '2. Verify it works:  pawrly sql "SELECT 1 AS hello"',
    "3. Create a starter pawrly.yaml in the project root (following the guide) with one commented local-file source and one commented HTTP API source, so I can see the shape and fill in my own.",
    "4. If I use an MCP client, show me how to expose this workspace over MCP:  pawrly mcp-stdio --config ./pawrly.yaml",
    "5. Summarize what you installed and changed, and list any API keys or environment variables I still need to set.",
    "",
    "Never hard-code secrets — use ${secret:NAME} references in pawrly.yaml and read them from the environment."
  );

  return lines.join("\n");
}
