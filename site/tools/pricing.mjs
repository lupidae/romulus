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
// both periods' prices are in it: the Monthly/Yearly switch is CSS alone. So
// are both periods' buttons of a paid plan: each leads to subscribing to it
// (`subscribe`?plan=<id>&interval=month|year, the API's plan ids), the
// period shown being the one the visitor chose; Free's to `signup`.
//
// A plan may carry an `offer` (the founding-team offer on Team): a line under
// its button, hidden in the HTML, that app.js shows once the API's `count`
// endpoint says places are left, with how many; its links add `&offer=<id>`.
// Off, sold out or unreachable, the line stays hidden.
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
  for (const key of ["currency", "yearly_months", "signup", "subscribe", "talk", "limits", "plans"])
    if (data[key] === undefined) throw new Error(`#plans-data: "${key}" is missing`);
  for (const plan of data.plans) {
    for (const key of ["id", "name", "for", "monthly", "action"])
      if (plan[key] === undefined) throw new Error(`#plans-data: plan ${plan.id ?? "?"} has no "${key}"`);
    for (const { key } of data.limits)
      if (plan.limits?.[key] === undefined)
        throw new Error(`#plans-data: plan ${plan.id} has no limit "${key}"`);
    if (plan.offer)
      for (const key of ["id", "label", "monthly", "teams", "count"])
        if (plan.offer[key] === undefined)
          throw new Error(`#plans-data: plan ${plan.id}'s offer has no "${key}"`);
  }
  return data;
}

/**
 * Where a paid plan's button leads for `interval` (`month` or `year`): the app's
 * subscribe link, with `offer` (an offer's id) when it is that offer's.
 */
export const subscribeHref = (data, plan, interval, offer) =>
  `${data.subscribe}?plan=${encodeURIComponent(plan.id)}&interval=${interval}` +
  (offer ? `&offer=${encodeURIComponent(offer)}` : "");

/**
 * An offer's line, as each period shows it: "Founding teams: 29 € a month for as long as
 * you stay subscribed, 20 of 20 left" (the Terms' 7.11 says what ends it).
 */
export const offerLine = (data, offer, interval, left = offer.teams) =>
  `${offer.label}: ${price(interval === "month" ? offer.monthly : offer.monthly * data.yearly_months, data.currency)} a ${interval} for as long as you stay subscribed, ${left} of ${offer.teams} left`;

/**
 * The analytics event a sign-up button sends (Umami reads `data-umami-event`
 * and its `data-umami-event-*` data on click): which plan, which period.
 */
export const signupEvent = (plan, interval) =>
  ` data-umami-event="signup_started" data-umami-event-plan="${escape(plan.id)}"` +
  (interval ? ` data-umami-event-interval="${interval}"` : "");

/** "1,490 €": numbers written as the plans table writes them, the currency after, never on its own line. */
export const price = (amount, currency) => `${amount.toLocaleString("en-US")}\u00a0${currency}`;

/** The cards' HTML, indented to sit in the pricing section's container. */
export function render(data) {
  const pad = (n) => " ".repeat(n);
  const cards = data.plans.map((plan) => {
    const yearly = plan.monthly * data.yearly_months;
    const id = `plan-${plan.id}`;
    const money = (amount) => escape(price(amount, data.currency)).replace(/\u00a0/g, "&nbsp;");
    const look = plan.recommended ? "primary" : "secondary";
    const actions = plan.monthly
      ? ["month", "year"].map(
          (interval) =>
            `<a class="button ${look} per-${interval}" href="${escape(subscribeHref(data, plan, interval))}"${signupEvent(plan, interval)}>${escape(plan.action)}</a>`,
        )
      : [`<a class="button ${look}" href="${escape(data.signup)}"${signupEvent(plan)}>${escape(plan.action)}</a>`];
    if (plan.offer) {
      const { offer } = plan;
      const links = ["month", "year"].map((interval) => {
        const [label, rest] = offerLine(data, offer, interval).split(/: (.*)/s);
        const words = escape(rest)
          .replace(/\u00a0/g, "&nbsp;")
          // "7 of 20 left" never breaks.
          .replace(` ${offer.teams} of ${offer.teams} left`, ` <span data-offer-left>${offer.teams}</span>&nbsp;of&nbsp;${offer.teams}&nbsp;left`);
        const event = `${signupEvent(plan, interval)} data-umami-event-offer="${escape(offer.id)}"`;
        return `  <a class="per-${interval}" href="${escape(subscribeHref(data, plan, interval, offer.id))}"${event}><b>${escape(label)}:</b> ${words}</a>`;
      });
      actions.push(`<p class="plan-offer" data-offer="${escape(offer.count)}" hidden>`, ...links, `</p>`);
    }
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
