// What a person pastes into their agent to connect it to romulus's MCP
// endpoint (docs/mcp.md), with the endpoint and their token filled in. A
// terminal command never holds the token itself (shell history keeps
// commands): Claude Code's reads it first, unseen.

/** Shown in place of a token not created yet. */
export const TOKEN_PLACEHOLDER = "<your token>";

export type AgentClient = "claude-code" | "claude-desktop" | "cursor" | "other";

export interface Snippet {
  /** Where it goes, in a sentence. */
  where: string;
  code: string;
  label: string;
}

/** The MCP endpoint of the romulus at `origin`. */
export function endpoint(origin: string): string {
  return `${origin.replace(/\/+$/, "")}/api/mcp`;
}

export function snippet(client: AgentClient, url: string, token: string): Snippet {
  switch (client) {
    case "claude-code":
      return {
        where:
          "Run both lines in a terminal and paste the token when the first waits: it asks without showing it, so the token never lands in your shell history. --scope user adds romulus to every project.",
        label: "Claude Code command",
        code: [
          "read -rs ROMULUS_TOKEN",
          `claude mcp add --transport http romulus ${url} --header "Authorization: Bearer $ROMULUS_TOKEN"`,
        ].join("\n"),
      };
    case "claude-desktop":
      return {
        where:
          "In Claude Desktop: Settings, Developer, Edit config (claude_desktop_config.json), then restart it. Needs Node.js.",
        label: "Claude Desktop configuration",
        code: json({
          mcpServers: {
            romulus: {
              command: "npx",
              args: ["-y", "mcp-remote", url, "--header", "Authorization:${ROMULUS_AUTH}"],
              env: { ROMULUS_AUTH: `Bearer ${token}` },
            },
          },
        }),
      };
    case "cursor":
      return {
        where: "In ~/.cursor/mcp.json (every project) or .cursor/mcp.json (this one).",
        label: "Cursor configuration",
        code: json({
          mcpServers: {
            romulus: { url, headers: { Authorization: `Bearer ${token}` } },
          },
        }),
      };
    case "other":
      return {
        where:
          "Any client of remote MCP servers (Streamable HTTP): VS Code, Windsurf, Zed, your own.",
        label: "Connection settings",
        code: [
          `URL:        ${url}`,
          "Transport:  Streamable HTTP",
          `Header:     Authorization: Bearer ${token}`,
        ].join("\n"),
      };
  }
}

function json(value: unknown): string {
  return JSON.stringify(value, null, 2);
}
