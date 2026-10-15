// node --test site/edge (just site-check runs it).
import assert from "node:assert/strict";
import { test } from "node:test";

import { CLIENT_IP_HEADER, KEY_HEADER, proxied, proxy } from "./proxy.js";

const env = {
  ROMULUS_PUBLIC_HOST: "romulus.lupidae.com",
  ROMULUS_ORIGIN: "https://romulus-origin.lupidae.com",
  ROMULUS_EDGE_KEY: "k".repeat(64),
};

/** A fetch that records what it was asked and answers `response`. */
function recorder(response = new Response("ok")) {
  const calls = [];
  const fetchImpl = async (url, init) => {
    calls.push({ url: String(url), init });
    return response;
  };
  return { calls, fetchImpl };
}

test("only the app and the API are proxied", () => {
  for (const path of ["/app", "/app/", "/app/acme/shop", "/api/health", "/api"]) assert.ok(proxied(path), path);
  for (const path of ["/", "/apple", "/application", "/api-docs", "/styles.css", "/fonts/x.woff2"])
    assert.ok(!proxied(path), path);
});

test("a request reaches the origin with its method, body, cookies and Origin, plus the edge key", async () => {
  const { calls, fetchImpl } = recorder();
  const request = new Request("https://romulus.lupidae.com/api/orgs?x=1", {
    method: "POST",
    headers: {
      cookie: "romulus_session=abc",
      origin: "https://romulus.lupidae.com",
      "x-romulus-csrf": "1",
      "cf-connecting-ip": "203.0.113.9",
      [KEY_HEADER]: "forged",
      "content-type": "application/json",
    },
    body: '{"name":"acme"}',
  });
  await proxy(request, env, fetchImpl);
  const [{ url, init }] = calls;
  assert.equal(url, "https://romulus-origin.lupidae.com/api/orgs?x=1");
  assert.equal(init.method, "POST");
  assert.equal(init.redirect, "manual");
  assert.equal(await new Response(init.body).text(), '{"name":"acme"}');
  assert.equal(init.headers.get("cookie"), "romulus_session=abc");
  assert.equal(init.headers.get("origin"), "https://romulus.lupidae.com");
  assert.equal(init.headers.get("x-romulus-csrf"), "1");
  assert.equal(init.headers.get(KEY_HEADER), env.ROMULUS_EDGE_KEY);
  assert.equal(init.headers.get(CLIENT_IP_HEADER), "203.0.113.9");
  assert.equal(init.headers.get("x-forwarded-host"), "romulus.lupidae.com");
  assert.equal(init.headers.get("host"), null);
});

test("a visitor cannot choose the address the API sees", async () => {
  const { calls, fetchImpl } = recorder();
  const request = new Request("https://romulus.lupidae.com/api/health", {
    headers: { [CLIENT_IP_HEADER]: "198.51.100.1" },
  });
  await proxy(request, env, fetchImpl);
  assert.equal(calls[0].init.headers.get(CLIENT_IP_HEADER), null);
  assert.equal(calls[0].init.body, undefined);
});

test("redirects and websocket upgrades come back as the origin sent them", async () => {
  const redirect = new Response(null, { status: 302, headers: { location: "https://romulus-auth.lupidae.com/oauth/v2/authorize" } });
  const { calls, fetchImpl } = recorder(redirect);
  const response = await proxy(
    new Request("https://romulus.lupidae.com/api/orgs/acme/drafts/1/ws", { headers: { upgrade: "websocket", connection: "Upgrade" } }),
    env,
    fetchImpl,
  );
  assert.equal(response, redirect);
  assert.equal(calls[0].init.headers.get("upgrade"), "websocket");
});

test("other hostnames are sent to the public one", async () => {
  const { calls, fetchImpl } = recorder();
  const response = await proxy(new Request("https://abc123.romulus-site.pages.dev/app/acme?tab=1"), env, fetchImpl);
  assert.equal(response.status, 308);
  assert.equal(response.headers.get("location"), "https://romulus.lupidae.com/app/acme?tab=1");
  assert.equal(calls.length, 0);
});

test("without its configuration, or its origin, the proxy answers 503 in the API's format", async () => {
  const response = await proxy(new Request("https://romulus.lupidae.com/api/health"), { ...env, ROMULUS_EDGE_KEY: "" });
  assert.equal(response.status, 503);
  assert.equal((await response.json()).code, "unavailable");
  const down = await proxy(new Request("https://romulus.lupidae.com/api/health"), env, async () => {
    throw new TypeError("network");
  });
  assert.equal(down.status, 503);
});
