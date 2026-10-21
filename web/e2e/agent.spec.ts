import { expect, type Page, test } from "@playwright/test";

import { createOrg, devSignIn, unique } from "./helpers";

/** What an MCP client sends first, with `token`. */
function initialize(page: Page, token: string, client: string) {
  return page.request.post("/api/mcp", {
    headers: { Authorization: `Bearer ${token}` },
    data: {
      jsonrpc: "2.0",
      id: 1,
      method: "initialize",
      params: {
        protocolVersion: "2025-06-18",
        capabilities: {},
        clientInfo: { name: client, version: "1" },
      },
    },
  });
}

test("an agent gets a token on the Connect page, connects, and is cut off by revoking it", async ({
  page,
  context,
}) => {
  await context.grantPermissions(["clipboard-read", "clipboard-write"]);
  await devSignIn(page, `${unique("agent").replace(" ", "-")}@romulus.test`, "/app/new");
  const org = await createOrg(page, unique("Agents"));

  // Someone without a token is offered to connect one from their
  // organization's home.
  await page.goto(`./${org}`);
  await page.getByRole("link", { name: "Connect your agent" }).click();
  await expect(page.getByRole("heading", { level: 1, name: "Connect your agent" })).toBeVisible();

  // By default, nothing secret: the agent signs in through the browser.
  const origin = new URL(page.url()).origin;
  await expect(page.getByLabel("Claude Code command", { exact: true })).toHaveText(
    `claude mcp add --transport http romulus ${origin}/api/mcp`,
  );
  await expect(page.getByText("Waiting for your agent to connect.")).toBeVisible();

  // With a token (Advanced): Claude Code's command asks for it; the
  // configurations wait for it.
  await expect(page.getByLabel("Name")).toHaveCount(0);
  await page.getByRole("button", { name: /^Advanced: connect with an access token/ }).click();
  const tokenTabs = page.getByRole("tablist", { name: "Agent for the token" });
  await expect(page.getByLabel("Claude Code command with a token", { exact: true })).toContainText(
    "read -rs ROMULUS_TOKEN",
  );
  await expect(page.getByText("The token goes in place of")).toHaveCount(0);
  await tokenTabs.getByRole("tab", { name: "Cursor" }).click();
  await expect(page.getByLabel("Cursor configuration with a token", { exact: true })).toContainText(
    "Bearer <your token>",
  );
  await expect(page.getByText("The token goes in place of <your token>.")).toBeVisible();
  await tokenTabs.getByRole("tab", { name: "Claude Code" }).click();
  await page.getByLabel("Name").fill("Claude Code e2e");
  await page.getByRole("button", { name: "Create token" }).click();
  const secret = await page.getByRole("textbox", { name: "Access token" }).inputValue();
  expect(secret).toMatch(/^rmls_pat_[A-Za-z0-9_-]{43}$/);

  // Every snippet holds the endpoint; the configurations hold the token, and
  // Claude Code's command asks for it (a command stays in shell history).
  await page.getByRole("button", { name: "Copy claude code command with a token" }).click();
  const command = await page.evaluate(() => navigator.clipboard.readText());
  expect(command).toBe(
    [
      "read -rs ROMULUS_TOKEN",
      `claude mcp add --transport http romulus ${origin}/api/mcp --header "Authorization: Bearer $ROMULUS_TOKEN"`,
    ].join("\n"),
  );
  expect(command).not.toContain(secret);
  await tokenTabs.getByRole("tab", { name: "Cursor" }).click();
  const cursor = JSON.parse(
    (await page.getByLabel("Cursor configuration with a token", { exact: true }).textContent()) ??
      "{}",
  ) as { mcpServers: { romulus: { headers: { Authorization: string } } } };
  expect(cursor.mcpServers.romulus.headers.Authorization).toBe(`Bearer ${secret}`);

  // The check from the browser, then the agent's own first request.
  await page.getByRole("button", { name: "Test connection" }).click();
  // The person the token acts as, by name (dev sign-in names them from the
  // address: agent-… → "Agent …").
  await expect(page.getByText(/^Connected as Agent /)).toBeVisible();
  expect((await initialize(page, secret, "e2e-agent")).status()).toBe(200);
  await expect(page.getByText(/Your agent connected .*\(e2e-agent 1\)/)).toBeVisible({
    timeout: 15_000,
  });

  // The account's tokens: when it was used, by what; then revoked.
  await page.getByRole("button", { name: /^Account:/ }).click();
  await page.getByRole("menuitem", { name: "Access tokens" }).click();
  await expect(page.getByRole("heading", { level: 1, name: "Access tokens" })).toBeVisible();
  const row = page.getByRole("listitem").filter({ hasText: "Claude Code e2e" });
  await expect(row).toContainText("Read and write");
  await expect(row).toContainText("by e2e-agent 1");
  await row.getByRole("button", { name: "Revoke Claude Code e2e" }).click();
  await page.getByRole("button", { name: "Revoke token" }).click();
  await expect(page.getByText("No access tokens")).toBeVisible();
  expect((await initialize(page, secret, "e2e-agent")).status()).toBe(401);
});

test("a token made on the tokens page is shown once, read only", async ({ page }) => {
  await devSignIn(page, `${unique("tokens").replace(" ", "-")}@romulus.test`, "/app/new");
  await createOrg(page, unique("Tokens"));
  await page.goto("./settings/tokens");
  await expect(page.getByText("No access tokens")).toBeVisible();
  await page.goto("./settings/agent");
  await page.getByRole("button", { name: /^Advanced: connect with an access token/ }).click();
  await page.getByLabel("Name").fill("first");
  await page.getByRole("button", { name: "Create token" }).click();
  await expect(page.getByRole("textbox", { name: "Access token" })).toBeVisible();
  await page.goto("./settings/tokens");
  await page.getByRole("button", { name: "New token" }).click();
  const dialog = page.getByRole("dialog");
  await dialog.getByLabel("Name").fill("Cursor, read only");
  await dialog.getByRole("combobox", { name: "Access" }).click();
  await page.getByRole("option", { name: /^Read only/ }).click();
  await dialog.getByRole("combobox", { name: "Expires after" }).click();
  await page.getByRole("option", { name: "30 days" }).click();
  await dialog.getByRole("button", { name: "Create token" }).click();
  const secret = await dialog.getByRole("textbox", { name: "Access token" }).inputValue();
  expect(secret).toMatch(/^rmls_pat_/);
  await dialog.getByRole("button", { name: "Done" }).click();
  const row = page.getByRole("listitem").filter({ hasText: "Cursor, read only" });
  await expect(row).toContainText("Read only");
  await expect(row).toContainText("Never used");
  await expect(row).toContainText(/Expires in (29|30) days/);
  // Shown once: nowhere on the page now.
  await expect(page.getByText(secret)).toHaveCount(0);
  // A read-only token is offered the reading tools only.
  const listed = await page.request.post("/api/mcp", {
    headers: { Authorization: `Bearer ${secret}` },
    data: { jsonrpc: "2.0", id: 2, method: "tools/list" },
  });
  const tools = ((await listed.json()) as { result: { tools: { name: string }[] } }).result.tools;
  expect(tools.map((t) => t.name)).toContain("get_schema");
  expect(tools.map((t) => t.name)).not.toContain("create_draft");
});
