import { createHash, randomBytes } from "node:crypto";

import { type APIRequestContext, expect, type Page, test } from "@playwright/test";

import { createOrg, devSignIn, unique } from "./helpers";

// An MCP client connecting by signing in (OAuth, crates/romulus-api/src/oauth),
// as Claude Code does it: it registers, sends the browser to romulus with a PKCE
// challenge, gets the code back on a loopback redirect, exchanges it. Nothing
// listens on the redirect here: the test catches the navigation instead.
const CALLBACK = "http://127.0.0.1:9/callback";

interface Agent {
  clientId: string;
  name: string;
  verifier: string;
  authorizeUrl: string;
}

async function registerAgent(request: APIRequestContext, name: string): Promise<Agent> {
  const registered = await request.post("/api/oauth/register", {
    data: { client_name: name, redirect_uris: [CALLBACK], token_endpoint_auth_method: "none" },
  });
  expect(registered.status()).toBe(201);
  const clientId = ((await registered.json()) as { client_id: string }).client_id;
  const verifier = randomBytes(32).toString("base64url");
  const params = new URLSearchParams({
    response_type: "code",
    client_id: clientId,
    redirect_uri: CALLBACK,
    state: "e2e-state",
    code_challenge: createHash("sha256").update(verifier).digest("base64url"),
    code_challenge_method: "S256",
    scope: "romulus.read romulus.write",
  });
  return { clientId, name, verifier, authorizeUrl: `/api/oauth/authorize?${params.toString()}` };
}

/** Where the browser was sent back to (the agent's callback), caught. */
function catchCallback(page: Page): Promise<URL> {
  return new Promise((resolve) => {
    void page.route(`${CALLBACK}**`, async (route) => {
      resolve(new URL(route.request().url()));
      await route.fulfill({ contentType: "text/plain", body: "Back in the agent." });
    });
  });
}

function mcp(request: APIRequestContext, token: string, method: string, params: object = {}) {
  return request.post("/api/mcp", {
    headers: { Authorization: `Bearer ${token}` },
    data: { jsonrpc: "2.0", id: 1, method, params },
  });
}

test("an agent connects by signing in: approved here, it works as you until disconnected", async ({
  page,
}) => {
  const email = `${unique("oauth").replace(" ", "-")}@romulus.test`;
  await devSignIn(page, email, "/app/new");
  await createOrg(page, unique("Apps"));
  const agent = await registerAgent(page.request, unique("Claude Code"));

  // The agent opens the browser: romulus asks.
  const callback = catchCallback(page);
  await page.goto(agent.authorizeUrl);
  await expect(
    page.getByRole("heading", { level: 1, name: `Allow ${agent.name} to use romulus?` }),
  ).toBeVisible();
  await expect(page.getByText("127.0.0.1, a program on this computer")).toBeVisible();
  await expect(page.getByText(email)).toBeVisible();
  await expect(page.getByRole("alert")).toContainText(
    "Only if you just started connecting an agent",
  );
  await expect(page.getByRole("radio", { name: /^Read and write/ })).toBeChecked();
  await page.getByRole("button", { name: "Allow", exact: true }).click();

  // Back in the agent, with a code it exchanges for tokens.
  const back = await callback;
  expect(back.searchParams.get("state")).toBe("e2e-state");
  expect(back.searchParams.get("iss")).toBe(new URL(page.url()).origin);
  const code = back.searchParams.get("code");
  expect(code).toBeTruthy();
  const exchanged = await page.request.post("/api/oauth/token", {
    form: {
      grant_type: "authorization_code",
      client_id: agent.clientId,
      code: code ?? "",
      code_verifier: agent.verifier,
      redirect_uri: CALLBACK,
    },
  });
  expect(exchanged.status()).toBe(200);
  const tokens = (await exchanged.json()) as { access_token: string; scope: string };
  expect(tokens.access_token).toMatch(/^rmls_oat_/);
  expect(tokens.scope).toBe("romulus.read romulus.write");
  const whoami = await mcp(page.request, tokens.access_token, "tools/call", {
    name: "whoami",
    arguments: {},
  });
  expect(whoami.status()).toBe(200);
  expect(JSON.stringify(await whoami.json())).toContain(`through the connected app`);

  // Listed in the account, and cut off from there.
  await page.goto("./settings/apps");
  await expect(page.getByRole("heading", { level: 1, name: "Connected apps" })).toBeVisible();
  const row = page.getByRole("listitem").filter({ hasText: agent.name });
  await expect(row).toContainText("Read and write");
  await expect(row).toContainText("On your computer (127.0.0.1)");
  await row.getByRole("button", { name: `Disconnect ${agent.name}` }).click();
  await page.getByRole("button", { name: "Disconnect", exact: true }).click();
  await expect(page.getByText("No connected apps")).toBeVisible();
  expect((await mcp(page.request, tokens.access_token, "ping")).status()).toBe(401);
});

test("denying sends the agent away with nothing, and a request is answered once", async ({
  page,
}) => {
  await devSignIn(page, `${unique("deny").replace(" ", "-")}@romulus.test`, "/app/new");
  const agent = await registerAgent(page.request, unique("Cursor"));
  const callback = catchCallback(page);
  await page.goto(agent.authorizeUrl);
  const consent = page.url();
  // Read only would do, but no.
  await page.getByRole("radio", { name: /^Read only/ }).check();
  await page.getByRole("button", { name: "Deny" }).click();
  const back = await callback;
  expect(back.searchParams.get("error")).toBe("access_denied");
  expect(back.searchParams.get("state")).toBe("e2e-state");
  expect(back.searchParams.get("code")).toBeNull();

  await page.goto(consent);
  await expect(page.getByRole("heading", { level: 1, name: "This request is over" })).toBeVisible();

  // An app romulus does not know is told so, and never sent anywhere.
  await page.goto(
    `/api/oauth/authorize?client_id=${crypto.randomUUID()}&redirect_uri=${encodeURIComponent(CALLBACK)}`,
  );
  await expect(
    page.getByRole("heading", { level: 1, name: "romulus does not know this app" }),
  ).toBeVisible();
});

test("signed out, the consent page goes through sign-in and comes back", async ({ page }) => {
  const agent = await registerAgent(page.request, unique("Claude"));
  await page.goto(agent.authorizeUrl);
  await page.waitForURL((url) => url.pathname === "/app/signin");
  const next = new URL(page.url()).searchParams.get("next") ?? "";
  expect(next).toMatch(/^\/app\/oauth\/authorize\?request=/);
  await devSignIn(page, `${unique("later").replace(" ", "-")}@romulus.test`, next);
  await expect(
    page.getByRole("heading", { level: 1, name: `Allow ${agent.name} to use romulus?` }),
  ).toBeVisible();
});

test("a request romulus refuses stays here: the person may return to the app, which is told why", async ({
  page,
}) => {
  const agent = await registerAgent(page.request, unique("Plain"));
  const url = new URL(agent.authorizeUrl, "http://romulus.invalid");
  url.searchParams.set("code_challenge_method", "plain");
  const callback = catchCallback(page);
  await page.goto(`${url.pathname}${url.search}`);
  // Never sent back by itself: the page says so, and names the way back.
  await expect(
    page.getByRole("heading", { level: 1, name: "romulus can’t accept this request" }),
  ).toBeVisible();
  await expect(page.getByText(/PKCE is required/)).toBeVisible();
  const back = page.getByRole("link", { name: "Return to 127.0.0.1" });
  await expect(back).toHaveAttribute("href", new RegExp(`^${CALLBACK}\\?error=invalid_request`));
  await back.click();
  const returned = await callback;
  expect(returned.searchParams.get("error")).toBe("invalid_request");
  expect(returned.searchParams.get("state")).toBe("e2e-state");
  expect(returned.searchParams.get("iss")).toBeTruthy();
});
