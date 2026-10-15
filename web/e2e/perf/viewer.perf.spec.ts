/// <reference lib="dom" />
import { expect, type Page, test } from "@playwright/test";

// How the schema viewer holds up at 300 tables, on a production build:
// time to first draw, then frame times while zooming in to 2x and panning at
// 2x and 1x. Run headed (`npm --prefix web run perf`): headless Chromium
// rasterizes in software and its frame times say nothing about a real screen.
// PERF_REPORT_ONLY=1 prints the numbers without failing on the budgets.

const BUDGET = { maxFrame: 100, p95: 20 };
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

const zoomOf = async (page: Page) => {
  const label = await page.getByRole("button", { name: /^Zoom \d+%/ }).getAttribute("aria-label");
  return Number(/Zoom (\d+)%/.exec(label ?? "")?.[1] ?? 0) / 100;
};

/** Budgets apply to zooming in and panning; other phases are reported only. */
function report(name: string, f: Frames, budgeted = true) {
  const line = `${name}: ${f.count} frames, median ${f.median.toFixed(1)} ms, p95 ${f.p95.toFixed(1)} ms, max ${f.max.toFixed(1)} ms`;
  console.log(line);
  test.info().annotations.push({ type: "perf", description: line });
  if (budgeted && !reportOnly) {
    expect.soft(f.max, `${name}: longest frame`).toBeLessThan(BUDGET.maxFrame);
    expect.soft(f.p95, `${name}: p95 frame`).toBeLessThan(BUDGET.p95);
  }
}

test("300 tables: first draw, zoom in, pan", async ({ page }) => {
  const started = Date.now();
  await page.goto("./dev/viewer?fixture=large");
  await expect(page.getByText(/Arranging 300 tables|Loading the schema/).first()).toBeVisible();
  const pending = Date.now() - started;
  const tables = page.locator(".react-flow__node-table");
  await expect(tables.first()).toBeVisible({ timeout: 30_000 });
  const firstTable = Date.now() - started;
  await expect(tables).toHaveCount(300);
  const allTables = Date.now() - started;
  const timings = await page.evaluate(() =>
    [...performance.getEntriesByType("measure"), ...performance.getEntriesByType("mark")]
      .filter((e) => e.name.startsWith("schema graph"))
      .map((e) => `${e.name} ${Math.round(e.startTime)}+${Math.round(e.duration)} ms`)
      .join("; "),
  );
  const load = `load: pending state ${pending} ms, first table ${firstTable} ms, all 300 ${allTables} ms (${timings})`;
  console.log(load);
  test.info().annotations.push({ type: "perf", description: load });

  await page.waitForTimeout(1500);
  const pane = (await page.locator(".react-flow__pane").boundingBox())!;
  await page.mouse.move(pane.x + pane.width / 2, pane.y + pane.height / 2);

  report(
    "zoom in to 2x",
    await frames(page, async () => {
      for (let i = 0; i < 80 && (await zoomOf(page)) < 2; i++) {
        await page.keyboard.down("Control");
        await page.mouse.wheel(0, -60);
        await page.keyboard.up("Control");
        await page.waitForTimeout(16);
      }
      await page.waitForTimeout(500);
    }),
  );
  expect(await zoomOf(page)).toBeGreaterThanOrEqual(1.9);

  const pan = async () => {
    for (let i = 0; i < 90; i++) {
      await page.mouse.wheel(i % 30 < 15 ? 80 : -80, i % 20 < 10 ? 40 : -40);
      await page.waitForTimeout(16);
    }
    await page.waitForTimeout(300);
  };
  report("pan at 2x", await frames(page, pan));

  await page.getByRole("button", { name: /^Zoom \d+%, reset to 100%/ }).click();
  await page.waitForTimeout(800);
  report("pan at 1x", await frames(page, pan));

  // Every table comes back on screen and mounts during the animation.
  report(
    "zoom out to fit (reported, no budget)",
    await frames(page, async () => {
      await page.getByRole("button", { name: "Fit to screen" }).click();
      await page.waitForTimeout(900);
    }),
    false,
  );
});
