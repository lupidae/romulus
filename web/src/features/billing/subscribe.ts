import { planNames } from "./format";
import type { Interval, Plan } from "./queries";

/**
 * The homepage's plan buttons lead to `/app/signup?plan=<plan>&interval=<interval>`:
 * a plan someone chose before having an account, carried through sign-in or
 * sign-up (the OIDC round trip keeps it in `next`), the Terms and the first
 * organization, to that organization's checkout. Plan ids are the API's
 * (`billing::plans`); Free needs no checkout, so it is not one.
 */
export interface Subscribe {
  plan: Exclude<Plan, "free">;
  interval: Interval;
}

const PAID: readonly Subscribe["plan"][] = ["starter", "team", "business"];

/** The route of the subscribe link (under the app's /app base). */
export const SUBSCRIBE_ROUTE = "/signup";

/**
 * The choice in a URL's search (`plan`, `interval`), or undefined when the
 * plan is missing or not a paid one. A missing or unknown interval is monthly,
 * as the homepage's switch starts.
 */
export function readSubscribe(search: Record<string, unknown>): Subscribe | undefined {
  const plan = PAID.find((p) => p === search.plan);
  if (!plan) return undefined;
  return { plan, interval: search.interval === "year" ? "year" : "month" };
}

/** The choice as search parameters, for the routes that carry it. */
export function subscribeSearch(choice: Subscribe | undefined): Partial<Subscribe> {
  return choice ? { plan: choice.plan, interval: choice.interval } : {};
}

/**
 * The choice a sign-in's `next` carries (`/app/signup?plan=…`), for the
 * sign-in pages to say what comes after: undefined for any other `next`.
 */
export function subscribeInNext(next: string | undefined): Subscribe | undefined {
  if (!next) return undefined;
  let url: URL;
  try {
    url = new URL(next, "http://romulus.invalid");
  } catch {
    return undefined;
  }
  if (url.pathname !== `/app${SUBSCRIBE_ROUTE}`) return undefined;
  return readSubscribe(Object.fromEntries(url.searchParams));
}

/** What comes after signing in, on the (English) sign-in pages. */
export function afterSignInLine(choice: Subscribe): string {
  return `Then you subscribe to ${planNames[choice.plan]}, billed ${choice.interval === "year" ? "yearly" : "monthly"}.`;
}
