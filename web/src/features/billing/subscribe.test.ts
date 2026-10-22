import { afterSignInLine, readSubscribe, subscribeInNext } from "./subscribe";

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
});
