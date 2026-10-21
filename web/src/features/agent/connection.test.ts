import { afterEach, describe, expect, it, vi } from "vitest";

import { checkConnection } from "./connection";

/** romulus's answers, by JSON-RPC method (tools/call is whoami). */
function stubRomulus(whoami: Record<string, unknown>) {
  const fetch = vi.fn((_url: string, init: { body: string }) => {
    const { id, method } = JSON.parse(init.body) as { id: number; method: string };
    const result =
      method === "tools/list"
        ? { tools: [{ name: "whoami" }, { name: "get_schema" }] }
        : method === "tools/call"
          ? whoami
          : { protocolVersion: "2025-06-18" };
    return Promise.resolve(new Response(JSON.stringify({ jsonrpc: "2.0", id, result })));
  });
  vi.stubGlobal("fetch", fetch);
  return fetch;
}

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("Test connection", () => {
  it("names the person the token acts as, from whoami's structured answer", async () => {
    const fetch = stubRomulus({
      content: [{ type: "text", text: "Alice <alice@romulus.test>, …" }],
      structuredContent: { user: { name: "Alice", email: "alice@romulus.test" } },
      isError: false,
    });
    await expect(checkConnection("https://romulus.example/api/mcp", "rmls_pat_x")).resolves.toEqual(
      { ok: true, name: "Alice", tools: 2 },
    );
    expect(fetch).toHaveBeenCalledTimes(3);
  });

  it("fails when whoami does, instead of saying it connected", async () => {
    stubRomulus({
      content: [{ type: "text", text: "romulus could not say who you are." }],
      isError: true,
    });
    await expect(checkConnection("https://romulus.example/api/mcp", "rmls_pat_x")).resolves.toEqual(
      { ok: false, message: "romulus could not say who you are." },
    );
  });
});
