import { checkoutProblems } from "./copy";
import {
  direction,
  duration,
  euros,
  formatDay,
  formatLongDate,
  historyLabel,
  isLive,
  meterPercent,
  meterState,
  planFeatures,
  priceLine,
  statusOf,
} from "./format";
import type { PlanOffer, SubscriptionInfo } from "./queries";

const team: PlanOffer = {
  plan: "team",
  name: "Team",
  monthly_cents: 4900,
  yearly_cents: 49000,
  limits: {
    monthly_cents: 4900,
    members: null,
    databases: 10,
    versions_per_database: null,
    schema_objects: 1000,
    history_days: null,
    playground_size: "medium",
    playground_hours: 80,
    approvals: 10,
    approvals_override: false,
  },
};

describe("meters", () => {
  it("warn from 80 %, say when at the limit, and when over it after a downgrade", () => {
    expect(meterState(1, 3)).toBe("ok");
    expect(meterState(4, 5)).toBe("near");
    expect(meterState(3, 3)).toBe("full");
    expect(meterState(7, 1)).toBe("over");
    expect(meterState(1_000_000, null)).toBe("ok");
    expect(meterState(0, 0)).toBe("full");
  });

  it("fill their bar in percent, capped at 100", () => {
    expect(meterPercent(1, 4)).toBe(25);
    expect(meterPercent(9, 3)).toBe(100);
    expect(meterPercent(5, null)).toBe(0);
    expect(meterPercent(0, 0)).toBe(100);
  });

  it("read playground time in hours and minutes", () => {
    expect(duration(59)).toBe("0 min");
    expect(duration(45 * 60)).toBe("45 min");
    expect(duration(4800)).toBe("1 h 20 min");
    expect(duration(72_000)).toBe("20 h");
  });
});

describe("prices and plans", () => {
  it("show whole euros, a year at ten months", () => {
    expect(euros(1900)).toBe("€19");
    expect(euros(1950)).toBe("€19.50");
    expect(priceLine(team, "month")).toBe("€49 a month");
    expect(priceLine(team, "year")).toBe("€490 a year");
    expect(priceLine({ ...team, monthly_cents: null }, "month")).toBeNull();
  });

  it("list what a plan holds", () => {
    expect(planFeatures(team.limits)).toEqual([
      "Unlimited members",
      "10 databases",
      "Unlimited versions",
      "1,000 tables and views per schema version",
      "All history kept",
      "80 playground hours a month, medium rows",
      "Up to 10 required approvals",
    ]);
    expect(historyLabel(90)).toBe("90 days");
    expect(historyLabel(365)).toBe("1 year");
  });

  it("tell an upgrade from a downgrade", () => {
    expect(direction("free", "starter")).toBe("up");
    expect(direction("business", "team")).toBe("down");
    expect(direction("team", "team")).toBe("same");
  });

  it("word a subscription's status, cancellation first", () => {
    const sub = (status: string, cancel = false): SubscriptionInfo => ({
      plan: "team",
      status,
      cancel_at_period_end: cancel,
    });
    expect(statusOf(sub("active"))).toEqual({ label: "Active", tone: "success" });
    expect(statusOf(sub("past_due")).label).toBe("Payment overdue");
    expect(statusOf(sub("active", true)).label).toBe("Cancels at period end");
    expect(isLive(sub("unpaid"))).toBe(true);
    expect(isLive(sub("canceled"))).toBe(false);
    expect(isLive(null)).toBe(false);
  });
});

describe("in French", () => {
  it("writes prices, dates and plans as French does", () => {
    expect(euros(1900, "fr")).toBe("19\u00a0€");
    expect(euros(1950, "fr")).toBe("19,50\u00a0€");
    expect(priceLine(team, "year", "fr")).toBe("490\u00a0€ par an");
    expect(formatLongDate("2026-11-04T12:00:00Z", "fr")).toBe("4 novembre 2026");
    expect(planFeatures(team.limits, "fr")).toContain(
      "1\u202f000 tables et vues par version du schéma",
    );
    expect(historyLabel(365, "fr")).toBe("1 an");
    expect(
      statusOf({ plan: "team", status: "active", cancel_at_period_end: true }, "fr").label,
    ).toBe("Prend fin en fin de période");
  });
});

describe("checkoutProblems", () => {
  const form = {
    buyer: "business" as const,
    acceptTerms: true,
    businessConfirmed: true,
    companyName: "Acme SAS",
    country: "FR",
    immediateStart: false,
    withdrawal: false,
  };

  it("needs a buyer chosen, never assumed, and the Terms from everyone", () => {
    expect(Object.values(checkoutProblems(form)).filter(Boolean)).toEqual([]);
    expect(checkoutProblems({ ...form, buyer: undefined }).buyer).toMatch(
      /business or as an individual/,
    );
    expect(checkoutProblems({ ...form, acceptTerms: false }).terms).toMatch(/Terms of Service/);
  });

  it("needs a business's statement, name and country", () => {
    expect(checkoutProblems({ ...form, businessConfirmed: false }).business).toBeDefined();
    expect(checkoutProblems({ ...form, companyName: "  " }).companyName).toBeDefined();
    expect(checkoutProblems({ ...form, country: undefined }).country).toBeDefined();
  });

  it("needs both of a consumer's requests, and nothing of a business", () => {
    const consumer = {
      ...form,
      buyer: "consumer" as const,
      businessConfirmed: false,
      companyName: "",
      country: undefined,
    };
    expect(checkoutProblems(consumer).consumer).toBeDefined();
    expect(checkoutProblems(consumer).business).toBeUndefined();
    expect(checkoutProblems(consumer).companyName).toBeUndefined();
    expect(checkoutProblems({ ...consumer, immediateStart: true }).consumer).toBeDefined();
    expect(
      checkoutProblems({ ...consumer, immediateStart: true, withdrawal: true }).consumer,
    ).toBeUndefined();
  });

  it("speaks the buyer's language", () => {
    expect(checkoutProblems({ ...form, acceptTerms: false }, "fr").terms).toBe(
      "Acceptez les Conditions générales pour continuer.",
    );
  });
});

describe("the last day to withdraw", () => {
  it("reads in UTC, as the emails say it, whatever the browser's time zone", () => {
    // The period ends at midnight UTC; a second before is the last day.
    expect(formatLongDate("2026-11-04T23:59:59Z", "en", "UTC")).toBe("November 4, 2026");
    expect(formatLongDate("2026-11-04T23:59:59Z", "fr", "UTC")).toBe("4 novembre 2026");
  });
});

it("says a calendar day as written, whatever the reader's time zone", () => {
  // The withdrawal's last day, as the API counts it in Paris.
  expect(formatDay("2026-11-04", "en")).toBe("November 4, 2026");
  expect(formatDay("2026-11-04", "fr")).toBe("4 novembre 2026");
});
