import { planNames } from "./format";
import type { FoundingOffer, Interval, Plan, PlanOffer } from "./queries";

/**
 * The homepage's plan buttons lead to `/app/signup?plan=<plan>&interval=<interval>`:
 * a plan someone chose before having an account, carried through sign-in or
 * sign-up (the OIDC round trip keeps it in `next`), the Terms and the first
 * organization, to that organization's checkout. Plan ids are the API's
 * (`billing::plans`); Free needs no checkout, so it is not one. The
 * founding-team offer's link adds `&offer=founding` (Team only: any other plan
 * drops it), which the checkout applies while places are left.
 */
export interface Subscribe {
  plan: Exclude<Plan, "free">;
  interval: Interval;
  offer?: Offer;
}

/** An offer a subscribe link may carry, and the plan it is for (`billing::plans::FOUNDING`). */
export type Offer = "founding";
export const OFFER_PLAN: Record<Offer, Subscribe["plan"]> = { founding: "team" };

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
  const interval = search.interval === "year" ? "year" : "month";
  const offer =
    search.offer === "founding" && OFFER_PLAN.founding === plan ? "founding" : undefined;
  return offer ? { plan, interval, offer } : { plan, interval };
}

/** The choice as search parameters, for the routes that carry it. */
export function subscribeSearch(choice: Subscribe | undefined): Partial<Subscribe> {
  if (!choice) return {};
  const { plan, interval, offer } = choice;
  return offer ? { plan, interval, offer } : { plan, interval };
}

/** The choice as the billing page's search, which opens its checkout. */
export function billingSearch(choice: Subscribe) {
  return { subscribe: choice.plan, interval: choice.interval, offer: choice.offer };
}

/**
 * The plan as a checkout of it would bill it: at the founding-team price
 * when the buyer came with the offer, it is this plan's, places are left and
 * the organization never had it. `founding` says the price is the offer's.
 */
export function offered(
  plan: PlanOffer,
  offer: Offer | undefined,
  founding: FoundingOffer | null | undefined,
): { plan: PlanOffer; founding: FoundingOffer | null } {
  if (
    offer !== "founding" ||
    !founding ||
    founding.used ||
    founding.left <= 0 ||
    founding.plan !== plan.plan
  ) {
    return { plan, founding: null };
  }
  return { plan: atFounding(plan, founding), founding };
}

/** `plan` at founding-team prices (the offer's, or a founding subscription's own). */
export function atFounding(
  plan: PlanOffer,
  prices: { monthly_cents: number; yearly_cents: number },
): PlanOffer {
  return { ...plan, monthly_cents: prices.monthly_cents, yearly_cents: prices.yearly_cents };
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
