import type { components } from "@/api/schema.gen";

import type { Interval, Plan, PlanLimits, PlanOffer, SubscriptionInfo } from "./queries";

type PlaygroundSize = components["schemas"]["PlaygroundSize"];

export const planNames: Record<Plan, string> = {
  free: "Free",
  starter: "Starter",
  team: "Team",
  business: "Business",
};

/** Cheapest first, as the API orders them. */
export const planOrder: readonly Plan[] = ["free", "starter", "team", "business"];

export const sizeNames: Record<PlaygroundSize, string> = { small: "Small", medium: "Medium" };

const longDate = new Intl.DateTimeFormat("en-US", { dateStyle: "long" });

/** "November 20, 2026": a renewal, an end, a deadline. */
export function formatLongDate(iso: string): string {
  return longDate.format(new Date(iso));
}

/** "€19", "€19.50": whole euros without cents. */
export function euros(cents: number): string {
  const whole = cents % 100 === 0;
  return new Intl.NumberFormat("en-US", {
    style: "currency",
    currency: "EUR",
    minimumFractionDigits: whole ? 0 : 2,
    maximumFractionDigits: whole ? 0 : 2,
  }).format(cents / 100);
}

/** "€19 a month", "€190 a year"; `null` for Free. */
export function priceLine(offer: PlanOffer, interval: Interval): string | null {
  const cents = interval === "month" ? offer.monthly_cents : offer.yearly_cents;
  if (cents == null) return null;
  return `${euros(cents)} a ${interval}`;
}

/** "1 database", "3 databases". */
export function count(n: number, one: string, many: string): string {
  return `${n.toLocaleString("en-US")} ${n === 1 ? one : many}`;
}

/**
 * How full a meter is: room left, 80 % or more, at its limit, or over it
 * (after a downgrade: nothing is deleted, creating more waits).
 */
export type MeterState = "ok" | "near" | "full" | "over";

export function meterState(used: number, limit: number | null | undefined): MeterState {
  if (limit == null) return "ok";
  if (used > limit) return "over";
  if (limit <= 0 || used >= limit) return "full";
  return used * 5 >= limit * 4 ? "near" : "ok";
}

/** The bar's value, 0 to 100 (a limit of 0 reads as full). */
export function meterPercent(used: number, limit: number | null | undefined): number {
  if (limit == null) return 0;
  if (limit <= 0) return 100;
  return Math.min(100, Math.round((used / limit) * 100));
}

/** "45 min", "1 h 20 min", "20 h": running time, rounded down to the minute. */
export function duration(seconds: number): string {
  const minutes = Math.floor(Math.max(0, seconds) / 60);
  const h = Math.floor(minutes / 60);
  const m = minutes % 60;
  if (h === 0) return `${m} min`;
  return m === 0 ? `${h} h` : `${h} h ${m} min`;
}

/** "90 days", "1 year", "Unlimited". */
export function historyLabel(days: number | null | undefined): string {
  if (days == null) return "All of it";
  if (days % 365 === 0) return count(days / 365, "year", "years");
  return count(days, "day", "days");
}

/** What a plan holds, one short line each, for the plan picker. */
export function planFeatures(limits: PlanLimits): string[] {
  return [
    limits.members == null ? "Unlimited members" : count(limits.members, "member", "members"),
    count(limits.databases, "database", "databases"),
    limits.versions_per_database == null
      ? "Unlimited versions"
      : `${count(limits.versions_per_database, "version", "versions")} per database`,
    `${limits.schema_objects.toLocaleString("en-US")} tables and views per schema`,
    limits.history_days == null
      ? "All history kept"
      : `${historyLabel(limits.history_days)} of history shown`,
    `${count(limits.playground_hours, "playground hour", "playground hours")} a month, ${sizeNames[
      limits.playground_size
    ].toLowerCase()} rows`,
    limits.approvals === 0
      ? "No required approvals"
      : limits.approvals === 1
        ? "1 required approval"
        : `Up to ${limits.approvals} required approvals${limits.approvals_override ? ", admin override" : ""}`,
  ];
}

/** Whether moving from `current` to `target` is a step up (charged now) or down (at renewal). */
export function direction(current: Plan, target: Plan): "up" | "down" | "same" {
  const from = planOrder.indexOf(current);
  const to = planOrder.indexOf(target);
  return to > from ? "up" : to < from ? "down" : "same";
}

/** Stripe's subscription status in a word, and how worried to be about it. */
export function statusOf(subscription: SubscriptionInfo): {
  label: string;
  tone: "success" | "warning" | "neutral";
} {
  if (subscription.cancel_at_period_end) return { label: "Cancels at period end", tone: "neutral" };
  switch (subscription.status) {
    case "active":
    case "trialing":
      return { label: "Active", tone: "success" };
    case "past_due":
    case "unpaid":
      return { label: "Payment overdue", tone: "warning" };
    case "canceled":
      return { label: "Cancelled", tone: "neutral" };
    default:
      return { label: "Not active", tone: "warning" };
  }
}

/** A subscription that can still be cancelled or changed (the API's "live"). */
export function isLive(subscription: SubscriptionInfo | null | undefined): boolean {
  return (
    subscription != null &&
    ["active", "trialing", "past_due", "unpaid"].includes(subscription.status)
  );
}
