import { checkoutProblems } from "./copy";
import {
  direction,
  duration,
  euros,
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
      "1,000 tables and views per schema",
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

describe("checkoutProblems", () => {
  const form = { acceptTerms: true, consumer: false, immediateStart: false, withdrawal: false };

  it("needs the Terms from everyone", () => {
    expect(checkoutProblems(form)).toEqual({ terms: undefined, consumer: undefined });
    expect(checkoutProblems({ ...form, acceptTerms: false }).terms).toMatch(/Terms of Service/);
  });

  it("needs both of a consumer's requests, never a business's", () => {
    expect(checkoutProblems({ ...form, consumer: true }).consumer).toBeDefined();
    expect(
      checkoutProblems({ ...form, consumer: true, immediateStart: true }).consumer,
    ).toBeDefined();
    expect(
      checkoutProblems({ ...form, consumer: true, immediateStart: true, withdrawal: true })
        .consumer,
    ).toBeUndefined();
    expect(checkoutProblems({ ...form, immediateStart: false }).consumer).toBeUndefined();
  });
});
