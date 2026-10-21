// node --test site/edge (just site-check runs it).
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
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

/** Where MCP clients look for how to sign in (RFC 9728, RFC 8414). */
const WELL_KNOWN = [
  "/.well-known/oauth-protected-resource/api/mcp",
  "/.well-known/oauth-authorization-server",
];

test("only the app, the API and OAuth's discovery documents are proxied", () => {
  for (const path of [
    "/app",
    "/app/",
    "/app/acme/shop",
    "/api/health",
    "/api",
    ...WELL_KNOWN,
  ])
    assert.ok(proxied(path), path);
  for (const path of [
    "/",
    "/apple",
    "/application",
    "/api-docs",
    "/styles.css",
    "/fonts/x.woff2",
    "/mentions-legales",
    "/legal-notice",
    "/.well-known/security.txt",
    // The root form would describe the origin as a resource (RFC 9728, 3.3).
    "/.well-known/oauth-protected-resource",
    "/.well-known/oauth-protected-resources",
    "/.well-known/oauth-authorization-server/api/mcp",
    "/.well-known/openid-configuration",
  ])
    assert.ok(!proxied(path), path);
});

/** Whether Pages runs the Function for `path`: public/_routes.json, where `*` matches anything and exclude wins. */
function routed(path) {
  const routes = JSON.parse(
    readFileSync(new URL("../public/_routes.json", import.meta.url), "utf8"),
  );
  const matches = (rule) =>
    new RegExp(
      `^${rule.replace(/[.+?^${}()|[\]\\]/g, "\\$&").replace(/\*/g, ".*")}$`,
    ).test(path);
  return routes.include.some(matches) && !routes.exclude.some(matches);
}

test("_routes.json runs the Function for the app, the API and OAuth's discovery only: the homepage and the legal notice stay static", () => {
  for (const path of [
    "/app",
    "/app/",
    "/app/acme/shop",
    "/api/health",
    ...WELL_KNOWN,
  ])
    assert.ok(routed(path), path);
  for (const path of [
    "/",
    "/index.html",
    "/mentions-legales",
    "/mentions-legales.html",
    "/legal-notice",
    "/legal-notice.html",
    "/sitemap.xml",
    "/apple",
    "/.well-known/security.txt",
    "/.well-known/oauth-protected-resource",
  ])
    assert.ok(!routed(path), path);
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

test("Stripe's webhook reaches the API with its exact bytes and signature (billing)", async () => {
  const { calls, fetchImpl } = recorder();
  // The signature covers the raw bytes: spacing, key order, escapes, a CRLF,
  // non-ASCII text must all arrive as sent.
  const raw = new TextEncoder().encode(
    '{"id": "evt_1",  "type":"invoice.paid",\r\n "name":"Société \\u00e9"}\n',
  );
  const signature =
    "t=1800000000,v1=5257a869e7ecebeda32affa62cdca3fa51cad7e77a0e56ff536d0ce8e108d8bd,v0=6ffbb59b";
  const request = new Request(
    "https://romulus.lupidae.com/api/billing/webhook",
    {
      method: "POST",
      headers: {
        "stripe-signature": signature,
        "content-type": "application/json; charset=utf-8",
        "user-agent": "Stripe/1.0 (+https://stripe.com/docs/webhooks)",
        "cf-connecting-ip": "3.18.12.63",
      },
      body: raw,
    },
  );
  await proxy(request, env, fetchImpl);
  const [{ url, init }] = calls;
  assert.equal(url, "https://romulus-origin.lupidae.com/api/billing/webhook");
  assert.equal(init.method, "POST");
  assert.deepEqual(
    new Uint8Array(await new Response(init.body).arrayBuffer()),
    raw,
  );
  assert.equal(init.headers.get("stripe-signature"), signature);
  assert.equal(
    init.headers.get("content-type"),
    "application/json; charset=utf-8",
  );
  assert.equal(init.headers.get(KEY_HEADER), env.ROMULUS_EDGE_KEY);
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
  const redirect = new Response(null, {
    status: 302,
    headers: {
      location: "https://romulus-auth.lupidae.com/oauth/v2/authorize",
    },
  });
  const { calls, fetchImpl } = recorder(redirect);
  const response = await proxy(
    new Request("https://romulus.lupidae.com/api/orgs/acme/drafts/1/ws", {
      headers: { upgrade: "websocket", connection: "Upgrade" },
    }),
    env,
    fetchImpl,
  );
  assert.equal(response, redirect);
  assert.equal(calls[0].init.headers.get("upgrade"), "websocket");
});

test("other hostnames are sent to the public one", async () => {
  const { calls, fetchImpl } = recorder();
  const response = await proxy(
    new Request("https://abc123.romulus-site.pages.dev/app/acme?tab=1"),
    env,
    fetchImpl,
  );
  assert.equal(response.status, 308);
  assert.equal(
    response.headers.get("location"),
    "https://romulus.lupidae.com/app/acme?tab=1",
  );
  assert.equal(calls.length, 0);
});

test("without its configuration, or its origin, the proxy answers 503 in the API's format", async () => {
  const response = await proxy(
    new Request("https://romulus.lupidae.com/api/health"),
    { ...env, ROMULUS_EDGE_KEY: "" },
  );
  assert.equal(response.status, 503);
  assert.equal((await response.json()).code, "unavailable");
  const down = await proxy(
    new Request("https://romulus.lupidae.com/api/health"),
    env,
    async () => {
      throw new TypeError("network");
    },
  );
  assert.equal(down.status, 503);
});

test("an agent's MCP request passes with its token, and the answer comes back as the origin streams it", async () => {
  const stream = new ReadableStream({
    start(controller) {
      controller.enqueue(new TextEncoder().encode('{"jsonrpc":"2.0","id":1,'));
      controller.enqueue(new TextEncoder().encode('"result":{}}'));
      controller.close();
    },
  });
  const answer = new Response(stream, {
    headers: { "content-type": "application/json" },
  });
  const { calls, fetchImpl } = recorder(answer);
  const request = new Request(`https://${env.ROMULUS_PUBLIC_HOST}/api/mcp`, {
    method: "POST",
    headers: {
      authorization: "Bearer rmls_pat_secret",
      "content-type": "application/json",
      "mcp-protocol-version": "2026-07-28",
      "mcp-method": "tools/call",
      "mcp-name": "whoami",
    },
    body: '{"jsonrpc":"2.0","id":1,"method":"tools/call"}',
  });
  assert.ok(proxied("/api/mcp"));
  const response = await proxy(request, env, fetchImpl);
  const [{ url, init }] = calls;
  assert.equal(url, new URL("/api/mcp", env.ROMULUS_ORIGIN).href);
  assert.equal(init.headers.get("authorization"), "Bearer rmls_pat_secret");
  for (const header of ["mcp-protocol-version", "mcp-method", "mcp-name"])
    assert.ok(init.headers.get(header), header);
  // The origin's response itself: nothing buffers it on the way.
  assert.equal(response, answer);
  assert.equal(await response.text(), '{"jsonrpc":"2.0","id":1,"result":{}}');
});

test("an MCP client finds romulus's authorization server through the proxy", async () => {
  const document = Response.json({
    resource: `https://${env.ROMULUS_PUBLIC_HOST}/api/mcp`,
  });
  const { calls, fetchImpl } = recorder(document);
  for (const path of WELL_KNOWN) {
    const response = await proxy(
      new Request(`https://${env.ROMULUS_PUBLIC_HOST}${path}`),
      env,
      fetchImpl,
    );
    assert.equal(response, document, path);
  }
  assert.deepEqual(
    calls.map((call) => call.url),
    WELL_KNOWN.map((path) => new URL(path, env.ROMULUS_ORIGIN).href),
  );
  for (const call of calls) {
    assert.equal(call.init.method, "GET");
    assert.equal(call.init.headers.get(KEY_HEADER), env.ROMULUS_EDGE_KEY);
  }
});

test("a client's token request reaches the API as a form, without a cookie of its own", async () => {
  const { calls, fetchImpl } = recorder();
  const body =
    "grant_type=authorization_code&code=c&code_verifier=v&client_id=id";
  await proxy(
    new Request(`https://${env.ROMULUS_PUBLIC_HOST}/api/oauth/token`, {
      method: "POST",
      headers: { "content-type": "application/x-www-form-urlencoded" },
      body,
    }),
    env,
    fetchImpl,
  );
  const [{ url, init }] = calls;
  assert.equal(url, new URL("/api/oauth/token", env.ROMULUS_ORIGIN).href);
  assert.equal(await new Response(init.body).text(), body);
  assert.equal(
    init.headers.get("content-type"),
    "application/x-www-form-urlencoded",
  );
  assert.equal(init.headers.get("cookie"), null);
});
