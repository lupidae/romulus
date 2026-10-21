// The app and the API on the homepage's origin. Pages serves the homepage
// (public/) itself; /app, /api and OAuth's discovery documents
// (/.well-known/oauth-*, where MCP clients look for how to sign in) go to the
// API through this proxy, which functions/_middleware.js runs
// (public/_routes.json limits the Function to those paths, so the homepage
// never runs it).
//
// The API is behind a Cloudflare Tunnel at env.ROMULUS_ORIGIN. That hostname
// is public, so every forwarded request carries the edge key, a secret shared
// with the API (crates/romulus-api/src/edge.rs), which refuses requests
// without it. Everything else is passed through untouched, the way the API
// expects its one public origin to behave:
// - method, body, cookies and `Origin` as the browser sent them (the CSRF
//   check compares `Origin` with https://romulus.lupidae.com);
// - redirects returned, not followed (sign-in goes through the IdP and back);
// - websocket upgrades (/api/orgs/…/drafts/…/ws): the runtime pipes them when
//   the origin's 101 is returned as is;
// - the visitor's address (CF-Connecting-IP), for the API's per-client limits.

export const KEY_HEADER = "x-romulus-edge-key";
export const CLIENT_IP_HEADER = "x-romulus-client-ip";

const PREFIXES = [
  "/app",
  "/api",
  // RFC 9728 and RFC 8414, at the root and path-suffixed (…/api/mcp).
  "/.well-known/oauth-protected-resource",
  "/.well-known/oauth-authorization-server",
];

/** Whether the API answers this path (the rest is the homepage). */
export function proxied(pathname) {
  return PREFIXES.some(
    (prefix) => pathname === prefix || pathname.startsWith(`${prefix}/`),
  );
}

/** Forwards `request` to the API. `env` holds the Function's variables. */
export async function proxy(request, env, fetchImpl = fetch) {
  const url = new URL(request.url);
  // romulus-site.pages.dev and preview deployments: one public origin only,
  // the one cookies, redirects and Origin checks are built for.
  if (url.hostname !== env.ROMULUS_PUBLIC_HOST) {
    return Response.redirect(
      `https://${env.ROMULUS_PUBLIC_HOST}${url.pathname}${url.search}`,
      308,
    );
  }
  if (!env.ROMULUS_EDGE_KEY || !env.ROMULUS_ORIGIN) {
    return unavailable("romulus is not configured here yet.");
  }

  const headers = new Headers(request.headers);
  // The runtime sets Host from the URL; the API never reads it.
  headers.delete("host");
  headers.set(KEY_HEADER, env.ROMULUS_EDGE_KEY);
  const client = request.headers.get("cf-connecting-ip");
  if (client) headers.set(CLIENT_IP_HEADER, client);
  else headers.delete(CLIENT_IP_HEADER);
  headers.set("x-forwarded-host", url.host);
  headers.set("x-forwarded-proto", url.protocol.replace(":", ""));

  const target = new URL(`${url.pathname}${url.search}`, env.ROMULUS_ORIGIN);
  const hasBody = request.method !== "GET" && request.method !== "HEAD";
  try {
    return await fetchImpl(target, {
      method: request.method,
      headers,
      body: hasBody ? request.body : undefined,
      redirect: "manual",
    });
  } catch (err) {
    console.error("proxy: the origin is unreachable", err);
    return unavailable(
      "romulus is unreachable right now. Try again in a minute.",
    );
  }
}

/** A 503 in the API's error format, which the web app understands. */
function unavailable(message) {
  return Response.json(
    { code: "unavailable", message },
    { status: 503, headers: { "cache-control": "no-store" } },
  );
}
