import { expect as base, type Locator, type Page } from "@playwright/test";

import { test } from "./fixtures";

// The live hero (site/public/hero, built from web/src/hero): romulus's own
// editor, graph, diff, migration and comments, running in the browser.

// Each test loads the editor and two wasm modules: generous under a loaded CI.
const expect = base.configure({ timeout: 15_000 });
test.describe.configure({ timeout: 90_000 });
const LOADED = { timeout: 60_000 };

/** Fails the test on any console error or uncaught exception (CSP violations included). */
function watchErrors(page: Page, allow?: RegExp) {
  const errors: string[] = [];
  page.on("console", (m) => {
    if (m.type() === "error" && !allow?.test(m.text())) errors.push(m.text());
  });
  page.on("pageerror", (e) => errors.push(String(e)));
  return errors;
}

/** On a desktop the island loads by itself once the page is idle. */
async function liveHero(page: Page): Promise<Locator> {
  await page.goto("/");
  await expect(page.locator(".hero-stage.is-live")).toBeAttached(LOADED);
  const live = page.locator("[data-hero-live]");
  await live.scrollIntoViewIfNeeded();
  await expect(live.getByTestId("compile-status")).toHaveText("Checked");
  return live;
}

const node = (live: Locator, table: string) =>
  live.locator(".react-flow__node", { has: live.page().getByText(table, { exact: true }) });

/** Puts the caret where a column of `customers` goes (the guide's first step), and types. */
async function addColumn(live: Locator, line: string) {
  await live.getByRole("button", { name: /Change the SQL or the graph/ }).click();
  await expect(live.locator(".cm-content").first()).toBeFocused();
  await expect(live.locator(".cm-activeLine").first()).toContainText("created_at");
  await live.page().keyboard.type(`${line}\n`);
}

test("the island takes the picture's place without moving anything", async ({ page }) => {
  const errors = watchErrors(page);
  await page.goto("/");
  const picture = page.locator(".product");
  const before = await picture.boundingBox();
  const height = await page.evaluate(() => document.documentElement.scrollHeight);
  await expect(page.locator(".hero-stage.is-live")).toBeAttached(LOADED);
  expect(await page.locator("[data-hero-live]").boundingBox()).toEqual(before);
  expect(await page.evaluate(() => document.documentElement.scrollHeight)).toBe(height);
  // The picture is out of the way, for eyes and for screen readers.
  await expect(picture).toHaveAttribute("aria-hidden", "true");
  await expect(page.getByRole("region", { name: "romulus, live in your browser" })).toBeVisible();
  // Said plainly near the way into the app: the parser, not a real Postgres.
  const note = page.locator(".hero-note");
  await expect(note).toContainText("Postgres 17's own parser");
  await expect(note.getByRole("link", { name: "Open the app" })).toHaveAttribute("href", "/app/signin");
  expect(errors).toEqual([]);
});

test("editing the SQL redraws the graph", async ({ page }) => {
  const errors = watchErrors(page);
  const live = await liveHero(page);
  await expect(node(live, "customers")).not.toContainText("phone");
  await addColumn(live, "  phone      text,");
  await expect(node(live, "customers")).toContainText("phone");
  await expect(live.getByRole("radio", { name: /^Changes/ })).toContainText("1");
  // A mistake is Postgres's error, located, and the graph keeps the last good schema.
  await page.keyboard.type("  plan_id    bigint REFERENCES plans,\n");
  await expect(live.getByTestId("compile-status")).toHaveText("1 error");
  await live.getByTestId("compile-status").click();
  await expect(page.getByText('relation "plans" does not exist')).toBeVisible();
  await expect(node(live, "customers")).toContainText("phone");
  expect(errors).toEqual([]);
});

test("save a version, change it, see the diff and the migration", async ({ page }) => {
  const errors = watchErrors(page);
  const live = await liveHero(page);
  await addColumn(live, "  phone      text,");
  await live.getByRole("button", { name: "Save v2", exact: true }).click();
  await expect(live.getByText("Saved v2")).toBeVisible();
  await expect(live.getByRole("button", { name: "Save v3", exact: true })).toBeVisible();

  await addColumn(live, "  country    char(2) NOT NULL DEFAULT 'FR',");
  await live.getByRole("radio", { name: /^Changes/ }).click();
  await expect(live.getByText("1 table changed")).toBeVisible();
  await expect(live.getByText("customers.country", { exact: false }).first()).toBeVisible();

  await live.getByRole("radio", { name: /^Migration/ }).click();
  const file = live.locator(".cm-content").first();
  await expect(file).toContainText("-- migrate:up");
  await expect(file).toContainText("ADD COLUMN country character(2)");
  await expect(file).toContainText("-- migrate:down");
  await expect(file).toContainText("DROP COLUMN country");
  await expect(live.getByRole("button", { name: /Change it again, see the diff/ })).toContainText(
    "Change it again",
  );
  expect(errors).toEqual([]);
});

/** The migration file's whole text: CodeMirror renders only the lines in view. */
const fileText = (live: Locator) =>
  live
    .locator(".cm-content")
    .first()
    .evaluate(
      (element) =>
        (
          element as unknown as {
            cmTile?: { view?: { state: { doc: { toString(): string } } } };
          }
        ).cmTile?.view?.state.doc.toString() ?? "",
    );

test("a column renamed in the SQL is a rename in the migration, unless switched off", async ({
  page,
}) => {
  const errors = watchErrors(page);
  const live = await liveHero(page);
  // `name` becomes `full_name`, typed in place.
  await live.getByRole("button", { name: /Change the SQL or the graph/ }).click();
  await expect(live.locator(".cm-activeLine").first()).toContainText("created_at");
  await page.keyboard.press("ArrowUp");
  await page.keyboard.press("Home");
  await page.keyboard.type("full_");
  await expect(node(live, "customers")).toContainText("full_name");

  await live.getByRole("radio", { name: /^Migration/ }).click();
  const rename = live.getByRole("switch", { name: "Renamed name to full_name?" });
  await expect(rename).toBeChecked();
  await expect.poll(() => fileText(live)).toContain("RENAME COLUMN name TO full_name;");
  await rename.click();
  await expect(rename).not.toBeChecked();
  await expect.poll(() => fileText(live)).toContain("DROP COLUMN name;");
  await expect(live.getByText(/name is dropped and full_name added/)).toBeVisible();
  expect(errors).toEqual([]);
});

test("a --- line in the SQL becomes a comment on the table below", async ({ page }) => {
  const errors = watchErrors(page);
  const live = await liveHero(page);
  await live.getByRole("button", { name: /Comment with a --- line/ }).click();
  await expect(live.locator(".cm-content").first()).toBeFocused();
  await expect(live.locator(".cm-activeLine").first()).toHaveText("--- ");
  await page.keyboard.type("Should email be citext?");
  await page.keyboard.press("Enter");
  await expect(live.locator(".cm-content")).toContainText("--- You: Should email be citext?");
  // A thread on customers, in the panel, which takes replies and resolves.
  await live.getByRole("button", { name: /^Comments/ }).click();
  const panel = live.getByRole("complementary", { name: "Comments" });
  // The seeded discussion is there too.
  await expect(panel.getByText("Partial refunds", { exact: false })).toBeVisible();
  await panel.getByText("Should email be citext?").click();
  await panel.getByPlaceholder("Reply…").fill("Yes, with the extension.");
  await page.keyboard.press("Enter");
  await expect(panel.getByText("Yes, with the extension.")).toBeVisible();
  await panel.getByRole("button", { name: "Resolve" }).click();
  await expect(live.getByText("Thread resolved")).toBeVisible();
  expect(errors).toEqual([]);
});

test("the demo is kept in this browser until Reset", async ({ page }) => {
  const live = await liveHero(page);
  await addColumn(live, "  phone      text,");
  await expect(node(live, "customers")).toContainText("phone");
  await page.waitForTimeout(600);
  await page.reload();
  const again = page.locator("[data-hero-live]");
  await expect(page.locator(".hero-stage.is-live")).toBeAttached(LOADED);
  await expect(node(again, "customers")).toContainText("phone");
  await again.getByRole("button", { name: "Reset", exact: true }).click();
  await expect(node(again, "customers")).not.toContainText("phone");
});

test("on a phone it waits for a tap, then one row of tabs switches everything", async ({ page }) => {
  const errors = watchErrors(page);
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto("/");
  await page.waitForLoadState("load");
  await expect(page.locator(".hero-stage.is-live")).not.toBeAttached();
  await page.getByRole("button", { name: "Try it live" }).click();
  await expect(page.locator(".hero-stage.is-live")).toBeAttached(LOADED);
  const live = page.locator("[data-hero-live]");
  const tabs = live.getByRole("radiogroup", { name: "View" });
  await expect(tabs.getByRole("radio")).toHaveText(["SQL", "Graph", "Changes", "Migration"]);
  await expect(live.locator(".react-flow")).toBeVisible();
  await tabs.getByRole("radio", { name: "SQL" }).click();
  await expect(live.locator(".cm-content").first()).toBeVisible();
  await tabs.getByRole("radio", { name: /^Changes/ }).click();
  await expect(live.getByText("No changes since v1")).toBeVisible();
  await tabs.getByRole("radio", { name: "Migration" }).click();
  await expect(live.getByText("Tool")).toBeVisible();
  const overflow = await page.evaluate(
    () => document.documentElement.scrollWidth - document.documentElement.clientWidth,
  );
  expect(overflow).toBe(0);
  expect(errors).toEqual([]);
});

test("when the island cannot load, the picture stays", async ({ page }) => {
  const errors = watchErrors(page, /Failed to load resource/);
  await page.route("**/hero/**", (route) => route.abort());
  await page.goto("/");
  await page.waitForLoadState("load");
  await page.waitForTimeout(2500);
  await expect(page.locator(".hero-stage.is-live")).not.toBeAttached();
  await expect(page.getByRole("img", { name: /romulus draft of a billing schema/ })).toBeVisible();
  await expect(page.locator("[data-hero-live]")).toBeHidden();
  // On a phone the button says so, and offers another try.
  await page.setViewportSize({ width: 390, height: 844 });
  await page.reload();
  await page.getByRole("button", { name: "Try it live" }).click();
  await expect(page.getByRole("button", { name: "Try again" })).toBeVisible();
  await expect(page.getByRole("status").and(page.locator("[data-hero-status]"))).toHaveText("The live editor did not load.");
  await expect(page.getByRole("img", { name: /romulus draft of a billing schema/ })).toBeVisible();
  expect(errors).toEqual([]);
});

// ---- Failures -------------------------------------------------------------------

/**
 * Lets a test break romulus-wasm on purpose: `window.__heroTraps` makes its
 * next compiles trap as a panic does, `window.__heroResetFails` makes the
 * fresh instance after a trap fail too.
 */
async function breakableEngine(page: Page) {
  await page.addInitScript(() => {
    const w = window as unknown as { __heroTraps: number; __heroResetFails: boolean };
    w.__heroTraps = 0;
    w.__heroResetFails = false;
    const wrap = (instance: WebAssembly.Instance) => {
      const exports = instance.exports as Record<string, unknown>;
      // romulus-wasm is the module exporting `syntaxError`.
      if (typeof exports.syntaxError !== "function") return instance;
      const compile = exports.compile as (...args: unknown[]) => unknown;
      return {
        exports: {
          ...exports,
          compile: (...args: unknown[]) => {
            if (w.__heroTraps > 0) {
              w.__heroTraps -= 1;
              throw new WebAssembly.RuntimeError("unreachable");
            }
            return compile(...args);
          },
        },
      } as unknown as WebAssembly.Instance;
    };
    const streaming = WebAssembly.instantiateStreaming.bind(WebAssembly);
    WebAssembly.instantiateStreaming = async (source, imports) => {
      const result = await streaming(source, imports);
      return { module: result.module, instance: wrap(result.instance) };
    };
    const instantiate = WebAssembly.instantiate.bind(WebAssembly) as (
      ...args: unknown[]
    ) => Promise<unknown>;
    (WebAssembly as { instantiate: unknown }).instantiate = async (...args: unknown[]) => {
      const result = (await instantiate(...args)) as
        | WebAssembly.Instance
        | WebAssembly.WebAssemblyInstantiatedSource;
      if (result instanceof WebAssembly.Instance) return wrap(result);
      return { module: result.module, instance: wrap(result.instance) };
    };
    const Instance = WebAssembly.Instance;
    (WebAssembly as { Instance: unknown }).Instance = new Proxy(Instance, {
      construct(target, args: [WebAssembly.Module, WebAssembly.Imports?]) {
        if (w.__heroResetFails) throw new WebAssembly.CompileError("no fresh instance");
        return wrap(new target(...args));
      },
    });
  });
}

const setFlags = (page: Page, flags: { traps?: number; resetFails?: boolean }) =>
  page.evaluate(({ traps, resetFails }) => {
    const w = window as unknown as { __heroTraps: number; __heroResetFails: boolean };
    if (traps !== undefined) w.__heroTraps = traps;
    if (resetFails !== undefined) w.__heroResetFails = resetFails;
  }, flags);

test("what this browser kept is forgotten when it does not read, and the demo starts over", async ({
  page,
}) => {
  const errors = watchErrors(page);
  await page.addInitScript(() => {
    localStorage.setItem(
      "romulus:hero:1",
      JSON.stringify({
        sql: "CREATE TABLE nothing_else (id int);",
        layout: { "public.nothing_else": { x: "left", y: 0 } },
        groups: {},
        threads: {},
        comments: {},
        versions: [{ number: 1, sql: "", layout: {}, groups: {}, savedAt: "now", by: "x" }],
      }),
    );
  });
  const live = await liveHero(page);
  await expect(node(live, "customers")).toBeVisible();
  await expect(node(live, "nothing_else")).toHaveCount(0);
  expect(errors).toEqual([]);
});

test("when the engine fails on a text, it says so and starts again", async ({ page }) => {
  const errors = watchErrors(page, /live editor's engine failed|unreachable/);
  await breakableEngine(page);
  const live = await liveHero(page);
  await setFlags(page, { traps: 1 });
  await addColumn(live, "  phone      text,");
  await expect(live.getByTestId("compile-status")).toHaveText("1 error");
  await live.getByTestId("compile-status").click();
  await expect(page.getByText("The browser preview could not check this SQL.")).toBeVisible();
  await page.keyboard.press("Escape");
  // A fresh engine checks the next edit.
  await live.locator(".cm-content").first().focus();
  await page.keyboard.type("  plan       text,\n");
  await expect(live.getByTestId("compile-status")).toHaveText("Checked");
  await expect(node(live, "customers")).toContainText("phone");
  await expect(node(live, "customers")).toContainText("plan");
  expect(errors).toEqual([]);
});

test("an engine that cannot start again takes the island down, and the picture comes back", async ({
  page,
}) => {
  const errors = watchErrors(page, /engine|live editor stopped|unreachable|fresh instance/);
  await breakableEngine(page);
  const live = await liveHero(page);
  await addColumn(live, "  phone      text,");
  await expect(node(live, "customers")).toContainText("phone");
  await setFlags(page, { traps: 99, resetFails: true });
  await page.keyboard.type("  plan       text,\n");
  await expect(page.locator(".hero-stage.is-live")).not.toBeAttached();
  await expect(page.getByRole("img", { name: /romulus draft of a billing schema/ })).toBeVisible();
  await expect(page.getByRole("status").and(page.locator("[data-hero-status]"))).toHaveText(/ran into a problem and stopped/);
  // What it kept is forgotten: trying again starts from the seed.
  expect(await page.evaluate(() => localStorage.getItem("romulus:hero:1"))).toBeNull();
  await setFlags(page, { traps: 0, resetFails: false });
  await page.getByRole("button", { name: "Try again" }).click();
  await expect(page.locator(".hero-stage.is-live")).toBeAttached(LOADED);
  await expect(page.getByRole("status").and(page.locator("[data-hero-status]"))).toHaveText("");
  const again = page.locator("[data-hero-live]");
  await expect(again.getByTestId("compile-status")).toHaveText("Checked");
  await expect(node(again, "customers")).not.toContainText("phone");
  expect(errors).toEqual([]);
});

// ---- The guide and the graph on a phone ---------------------------------------------

/** Scrolls the page (at once: the site scrolls smoothly) so the island's header is out of view. */
const scrollHeaderAway = (page: Page) =>
  page.evaluate(() => {
    const island = document.querySelector("[data-hero-live]")!;
    window.scrollBy({ top: island.getBoundingClientRect().top + 160, behavior: "instant" });
  });

test("on a phone, each step of the guide does what it says, wherever the page is", async ({
  page,
}) => {
  const errors = watchErrors(page);
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto("/");
  await page.getByRole("button", { name: "Try it live" }).click();
  await expect(page.locator(".hero-stage.is-live")).toBeAttached(LOADED);
  const live = page.locator("[data-hero-live]");
  const guide = live.getByRole("list", { name: /^Try it/ });
  await expect(live.getByTestId("compile-status")).toHaveText("Checked");

  // Nothing to save yet: the step says so and puts the caret where a change goes.
  await scrollHeaderAway(page);
  await guide.getByRole("button", { name: "Save v2" }).click();
  await expect(live.getByText("Change something first")).toBeVisible();
  await expect(live.locator(".cm-activeLine").first()).toContainText("created_at");
  await expect(live.locator(".cm-activeLine").first()).toBeInViewport();
  await page.keyboard.type("  phone      text,\n");
  await expect(live.getByRole("button", { name: "Save v2", exact: true }).first()).not.toHaveAttribute(
    "aria-disabled",
  );

  // Saved from the guide, with the window's top brought back into view.
  await scrollHeaderAway(page);
  await guide.getByRole("button", { name: "Save v2" }).click();
  await expect(live.getByText("Saved v2")).toBeVisible();
  await expect(live.getByRole("button", { name: "Save v3", exact: true }).first()).toBeInViewport();

  // The comment step's line is where the visitor can see it.
  await scrollHeaderAway(page);
  await guide.getByRole("button", { name: "Comment" }).click();
  await expect(live.locator(".cm-activeLine").first()).toHaveText("--- ");
  await expect(live.locator(".cm-activeLine").first()).toBeInViewport();
  expect(errors).toEqual([]);
});

/** Whether `table` is whole inside the graph pane, clear of its zoom controls and map. */
async function inClearView(live: Locator, table: Locator) {
  const box = await table.boundingBox();
  const pane = await live.locator(".react-flow").first().boundingBox();
  if (!box || !pane) return false;
  const inside =
    box.x >= pane.x &&
    box.y >= pane.y &&
    box.x + box.width <= pane.x + pane.width &&
    box.y + box.height <= pane.y + pane.height;
  // The map shows only while part of the graph is out of view.
  const map = live.locator(".react-flow__minimap");
  const covers = [
    await live.getByRole("button", { name: "Zoom out" }).boundingBox(),
    (await map.count()) ? await map.first().boundingBox() : null,
  ];
  const clear = covers.every(
    (c) =>
      !c ||
      box.x + box.width <= c.x ||
      c.x + c.width <= box.x ||
      box.y + box.height <= c.y ||
      c.y + c.height <= box.y,
  );
  return inside && clear;
}

test("a table typed in the SQL comes into view, even after the view moved", async ({ page }) => {
  const errors = watchErrors(page);
  const live = await liveHero(page);
  // The caret in customers selects it: the graph pans to it (it no longer fits by itself).
  await addColumn(live, "  phone      text,");
  await page.keyboard.press("ControlOrMeta+End");
  await page.keyboard.type(
    "\nCREATE TABLE refunds (id bigint PRIMARY KEY, payment_id bigint REFERENCES payments);",
  );
  const refunds = node(live, "refunds");
  await expect(refunds).toBeVisible();
  await expect.poll(() => inClearView(live, refunds), { timeout: 10_000 }).toBe(true);
  expect(errors).toEqual([]);
});

test("on a phone, a table typed in the SQL is in view on the graph", async ({ page }) => {
  const errors = watchErrors(page);
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto("/");
  await page.getByRole("button", { name: "Try it live" }).click();
  await expect(page.locator(".hero-stage.is-live")).toBeAttached(LOADED);
  const live = page.locator("[data-hero-live]");
  const tabs = live.getByRole("radiogroup", { name: "View" });
  await expect(live.getByTestId("compile-status")).toHaveText("Checked");

  await tabs.getByRole("radio", { name: "SQL" }).click();
  await live.locator(".cm-content").first().click();
  await page.keyboard.press("ControlOrMeta+End");
  await page.keyboard.type(
    "\nCREATE TABLE refunds (id bigint PRIMARY KEY, payment_id bigint REFERENCES payments);\n",
  );
  await expect(live.getByTestId("compile-status")).toHaveText("Checked");
  await tabs.getByRole("radio", { name: "Graph" }).click();

  const refunds = node(live, "refunds");
  await expect(refunds).toBeVisible();
  await expect.poll(() => inClearView(live, refunds), { timeout: 10_000 }).toBe(true);
  expect(errors).toEqual([]);
});
