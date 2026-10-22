/// <reference lib="dom" />
import { expect, type Page, test } from "@playwright/test";

// How the time machine holds up on a long history (220 tables over 120
// versions, the dev route's `large` fixture), on a production build: time
// until the graph is drawn, then frame times while the cursor steps through
// versions (each a new frame of the graph: tables appearing, going, changing),
// while it plays, and when the mode switches. Run headed
// (`npm --prefix web run perf`), like the viewer's spec.
// PERF_REPORT_ONLY=1 prints the numbers without failing on the budgets.

const BUDGET = { maxFrame: 100, p95: 20, firstRender: 4000 };
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

/** The cursor, `count` versions back or forward with the arrow keys. */
async function step(page: Page, key: "ArrowLeft" | "ArrowRight", count: number, pause = 140) {
  for (let i = 0; i < count; i++) {
    await page.keyboard.press(key);
    await page.waitForTimeout(pause);
  }
  await page.waitForTimeout(600);
}

test("220 tables over 120 versions: first render, scrubbing, playing, modes", async ({ page }) => {
  const started = Date.now();
  await page.goto("./dev/time-machine?fixture=large");
  await expect(page.locator(".react-flow__node-table").first()).toBeVisible({ timeout: 60_000 });
  const drawn = Date.now() - started;
  note(`load: graph drawn in ${drawn} ms (the first layout of every table of the history)`);
  if (!reportOnly) expect.soft(drawn, "first render").toBeLessThan(BUDGET.firstRender);
  await page.waitForTimeout(1500);

  const cursor = page.getByRole("slider", { name: "Cursor" });
  await cursor.focus();
  // From the end to the middle in one jump (reported: a third of the history changes at once).
  report(
    "jump to the start (reported, no budget)",
    await frames(page, async () => {
      await page.keyboard.press("Home");
      await page.waitForTimeout(800);
    }),
    false,
  );
  report("scrub forward, overview", await frames(page, () => step(page, "ArrowRight", 30)));
  report("scrub back, overview", await frames(page, () => step(page, "ArrowLeft", 15)));

  // Zoomed in, rows are drawn: appearing columns animate.
  await page.getByRole("button", { name: "Zoom in" }).click();
  await page.getByRole("button", { name: "Zoom in" }).click();
  await page.getByRole("button", { name: "Zoom in" }).click();
  await page.waitForTimeout(1200);
  await cursor.focus();
  report("scrub forward, zoomed in", await frames(page, () => step(page, "ArrowRight", 30)));

  report(
    "play 6 s",
    await frames(page, async () => {
      await page.getByRole("button", { name: "Play the range" }).click();
      await page.waitForTimeout(6000);
      await page.getByRole("button", { name: "Pause" }).click();
      await page.waitForTimeout(400);
    }),
  );

  report(
    "switch to one version and back (reported, no budget)",
    await frames(page, async () => {
      await page.getByRole("radio", { name: "One version" }).click();
      await page.waitForTimeout(800);
      await page.getByRole("radio", { name: "All versions" }).click();
      await page.waitForTimeout(800);
    }),
    false,
  );
});
