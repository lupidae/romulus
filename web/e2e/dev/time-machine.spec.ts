import { expect, type Page, test } from "@playwright/test";

// The time machine on its dev fixtures (/app/dev/time-machine): the two
// modes, the timeline's handles and keys, and the age scale's colors.
// The shop: v1 customers, orders · v2 products · v3 order_lines, coupons ·
// v4 timestamps · v5 coupons dropped · v6 totals · v7 products renamed
// catalog_items · v8 invoices.

const node = (page: Page, name: string) =>
  page.locator(`.react-flow__node[data-id="public.${name}"]`);
const tables = (page: Page) => page.locator(".react-flow__node-table");
const caption = (page: Page) => page.locator("[data-mode-caption]:visible");

async function open(page: Page, query = "") {
  const errors: string[] = [];
  page.on("pageerror", (error) => errors.push(error.message));
  await page.goto(`./dev/time-machine${query}`);
  // The first visit after a cold start waits for Vite to bundle the deps.
  await expect(tables(page).first()).toBeVisible({ timeout: 30_000 });
  return errors;
}

/** Table ids drawn, and those drawn in full (there at the cursor). */
async function drawn(page: Page) {
  return page.locator(".react-flow__node-table").evaluateAll((nodes) => {
    const all = nodes.map((n) => n.getAttribute("data-id")!).sort();
    const full = nodes
      .filter((n) => {
        const box = n.querySelector(".sg-node");
        return box && !box.hasAttribute("data-ghost") && !box.hasAttribute("data-later");
      })
      .map((n) => n.getAttribute("data-id")!)
      .sort();
    return { all, full };
  });
}

test("the two modes differ whenever the cursor is before the range's end", async ({ page }) => {
  const errors = await open(page);
  const cursor = page.getByRole("slider", { name: "Cursor" });
  await expect(cursor).toHaveAttribute("aria-valuetext", /^v8, /);
  await expect(caption(page)).toHaveText("All 8 versions superposed, v8 highlighted");
  const superposedRadio = page.getByRole("radio", { name: "Superposed" });
  await expect(superposedRadio).toHaveAttribute("data-state", "on");

  // Back to v4: superposed still draws all of the range, invoices (v8) faded.
  await cursor.focus();
  for (let i = 0; i < 4; i++) await page.keyboard.press("ArrowLeft");
  await expect(cursor).toHaveAttribute("aria-valuetext", /^v4, Timestamps/);
  await expect(caption(page)).toHaveText("All 8 versions superposed, v4 highlighted");
  await expect(node(page, "invoices").locator(".sg-node")).toHaveAttribute("data-later", "");
  await expect(node(page, "invoices")).toContainText("+v8");
  // coupons: there at v4 (in full), though it goes in v5.
  await expect(node(page, "coupons").locator(".sg-node")).not.toHaveAttribute("data-ghost");
  await expect(node(page, "coupons")).toContainText("−v5");
  // orders.status (v5) is a column still to come.
  await expect(node(page, "orders").locator('[data-column="status"]')).toHaveAttribute(
    "data-later",
    "",
  );
  const superposed = await drawn(page);
  expect(superposed.all).toContain("public.invoices");

  // At the cursor: exactly v4, which is what superposed drew in full.
  await page.locator("body").press("m");
  await expect(page.getByRole("radio", { name: "At cursor" })).toHaveAttribute("data-state", "on");
  await expect(caption(page)).toHaveText("The schema at v4");
  // Once what leaves has gone (an animation: polled, not waited for).
  await expect.poll(async () => (await drawn(page)).all).toEqual(superposed.full);
  expect(superposed.full).not.toEqual(superposed.all);
  await expect(node(page, "invoices")).toHaveCount(0);
  await expect(node(page, "orders").locator('[data-column="status"]')).toHaveCount(0);

  // And back: switching always shows a change.
  await page.getByRole("radio", { name: "Superposed" }).click();
  await expect(node(page, "invoices")).toBeVisible();
  expect(errors).toEqual([]);
});

test("a dragged range handle says where it is, during the drag and after it", async ({ page }) => {
  await open(page);
  const start = page.getByRole("slider", { name: "Range start" });
  await expect(start).toHaveAttribute("aria-valuetext", /^v1, /);
  const track = (await page.locator("[data-track]").boundingBox())!;
  const handle = (await start.boundingBox())!;
  // Versions sit evenly along the track, 14px in from both ends.
  const xOf = (i: number) => track.x + 14 + ((track.width - 28) * i) / 7;
  const y = handle.y + handle.height / 2;
  await page.mouse.move(handle.x + handle.width / 2, y);
  await page.mouse.down();
  await page.mouse.move(xOf(2), y, { steps: 12 });
  const card = page.locator("[data-track-card]");
  await expect(card).toHaveAttribute("data-track-card", "from");
  await expect(card).toContainText("Range start");
  await expect(card).toContainText("v3");
  await expect(card).toContainText("Order lines, coupons");
  await page.mouse.up();
  await expect(start).toHaveAttribute("aria-valuetext", /^v3, /);
  await expect(page.getByText("v3 → v8 · 6 versions")).toBeVisible();
  // Dropped: the card is the hover's again, on v3, not on where the drag began.
  await expect(card).toHaveAttribute("data-track-card", "hover");
  await expect(card).toContainText("v3");
  await expect(card).not.toContainText("Customers and orders");
  await expect(caption(page)).toHaveText("6 versions (v3 → v8) superposed, v8 highlighted");
});

test("keys go through the timeline as it reads, each control visibly focused", async ({ page }) => {
  await open(page);
  const order = [
    page.getByRole("slider", { name: "Range start" }),
    page.getByRole("slider", { name: "Range end" }),
    page.getByRole("slider", { name: "Cursor" }),
    page.getByRole("button", { name: "Play the range" }),
    // Next version is disabled at the range's end: skipped.
    page.getByRole("button", { name: "Previous version" }),
    page.getByRole("radio", { name: "Superposed" }),
    page.getByRole("button", { name: "Only changes" }),
    page.getByRole("searchbox", { name: "Filter tables by name" }),
  ];
  await order[0].focus();
  for (const [i, control] of order.entries()) {
    if (i > 0) await page.keyboard.press("Tab");
    await expect(control).toBeFocused();
    // The ring: a box shadow in the ring color, never nothing (a field with
    // a prefix draws it on its group).
    const ringed =
      (await control.getAttribute("type")) === "search" ? control.locator("..") : control;
    await expect(ringed).toHaveCSS("box-shadow", /rgb/);
  }
});

test("the age scale: six distinct colors, at least 3:1 on a table in both themes", async ({
  browser,
}) => {
  for (const colorScheme of ["light", "dark"] as const) {
    const page = await browser.newPage({ colorScheme });
    await open(page, "?fixture=large");
    const colors = await page.evaluate(() => {
      const probe = document.createElement("div");
      document.body.append(probe);
      const rgb = (token: string) => {
        probe.style.color = `var(${token})`;
        return getComputedStyle(probe).color;
      };
      const out = {
        card: rgb("--color-card"),
        canvas: rgb("--color-background"),
        eras: [1, 2, 3, 4, 5, 6].map((i) => rgb(`--color-era-${i}`)),
      };
      probe.remove();
      return out;
    });
    const channels = (c: string) =>
      c
        .match(/[\d.]+/g)!
        .slice(0, 3)
        .map(Number);
    const luminance = (c: string) => {
      const [r, g, b] = channels(c).map((v) => {
        const s = v / 255;
        return s <= 0.04045 ? s / 12.92 : ((s + 0.055) / 1.055) ** 2.4;
      });
      return 0.2126 * r + 0.7152 * g + 0.0722 * b;
    };
    const ratio = (a: string, b: string) => {
      const [hi, lo] = [luminance(a), luminance(b)].sort((x, y) => y - x);
      return (hi + 0.05) / (lo + 0.05);
    };
    expect(new Set(colors.eras).size).toBe(6);
    for (const era of colors.eras) {
      expect(ratio(era, colors.card), `${colorScheme} ${era} on the card`).toBeGreaterThanOrEqual(
        3,
      );
      expect(
        ratio(era, colors.canvas),
        `${colorScheme} ${era} on the canvas`,
      ).toBeGreaterThanOrEqual(3);
    }
    // Zoomed out (220 tables fit), every run of versions fills its tables
    // with its own color.
    const fills = await page
      .locator(".schema-graph[data-lod='overview'] .sg-node[data-era]")
      .evaluateAll((nodes) => [...new Set(nodes.map((n) => getComputedStyle(n).backgroundColor))]);
    expect(fills.length).toBeGreaterThanOrEqual(6);
    await page.close();
  }
});

test.describe("on a phone", () => {
  test.use({ viewport: { width: 390, height: 844 } });

  test("the sheet opened from the keyboard takes the keys, on the cursor", async ({ page }) => {
    await open(page);
    const toggle = page.getByRole("button", { name: "Show the timeline" });
    await toggle.focus();
    await page.keyboard.press("Enter");
    const cursor = page.getByRole("slider", { name: "Cursor" });
    await expect(cursor).toBeFocused();
    await page.keyboard.press("ArrowLeft");
    await expect(cursor).toHaveAttribute("aria-valuetext", /^v7, /);
    await expect(caption(page)).toHaveText("All 8 versions superposed, v7 highlighted");
    // Opened by touch or click, focus stays where it was.
    await page.getByRole("button", { name: "Hide the timeline" }).click();
    await page.getByRole("button", { name: "Show the timeline" }).click();
    await expect(page.getByRole("button", { name: "Hide the timeline" })).toBeFocused();
    expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(
      390,
    );
  });
});
