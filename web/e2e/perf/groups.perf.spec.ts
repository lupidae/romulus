/// <reference lib="dom" />
import { expect, type Page, test } from "@playwright/test";

// Dragging a group at 300 tables (six groups of about 50, one per schema), on
// a production build of the dev editor: zoomed out, by the area label, with
// every table on screen; zoomed in, by the frame's label, with the rows
// drawn. Its tables follow the pointer every frame, the edges between them
// shift with them and the edges leaving the group are routed again. Headed,
// as viewer.perf.spec.ts (same budgets); PERF_REPORT_ONLY=1 only prints.

const BUDGET = { maxFrame: 100, p95: 20 };
const reportOnly = process.env.PERF_REPORT_ONLY === "1";

interface Frames {
  count: number;
  median: number;
  p95: number;
  max: number;
}

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

function report(name: string, f: Frames) {
  const line = `${name}: ${f.count} frames, median ${f.median.toFixed(1)} ms, p95 ${f.p95.toFixed(1)} ms, max ${f.max.toFixed(1)} ms`;
  console.log(line);
  test.info().annotations.push({ type: "perf", description: line });
  if (!reportOnly) {
    expect.soft(f.max, `${name}: longest frame`).toBeLessThan(BUDGET.maxFrame);
    expect.soft(f.p95, `${name}: p95 frame`).toBeLessThan(BUDGET.p95);
  }
}

/** Presses at (x, y), moves in a loop for about 1.5 s at 60 Hz, lets go. */
async function dragFrom(page: Page, x: number, y: number) {
  await page.mouse.move(x, y);
  await page.mouse.down();
  for (let i = 1; i <= 90; i++) {
    const t = i / 90;
    await page.mouse.move(x + Math.sin(t * Math.PI * 2) * 160, y + t * 120);
    await page.waitForTimeout(16);
  }
  await page.mouse.up();
  await page.waitForTimeout(400);
}

const zoomOf = async (page: Page) => {
  const label = await page.getByRole("button", { name: /^Zoom \d+%/ }).getAttribute("aria-label");
  return Number(/Zoom (\d+)%/.exec(label ?? "")?.[1] ?? 0) / 100;
};

const table = (page: Page, key: string) => page.locator(`.react-flow__node[data-id="${key}"]`);

test("300 tables: a group of 50 dragged zoomed out and zoomed in", async ({ page }) => {
  await page.goto("./dev/editor?fixture=large&groups=schemas");
  const tables = page.locator(".react-flow__node-table");
  await expect(tables).toHaveCount(300, { timeout: 60_000 });
  await expect(page.locator("[data-group]")).toHaveCount(6);
  await page.waitForTimeout(1500);

  // Zoomed out: the area label is the handle.
  const label = page.locator("[data-area-handle]").filter({ hasText: "identity" });
  const at = (await label.boundingBox())!;
  const before = (await table(page, "identity.users").boundingBox())!;
  report(
    "drag a group, zoomed out (300 tables on screen)",
    await frames(page, () => dragFrom(page, at.x + at.width / 2, at.y + at.height / 2)),
  );
  const after = (await table(page, "identity.users").boundingBox())!;
  expect(after.y).toBeGreaterThan(before.y);

  // Zoomed in on the group's corner: its frame's label, rows drawn.
  const corner = (await page.locator('[data-group="schema-identity"]').boundingBox())!;
  await page.mouse.move(corner.x + 40, corner.y + 4);
  for (let i = 0; i < 80 && (await zoomOf(page)) < 0.55; i++) {
    await page.keyboard.down("Control");
    await page.mouse.wheel(0, -60);
    await page.keyboard.up("Control");
    await page.waitForTimeout(16);
  }
  await page.waitForTimeout(800);
  const handle = page.locator('[data-group="schema-identity"] [data-group-handle]');
  const h = (await handle.boundingBox())!;
  report(
    "drag a group, zoomed in (rows drawn)",
    await frames(page, () => dragFrom(page, Math.max(h.x, 0) + 120, h.y + h.height / 2)),
  );
});
