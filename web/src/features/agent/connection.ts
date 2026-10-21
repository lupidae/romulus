// "Test connection": what an agent does first, from this browser. It proves
// the endpoint answers through whatever sits in front of romulus, and that
// the token works.

export type ConnectionCheck =
  { ok: true; name: string; tools: number } | { ok: false; message: string };

const headers = (token: string) => ({
  Authorization: `Bearer ${token}`,
  "Content-Type": "application/json",
  Accept: "application/json, text/event-stream",
});

interface RpcAnswer {
  result?: Record<string, unknown>;
  error?: { message?: string };
}

async function rpc(
  url: string,
  token: string,
  id: number,
  method: string,
  params: unknown,
): Promise<RpcAnswer> {
  // Resolved per call, so test stubs are seen.
  const response = await globalThis.fetch(url, {
    method: "POST",
    headers: headers(token),
    body: JSON.stringify({ jsonrpc: "2.0", id, method, params }),
  });
  const body = (await response.json().catch(() => ({}))) as RpcAnswer;
  if (!response.ok && !body.error) {
    throw new Error(`romulus answered ${response.status}.`);
  }
  if (body.error) throw new Error(body.error.message ?? "romulus refused the request.");
  return body;
}

/** Initializes as an MCP client would, lists the tools, asks who it is. */
export async function checkConnection(url: string, token: string): Promise<ConnectionCheck> {
  try {
    await rpc(url, token, 1, "initialize", {
      protocolVersion: "2025-06-18",
      capabilities: {},
      clientInfo: { name: "romulus-web (Test connection)", version: "1" },
    });
    const listed = await rpc(url, token, 2, "tools/list", {});
    const tools = Array.isArray(listed.result?.tools) ? listed.result.tools.length : 0;
    const who = await rpc(url, token, 3, "tools/call", { name: "whoami", arguments: {} });
    const user = (who.result?.structuredContent as { user?: { name?: string } } | undefined)?.user;
    return { ok: true, name: user?.name ?? "you", tools };
  } catch (error) {
    return {
      ok: false,
      message: error instanceof Error ? error.message : "romulus could not be reached.",
    };
  }
}
