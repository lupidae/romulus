import { afterEach, describe, expect, it, vi } from "vitest";

import { api, ApiError, unwrap } from "./client";

function respond(status: number, body: unknown) {
  return vi.fn((_input: Request) =>
    Promise.resolve(
      new Response(JSON.stringify(body), {
        status,
        headers: { "content-type": "application/json" },
      }),
    ),
  );
}

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("api client", () => {
  it("returns the data of a successful call", async () => {
    vi.stubGlobal("fetch", respond(200, { status: "ok", database: "ok", version: "0.1.0" }));
    await expect(unwrap(api.GET("/api/health"))).resolves.toMatchObject({ status: "ok" });
  });

  it("throws an ApiError carrying the API's code and message", async () => {
    vi.stubGlobal("fetch", respond(503, { code: "unavailable", message: "Database down" }));
    const error = await unwrap(api.GET("/api/health")).catch((e: unknown) => e);
    expect(error).toBeInstanceOf(ApiError);
    expect(error).toMatchObject({ status: 503, code: "unavailable", message: "Database down" });
  });

  it("keeps the per-field messages of a validation error", async () => {
    vi.stubGlobal(
      "fetch",
      respond(422, { code: "validation", message: "Bad slug", details: { slug: "Bad slug" } }),
    );
    const error = await unwrap(api.GET("/api/health")).catch((e: unknown) => e);
    expect(error).toMatchObject({ code: "validation", fields: { slug: "Bad slug" } });
  });

  it("sends the CSRF header on mutations only", async () => {
    const fetch = respond(200, {});
    vi.stubGlobal("fetch", fetch);
    await api.GET("/api/health");
    await api.POST("/api/auth/logout");
    const [get, post] = fetch.mock.calls.map(([request]) => request);
    expect(get?.headers.get("X-Romulus-Csrf")).toBeNull();
    expect(post?.headers.get("X-Romulus-Csrf")).toBe("1");
  });
});
