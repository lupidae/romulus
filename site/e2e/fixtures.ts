import { test as base } from "@playwright/test";

/** The homepage's audience measurement (index.html, _headers). */
export const ANALYTICS = "https://stats.lupidae.com";

/**
 * Every test: the analytics server is never reached. Its script is an empty
 * one (the page's own tag only names the website and the domain it counts
 * on, and that is romulus.lupidae.com, not this server), and what was
 * asked of it is `analytics`.
 */
export const test = base.extend<{ analytics: string[]; foundingOffer: void }>({
  analytics: [
    async ({ context }, use) => {
      const asked: string[] = [];
      await context.route(`${ANALYTICS}/**`, (route) => {
        asked.push(route.request().url());
        return route.fulfill({ status: 200, contentType: "text/javascript", body: "" });
      });
      await use(asked);
    },
    { auto: true },
  ],
  // The homepage asks the API how many founding-offer places are left
  // (app.js); this server has no API: none are (a test answers otherwise
  // with its own page.route, which comes first).
  foundingOffer: [
    async ({ context }, use) => {
      await context.route("**/api/billing/offers/founding", (route) => route.fulfill({ json: { left: 0 } }));
      await use();
    },
    { auto: true },
  ],
});

export { expect } from "@playwright/test";
