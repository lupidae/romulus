/// <reference lib="dom" />
import { expect, type Page, test } from "@playwright/test";

// How the diff view holds up at 300 tables (a tenth changed, a few removed, a
// dozen added), on a production build: time until the change list, the graph
// and the SQL diff are drawn, then frame times while picking changes (j, and
// clicks in the list) and scrolling the SQL diff and the list. Run headed
// (`npm --prefix web run perf`), like the viewer's spec.
// PERF_REPORT_ONLY=1 prints the numbers without failing on the budgets.

const BUDGET = { maxFrame: 100, p95: 20, firstRender: 2000 };
const reportOnly = process.env.PERF_REPORT_ONLY === "1";

interface Frames {
  count: number;
  median: number;
  p95: number;
  max: number;
}

/** rAF-to-rAF intervals while `action` runs. */
async function frames(page: Page, action: () => Promise<void>): Promise<Frames> {
  await page.evaluate(() => {
    const w = window as unknown as { __frames: number[]; __recording: boolean };
    w.__frames = [];
    w.__recording = true;
    let last = performance.now();
    const tick = (now: number) => {
      w.__frames.push(now - last);
      last = now;
      if (w.__recording) requestAnimationFrame(tick);
    };
    requestAnimationFrame(tick);
  });
  await action();
  const intervals = await page.evaluate(() => {
    const w = window as unknown as { __frames: number[]; __recording: boolean };
    w.__recording = false;
    return w.__frames.slice(1);
  });
  const sorted = [...intervals].sort((a, b) => a - b);
  const at = (q: number) => sorted[Math.min(sorted.length - 1, Math.floor(sorted.length * q))] ?? 0;
  return { count: sorted.length, median: at(0.5), p95: at(0.95), max: at(1) };
}

function note(line: string) {
  console.log(line);
  test.info().annotations.push({ type: "perf", description: line });
}

function report(name: string, f: Frames, budgeted = true) {
  note(
    `${name}: ${f.count} frames, median ${f.median.toFixed(1)} ms, p95 ${f.p95.toFixed(1)} ms, max ${f.max.toFixed(1)} ms`,
  );
  if (budgeted && !reportOnly) {
    expect.soft(f.max, `${name}: longest frame`).toBeLessThan(BUDGET.maxFrame);
    expect.soft(f.p95, `${name}: p95 frame`).toBeLessThan(BUDGET.p95);
  }
}

/** Wheel steps over the middle of `box`, back and forth. */
async function scroll(page: Page, box: { x: number; y: number; width: number; height: number }) {
  await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
  for (let i = 0; i < 80; i++) {
    await page.mouse.wheel(0, i % 40 < 30 ? 160 : -240);
    await page.waitForTimeout(16);
  }
  await page.waitForTimeout(300);
}

/** Opens the diff of `fixture` and reports when each pane is drawn; returns how many changes it lists. */
async function load(page: Page, fixture: string): Promise<number> {
  // Long tasks from the start, load included.
  await page.addInitScript(() => {
    const w = window as unknown as { __long: number[] };
    w.__long = [];
    new PerformanceObserver((list) => {
      for (const e of list.getEntries()) w.__long.push(Math.round(e.duration));
    }).observe({ type: "longtask", buffered: true });
  });
  const started = Date.now();
  await page.goto(`./dev/diff?fixture=${fixture}`);
  const rows = page.locator("[data-change]");
  await expect(rows.first()).toBeVisible({ timeout: 30_000 });
  const list = Date.now() - started;
  const tables = page.locator(".react-flow__node-table");
  await expect(tables.first()).toBeVisible({ timeout: 30_000 });
  const graph = Date.now() - started;
  const sql = page.getByRole("region", { name: "SQL diff" }).locator(".cm-editor").first();
  await expect(sql).toBeVisible({ timeout: 30_000 });
  const text = Date.now() - started;
  const changes = await rows.count();
  const longTasks = await page.evaluate(() =>
    (window as unknown as { __long: number[] }).__long.sort((a, b) => b - a).slice(0, 5),
  );
  const marks = await page.evaluate(() =>
    [...performance.getEntriesByType("measure"), ...performance.getEntriesByType("mark")]
      .filter((e) => e.name.startsWith("schema graph") || e.name.startsWith("sql diff"))
      .map((e) => `${e.name} ${Math.round(e.startTime)}+${Math.round(e.duration)} ms`)
      .join("; "),
  );
  note(
    `load (${fixture}): change list ${list} ms (${changes} changes), graph ${graph} ms, SQL diff ${text} ms; longest tasks ${longTasks.join(", ") || "none"} ms (${marks})`,
  );
  if (!reportOnly) {
    for (const [name, ms] of [
      ["change list", list],
      ["graph", graph],
      ["SQL diff", text],
    ] as const)
      expect.soft(ms, `${name}: first render`).toBeLessThan(BUDGET.firstRender);
  }
  await page.waitForTimeout(1500);
  return changes;
}

/** j, `count` times: the graph zooms, the SQL scrolls, the list follows. */
async function pickWithJ(page: Page, count: number) {
  for (let i = 0; i < count; i++) {
    await page.keyboard.press("j");
    await page.waitForTimeout(150);
  }
  await page.waitForTimeout(600);
}

test("300-table diff: first render, picking changes, scrolling", async ({ page }) => {
  const changes = await load(page, "large");
  expect(changes).toBeGreaterThan(25);
  const rows = page.locator("[data-change]");

  // The first pick leaves the overview of every table for a close-up (like
  // the viewer's zoom to fit, reported only); the next ones are budgeted.
  report(
    "first pick, from the overview (reported, no budget)",
    await frames(page, () => pickWithJ(page, 1)),
    false,
  );
  report("pick changes with j", await frames(page, () => pickWithJ(page, 20)));

  report(
    "click changes in the list",
    await frames(page, async () => {
      for (let i = 0; i < 8; i++) {
        await rows.nth(i * 3).click();
        await page.waitForTimeout(250);
      }
      await page.waitForTimeout(600);
    }),
  );

  const listBox = (await page.getByRole("region", { name: /^Changes/ }).boundingBox())!;
  report("scroll the change list", await frames(page, () => scroll(page, listBox)));

  const sqlBox = (await page.getByRole("region", { name: "SQL diff" }).boundingBox())!;
  report("scroll the SQL diff, unified", await frames(page, () => scroll(page, sqlBox)));

  // Side by side, with the SQL pane alone: the merge view is built again.
  report(
    "switch to side by side, SQL only (reported, no budget)",
    await frames(page, async () => {
      await page.getByRole("button", { name: "Layout" }).click();
      await page.getByRole("menuitemradio", { name: "Text only" }).click();
      await page.getByRole("radio", { name: "Side by side" }).click();
      await expect(page.getByRole("textbox", { name: "SQL of v1" })).toBeVisible();
      await page.waitForTimeout(600);
    }),
    false,
  );
  const wideBox = (await page.getByRole("region", { name: "SQL diff" }).boundingBox())!;
  report("scroll the SQL diff, side by side", await frames(page, () => scroll(page, wideBox)));
  report("pick changes with j, side by side", await frames(page, () => pickWithJ(page, 12)));
});

test("every table changed: a list of 300 changes", async ({ page }) => {
  const changes = await load(page, "large-all");
  expect(changes).toBeGreaterThan(250);
  report(
    "first pick, from the overview (reported, no budget)",
    await frames(page, () => pickWithJ(page, 1)),
    false,
  );
  report("pick changes with j (300 in the list)", await frames(page, () => pickWithJ(page, 20)));
  const listBox = (await page.getByRole("region", { name: /^Changes/ }).boundingBox())!;
  report("scroll the change list (300 changes)", await frames(page, () => scroll(page, listBox)));
});
