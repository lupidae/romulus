import type { components } from "@/api/schema.gen";
import { intlLocale, type Locale } from "@/features/legal/locale";

import { billingCopy } from "./copy";
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

/**
 * "November 20, 2026", "20 novembre 2026": a renewal, an end, a deadline.
 * `timeZone: "UTC"` for a day the API counts in UTC (the last day to
 * withdraw), so the page says what the emails say.
 */
export function formatLongDate(iso: string, locale: Locale = "en", timeZone?: string): string {
  return new Intl.DateTimeFormat(intlLocale(locale), { dateStyle: "long", timeZone }).format(
    new Date(iso),
  );
}

/** A calendar day ("2026-11-04", the API's dates): "November 4, 2026", "4 novembre 2026". */
export function formatDay(day: string, locale: Locale = "en"): string {
  return formatLongDate(`${day}T12:00:00Z`, locale, "UTC");
}

/** "Nov 20, 2026", "20 nov. 2026": a date in a table. */
export function formatShortDate(iso: string, locale: Locale = "en"): string {
  return new Intl.DateTimeFormat(intlLocale(locale), { dateStyle: "medium" }).format(new Date(iso));
}

/** "November 20, 2026 at 2:05 PM", "20 novembre 2026 à 14:05": a moment to keep. */
export function formatMoment(iso: string, locale: Locale = "en"): string {
  return new Intl.DateTimeFormat(intlLocale(locale), {
    dateStyle: "long",
    timeStyle: "short",
  }).format(new Date(iso));
}

/** "€19", "€19.50"; "19 €", "19,50 €": whole euros without cents. */
export function euros(cents: number, locale: Locale = "en"): string {
  const whole = cents % 100 === 0;
  return new Intl.NumberFormat(intlLocale(locale), {
    style: "currency",
    currency: "EUR",
    minimumFractionDigits: whole ? 0 : 2,
    maximumFractionDigits: whole ? 0 : 2,
  }).format(cents / 100);
}

/** An amount in its currency, euros or another (cents shown). */
export function money(cents: number, currency: string, locale: Locale = "en"): string {
  return currency.toLowerCase() === "eur"
    ? euros(cents, locale)
    : new Intl.NumberFormat(intlLocale(locale), {
        style: "currency",
        currency: currency.toUpperCase(),
      }).format(cents / 100);
}

/** "€19 a month", "19 € par mois"; `null` for Free. */
export function priceLine(
  offer: PlanOffer,
  interval: Interval,
  locale: Locale = "en",
): string | null {
  const cents = interval === "month" ? offer.monthly_cents : offer.yearly_cents;
  if (cents == null) return null;
  return billingCopy[locale].summary.price(euros(cents, locale), interval);
}

/** "1 database", "3 databases". */
export function count(n: number, one: string, many: string, locale: Locale = "en"): string {
  return `${n.toLocaleString(intlLocale(locale))} ${n === 1 ? one : many}`;
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
export function historyLabel(days: number | null | undefined, locale: Locale = "en"): string {
  if (locale === "fr") {
    if (days == null) return "Tout";
    if (days % 365 === 0) return count(days / 365, "an", "ans", locale);
    return count(days, "jour", "jours", locale);
  }
  if (days == null) return "All of it";
  if (days % 365 === 0) return count(days / 365, "year", "years");
  return count(days, "day", "days");
}

/** What a plan holds, one short line each, for the plan picker. */
export function planFeatures(limits: PlanLimits, locale: Locale = "en"): string[] {
  if (locale === "fr") {
    const n = (value: number) => value.toLocaleString("fr-FR");
    return [
      limits.members == null
        ? "Membres illimités"
        : count(limits.members, "membre", "membres", locale),
      count(limits.databases, "base de données", "bases de données", locale),
      limits.versions_per_database == null
        ? "Versions illimitées"
        : `${count(limits.versions_per_database, "version", "versions", locale)} par base de données`,
      `${n(limits.schema_objects)} tables et vues par version du schéma`,
      limits.history_days == null
        ? "Tout l'historique conservé"
        : `${historyLabel(limits.history_days, locale)} d'historique affiché`,
      `${count(limits.playground_hours, "heure", "heures", locale)} de playground par mois, ${
        limits.playground_size === "small" ? "petit volume" : "volume moyen"
      }`,
      limits.approvals === 0
        ? "Aucune approbation requise"
        : limits.approvals === 1
          ? "1 approbation requise"
          : `Jusqu'à ${limits.approvals} approbations requises${limits.approvals_override ? ", contournement par un administrateur" : ""}`,
    ];
  }
  return [
    limits.members == null ? "Unlimited members" : count(limits.members, "member", "members"),
    count(limits.databases, "database", "databases"),
    limits.versions_per_database == null
      ? "Unlimited versions"
      : `${count(limits.versions_per_database, "version", "versions")} per database`,
    `${limits.schema_objects.toLocaleString("en-US")} tables and views per schema version`,
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
export function statusOf(
  subscription: SubscriptionInfo,
  locale: Locale = "en",
): {
  label: string;
  tone: "success" | "warning" | "neutral";
} {
  const words = billingCopy[locale].status;
  if (subscription.cancel_at_period_end) return { label: words.cancels, tone: "neutral" };
  switch (subscription.status) {
    case "active":
    case "trialing":
      return { label: words.active, tone: "success" };
    case "past_due":
    case "unpaid":
      return { label: words.overdue, tone: "warning" };
    case "canceled":
      return { label: words.cancelled, tone: "neutral" };
    default:
      return { label: words.inactive, tone: "warning" };
  }
}

/** A subscription that can still be cancelled or changed (the API's "live"). */
export function isLive(subscription: SubscriptionInfo | null | undefined): boolean {
  return (
    subscription != null &&
    ["active", "trialing", "past_due", "unpaid"].includes(subscription.status)
  );
}
