import type { FoundingOffer, PlanOffer } from "./queries";
import {
  afterSignInLine,
  billingSearch,
  offered,
  readSubscribe,
  subscribeInNext,
  subscribeSearch,
} from "./subscribe";

describe("the plan a subscribe link carries", () => {
  it("is a paid plan of the API's, monthly unless yearly", () => {
    expect(readSubscribe({ plan: "team", interval: "year" })).toEqual({
      plan: "team",
      interval: "year",
    });
    expect(readSubscribe({ plan: "starter" })).toEqual({ plan: "starter", interval: "month" });
    expect(readSubscribe({ plan: "business", interval: "weekly" })).toEqual({
      plan: "business",
      interval: "month",
    });
  });

  it.each([{}, { plan: "free" }, { plan: "pro" }, { plan: "Team" }, { plan: ["team"] }])(
    "is nothing for %j: the usual sign-up",
    (search) => {
      expect(readSubscribe(search)).toBeUndefined();
    },
  );

  it("is read back from a sign-in's next, and only from the subscribe route", () => {
    expect(subscribeInNext("/app/signup?plan=team&interval=year")).toEqual({
      plan: "team",
      interval: "year",
    });
    expect(subscribeInNext("/app/new?plan=team")).toBeUndefined();
    expect(subscribeInNext("/app/signup?plan=free")).toBeUndefined();
    expect(subscribeInNext(undefined)).toBeUndefined();
    expect(afterSignInLine({ plan: "team", interval: "year" })).toBe(
      "Then you subscribe to Team, billed yearly.",
    );
  });

  it("keeps the founding offer for Team only", () => {
    const team = readSubscribe({ plan: "team", interval: "year", offer: "founding" });
    expect(team).toEqual({ plan: "team", interval: "year", offer: "founding" });
    expect(subscribeSearch(team)).toEqual({ plan: "team", interval: "year", offer: "founding" });
    expect(billingSearch(team!)).toEqual({
      subscribe: "team",
      interval: "year",
      offer: "founding",
    });
    expect(readSubscribe({ plan: "starter", offer: "founding" })).toEqual({
      plan: "starter",
      interval: "month",
    });
    expect(readSubscribe({ plan: "team", offer: "other" })).toEqual({
      plan: "team",
      interval: "month",
    });
    expect(subscribeInNext("/app/signup?plan=team&interval=month&offer=founding")).toEqual({
      plan: "team",
      interval: "month",
      offer: "founding",
    });
  });
});

describe("a plan as its checkout bills it", () => {
  const team: PlanOffer = {
    plan: "team",
    name: "Team",
    monthly_cents: 4900,
    yearly_cents: 49000,
    limits: {} as PlanOffer["limits"],
  };
  const founding: FoundingOffer = {
    plan: "team",
    monthly_cents: 2900,
    yearly_cents: 29000,
    list_monthly_cents: 4900,
    list_yearly_cents: 49000,
    teams: 20,
    left: 3,
  };

  it("is at the founding price when asked for, while places are left", () => {
    const shown = offered(team, "founding", founding);
    expect(shown.plan.monthly_cents).toBe(2900);
    expect(shown.plan.yearly_cents).toBe(29000);
    expect(shown.founding).toBe(founding);
  });

  it("is at its own price otherwise", () => {
    expect(offered(team, undefined, founding)).toEqual({ plan: team, founding: null });
    expect(offered(team, "founding", null)).toEqual({ plan: team, founding: null });
    expect(offered(team, "founding", { ...founding, left: 0 })).toEqual({
      plan: team,
      founding: null,
    });
    const starter = { ...team, plan: "starter" as const, name: "Starter" };
    expect(offered(starter, "founding", founding)).toEqual({ plan: starter, founding: null });
  });
});
