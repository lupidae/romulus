import { describe, expect, it } from "vitest";

import { endpoint, snippet, TOKEN_PLACEHOLDER, tokenSnippet } from "./snippets";

describe("connection snippets", () => {
  const url = endpoint("https://romulus.example/");

  it("name the endpoint under /api", () => {
    expect(url).toBe("https://romulus.example/api/mcp");
  });

  it("connect by signing in with the endpoint alone, nothing secret", () => {
    expect(snippet("claude-code", url).code).toBe(
      "claude mcp add --transport http romulus https://romulus.example/api/mcp",
    );
    expect(snippet("claude", url).code).toBe(url);
    const cursor = JSON.parse(snippet("cursor", url).code) as {
      mcpServers: { romulus: Record<string, unknown> };
    };
    expect(cursor.mcpServers.romulus).toEqual({ url });
    for (const client of ["claude-code", "claude", "cursor", "other"] as const) {
      expect(snippet(client, url).code).not.toMatch(/Bearer|token/i);
    }
  });

  it("give Claude Code a token command that reads the token, never holding it", () => {
    const code = tokenSnippet("claude-code", url, "rmls_pat_x").code;
    expect(code).toBe(
      [
        "read -rs ROMULUS_TOKEN",
        'claude mcp add --transport http romulus https://romulus.example/api/mcp --header "Authorization: Bearer $ROMULUS_TOKEN"',
      ].join("\n"),
    );
    expect(code).not.toContain("rmls_pat_x");
  });

  it("give JSON clients valid JSON holding the URL and the token", () => {
    for (const client of ["claude-desktop", "cursor"] as const) {
      const config = JSON.parse(tokenSnippet(client, url, "rmls_pat_x").code) as {
        mcpServers: { romulus: unknown };
      };
      expect(JSON.stringify(config.mcpServers.romulus)).toContain("rmls_pat_x");
      expect(JSON.stringify(config.mcpServers.romulus)).toContain(url);
    }
  });

  it("show a placeholder until a token exists", () => {
    expect(tokenSnippet("other", url, TOKEN_PLACEHOLDER).code).toContain(
      "Authorization: Bearer <your token>",
    );
  });
});
