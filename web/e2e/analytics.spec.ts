import { expect, type Page, test } from "@playwright/test";

import { createOrg, devSignIn, unique } from "./helpers";

// Analytics are the server's to turn on (ROMULUS_ANALYTICS_*, `instance.analytics`);
// this API has none, so the instance is answered with a tracker here, and the
// tracker is a stand-in that records what the app hands it (Umami's
// `track(fn)`: the app builds every payload from the tracker's defaults).
const SCRIPT = "https://stats.example.test/script.js";
const FAKE_UMAMI = `(() => {
  const script = document.currentScript;
  window.__sent = [];
  window.__script = { ...script.dataset };
  const defaults = () => ({ website: script.dataset.websiteId, hostname: location.hostname,
    url: location.href, title: document.title, referrer: document.referrer });
  window.umami = { track: (build) => window.__sent.push(build(defaults())) };
})();`;

async function withTracker(page: Page) {
  await page.route("**/api/instance", async (route) => {
    const response = await route.fetch();
    const instance = (await response.json()) as Record<string, unknown>;
    await route.fulfill({
      response,
      json: { ...instance, analytics: { script_url: SCRIPT, website_id: "site-e2e" } },
    });
  });
  await page.route(SCRIPT, (route) =>
    route.fulfill({ contentType: "text/javascript", body: FAKE_UMAMI }),
  );
}

interface Sent {
  url: string;
  title: string;
  referrer: string;
  name?: string;
  data?: Record<string, string>;
}

/** What the tracker was handed on this page (nothing before it loads). */
const sent = (page: Page) =>
  page.evaluate(() => (window as unknown as { __sent?: Sent[] }).__sent ?? []);

test("a new account's source is reported once and its sign-up counted, by route pattern only", async ({
  page,
}) => {
  await withTracker(page);
  const email = `${unique("track").replace(/\W/g, "")}@romulus.test`;
  // A first visit from a campaign, straight to the app.
  await page.goto("./signin?utm_source=e2e&utm_campaign=launch&next=/app/new", {
    referer: "https://news.example.org/item?id=1",
  });
  await expect(page.getByRole("heading", { name: "Sign in to romulus" })).toBeVisible();
  // Every way in from here comes back to where it was going, with the source
  // in the address (the dev sign-in's link, as the identity provider's way
  // back would); nothing of it is stored in the browser.
  const via = new URL(
    (await page.getByRole("link", { name: "alice" }).getAttribute("href"))!,
    page.url(),
  );
  const next = via.searchParams.get("next")!;
  expect(next).toBe(
    "/app/new?utm_source=e2e&utm_campaign=launch&ref_origin=https%3A%2F%2Fnews.example.org",
  );
  expect(
    await page.evaluate(() => Object.keys(localStorage).filter((k) => !k.startsWith("romulus:"))),
  ).toEqual([]);

  const reported = page.waitForResponse((r) => r.url().endsWith("/api/me/signup-source"));
  await devSignIn(page, email, next);
  const report = await reported;
  expect(report.request().postDataJSON()).toMatchObject({
    utm_source: "e2e",
    utm_campaign: "launch",
    referrer: "https://news.example.org",
  });
  expect(await report.json()).toEqual({ recorded: true, new_account: true });
  const seen: Sent[] = [];
  // The page view and the event, in whichever order they were ready.
  await expect
    .poll(async () => (await sent(page)).map((s) => s.name ?? s.url).sort())
    .toEqual(["/app/new?utm_source=e2e&utm_campaign=launch", "signup_completed"]);
  seen.push(...(await sent(page)));
  expect(await page.evaluate(() => (window as unknown as { __script: object }).__script)).toEqual({
    websiteId: "site-e2e",
    autoTrack: "false",
    doNotTrack: "true",
  });

  const name = unique("Tracked");
  const slug = await createOrg(page, name);
  await page.goto(`./${slug}`);
  await expect
    .poll(async () => (await sent(page)).map((s) => s.name ?? s.url))
    .toContain("/app/$org");
  seen.push(...(await sent(page)));
  // Nothing names the organization, carries a query beyond utm_*, or a title.
  for (const s of seen) {
    expect(JSON.stringify(s)).not.toContain(slug);
    expect(s.url).toMatch(/^\/app\/[^?]*(\?utm_[a-z]+=[^&]*(&utm_[a-z]+=[^&]*)*)?$/);
    expect(s.title).toBe("romulus");
  }

  // A later page load reports again (nothing in the browser remembers it):
  // the server finds the sign-up told, and no second one is counted.
  const again = page.waitForResponse((r) => r.url().endsWith("/api/me/signup-source"));
  await page.reload();
  expect(await (await again).json()).toEqual({ recorded: false, new_account: false });
  await expect
    .poll(async () => (await sent(page)).map((s) => s.name ?? s.url))
    .toContain("/app/$org");
  expect((await sent(page)).filter((s) => s.name)).toEqual([]);
});

test("without a tracker the app loads nothing from another origin", async ({ page }) => {
  const elsewhere: string[] = [];
  page.on("request", (request) => {
    const { protocol, hostname } = new URL(request.url());
    if (protocol !== "data:" && hostname !== "localhost" && hostname !== "127.0.0.1")
      elsewhere.push(request.url());
  });
  await devSignIn(page, `${unique("plain").replace(/\W/g, "")}@romulus.test`, "/app/new");
  await expect(page.getByLabel("Organization name")).toBeVisible();
  expect(await page.locator("script[src^='http']").count()).toBe(0);
  expect(elsewhere).toEqual([]);
});
