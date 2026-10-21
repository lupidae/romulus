// What a person pastes into their agent to connect it to romulus's MCP
// endpoint (docs/mcp.md). By default nothing secret: the agent signs in
// through the browser (OAuth) and the person approves it here. With an
// access token (the Advanced way: CI, agents without a browser), the
// configurations hold it, and a terminal command never does (shell history
// keeps commands): Claude Code's reads it first, unseen.

/** Shown in place of a token not created yet. */
export const TOKEN_PLACEHOLDER = "<your token>";

export type AgentClient = "claude-code" | "claude" | "cursor" | "other";

/** The clients a token can be pasted into (claude.ai has no header to fill). */
export type TokenClient = "claude-code" | "claude-desktop" | "cursor" | "other";

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

/** Connecting by signing in: the endpoint is all the agent needs. */
export function snippet(client: AgentClient, url: string): Snippet {
  switch (client) {
    case "claude-code":
      return {
        where:
          "Run it in a terminal. Then, in Claude Code, type /mcp, choose romulus and Authenticate.",
        label: "Claude Code command",
        code: `claude mcp add --transport http romulus ${url}`,
      };
    case "claude":
      return {
        where:
          "On claude.ai or in Claude Desktop: Settings, Connectors, Add custom connector, this URL, Connect. Claude connects from Anthropic's servers: this romulus must be reachable from the internet.",
        label: "Connector URL",
        code: url,
      };
    case "cursor":
      return {
        where:
          "In ~/.cursor/mcp.json, or .cursor/mcp.json for one project. Then click romulus's login in Cursor's MCP settings.",
        label: "Cursor configuration",
        code: json({ mcpServers: { romulus: { url } } }),
      };
    case "other":
      return {
        where:
          "Any client of remote MCP servers: VS Code, Windsurf, Zed, your own. It finds how to sign in from the URL.",
        label: "Connection settings",
        code: [
          `URL:        ${url}`,
          "Transport:  Streamable HTTP",
          "Sign-in:    OAuth, in your browser",
        ].join("\n"),
      };
  }
}

/** Connecting with an access token, for agents that cannot open a browser. */
export function tokenSnippet(client: TokenClient, url: string, token: string): Snippet {
  switch (client) {
    case "claude-code":
      return {
        where:
          "Run both lines in a terminal and paste the token when the first waits: it asks without showing it, so the token never lands in your shell history. --scope user adds romulus to every project.",
        label: "Claude Code command with a token",
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
        label: "Cursor configuration with a token",
        code: json({
          mcpServers: {
            romulus: { url, headers: { Authorization: `Bearer ${token}` } },
          },
        }),
      };
    case "other":
      return {
        where:
          "Any client of remote MCP servers (Streamable HTTP) that takes a header, or a script.",
        label: "Connection settings with a token",
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
