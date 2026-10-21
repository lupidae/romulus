#!/usr/bin/env node
// The pricing section's plan cards, rendered from the one place the plans
// are written: the JSON block `#plans-data` in public/index.html. Change a
// price or a limit there, then run this; the cards between the `plans`
// markers are rewritten, and nothing else in the page is touched.
//
//   node tools/pricing.mjs           rewrite the cards (npm --prefix site run pricing)
//   node tools/pricing.mjs --check   exit 1 if they differ from the JSON (the e2e test runs it too)
//
// The cards are plain HTML, so the page needs no script to show them, and
// both periods' prices are in it: the Monthly/Yearly switch is CSS alone.
import { readFile, writeFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";

export const page = fileURLToPath(new URL("../public/index.html", import.meta.url));

const DATA = /<script type="application\/json" id="plans-data">([\s\S]*?)<\/script>/;
const START = "<!-- plans: generated from #plans-data by site/tools/pricing.mjs -->";
const END = "<!-- /plans -->";

const escape = (text) =>
  String(text).replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");

/** The plans as the page writes them, checked enough that a typo fails loudly. */
export function readPlans(html) {
  const match = DATA.exec(html);
  if (!match) throw new Error('index.html: no <script type="application/json" id="plans-data">');
  const data = JSON.parse(match[1]);
  for (const key of ["currency", "yearly_months", "signup", "talk", "limits", "plans"])
    if (data[key] === undefined) throw new Error(`#plans-data: "${key}" is missing`);
  for (const plan of data.plans) {
    for (const key of ["id", "name", "for", "monthly", "action"])
      if (plan[key] === undefined) throw new Error(`#plans-data: plan ${plan.id ?? "?"} has no "${key}"`);
    for (const { key } of data.limits)
      if (plan.limits?.[key] === undefined)
        throw new Error(`#plans-data: plan ${plan.id} has no limit "${key}"`);
  }
  return data;
}

/** "1,490 €": numbers written as the plans table writes them, the currency after, never on its own line. */
export const price = (amount, currency) => `${amount.toLocaleString("en-US")}\u00a0${currency}`;

/** The cards' HTML, indented to sit in the pricing section's container. */
export function render(data) {
  const pad = (n) => " ".repeat(n);
  const cards = data.plans.map((plan) => {
    const yearly = plan.monthly * data.yearly_months;
    const id = `plan-${plan.id}`;
    const money = (amount) => escape(price(amount, data.currency)).replace(/\u00a0/g, "&nbsp;");
    const actions = [
      `<a class="button ${plan.recommended ? "primary" : "secondary"}" href="${escape(data.signup)}">${escape(plan.action)}</a>`,
    ];
    if (plan.talk) actions.push(`<a class="plan-talk" href="${escape(data.talk)}">Talk to us</a>`);
    const lines = [
      `<article class="plan${plan.recommended ? " is-recommended" : ""}" aria-labelledby="${id}">`,
      `  <div class="plan-head">`,
      `    <h3 id="${id}">${escape(plan.name)}</h3>`,
      ...(plan.recommended ? [`    <span class="plan-pick">Recommended</span>`] : []),
      `  </div>`,
      `  <p class="plan-for">${escape(plan.for)}</p>`,
      `  <p class="plan-price">`,
      `    <span class="per-month"><b>${money(plan.monthly)}</b> per month</span>`,
      `    <span class="per-year"><b>${money(yearly)}</b> per year</span>`,
      `  </p>`,
      plan.monthly
        ? `  <p class="plan-alt"><span class="per-month">or ${money(yearly)} a year</span><span class="per-year">or ${money(plan.monthly)} a month</span></p>`
        : `  <p class="plan-alt">${escape(plan.note ?? "")}</p>`,
      `  <div class="plan-actions">`,
      ...actions.map((a) => `    ${a}`),
      `  </div>`,
      `  <dl class="plan-limits">`,
      ...data.limits.map(
        ({ key, label }) => `    <div><dt>${escape(label)}</dt><dd>${escape(plan.limits[key])}</dd></div>`,
      ),
      `  </dl>`,
      `</article>`,
    ];
    return lines.map((l) => pad(6) + l).join("\n");
  });
  return [`${pad(4)}<div class="plans">`, ...cards, `${pad(4)}</div>`].join("\n");
}

/** The page with its cards rendered from its own JSON. */
export function withPlans(html) {
  const start = html.indexOf(START);
  const end = html.indexOf(END);
  if (start < 0 || end < start) throw new Error(`index.html: the markers "${START}" … "${END}" are missing`);
  // From the end marker's line on, indentation included, the page is kept as is.
  const endLine = html.lastIndexOf("\n", end) + 1;
  return `${html.slice(0, start + START.length)}\n${render(readPlans(html))}\n${html.slice(endLine)}`;
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const html = await readFile(page, "utf8");
  const next = withPlans(html);
  if (process.argv.includes("--check")) {
    if (next !== html) {
      console.error("pricing: the plan cards differ from #plans-data: run `npm --prefix site run pricing`");
      process.exit(1);
    }
    console.log("pricing: the plan cards match #plans-data");
  } else if (next !== html) {
    await writeFile(page, next);
    console.log("pricing: plan cards rewritten from #plans-data");
  } else {
    console.log("pricing: already up to date");
  }
}
