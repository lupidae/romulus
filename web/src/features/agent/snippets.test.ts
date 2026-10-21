import { describe, expect, it } from "vitest";

import { endpoint, snippet, TOKEN_PLACEHOLDER } from "./snippets";

describe("connection snippets", () => {
  const url = endpoint("https://romulus.example/");

  it("name the endpoint under /api", () => {
    expect(url).toBe("https://romulus.example/api/mcp");
  });

  it("give Claude Code a command that reads the token, never holding it", () => {
    const code = snippet("claude-code", url, "rmls_pat_x").code;
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
      const config = JSON.parse(snippet(client, url, "rmls_pat_x").code) as {
        mcpServers: { romulus: unknown };
      };
      expect(JSON.stringify(config.mcpServers.romulus)).toContain("rmls_pat_x");
      expect(JSON.stringify(config.mcpServers.romulus)).toContain(url);
    }
  });

  it("show a placeholder until a token exists", () => {
    expect(snippet("other", url, TOKEN_PLACEHOLDER).code).toContain(
      "Authorization: Bearer <your token>",
    );
  });
});
