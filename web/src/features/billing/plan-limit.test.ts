import { ApiError } from "@/api/client";

import { capSummary, capTitles, isPlanLimit, liftLine, planLimitOf } from "./plan-limit";
import { closeUpgrade, showUpgrade } from "./upgrade";

const refusal = (details: unknown, message = "The Free plan keeps 1 database.") =>
  new ApiError(402, { code: "plan_limit", message, details: details as never });

describe("planLimitOf", () => {
  it("reads a 402 plan_limit into the cap, the plan and what lifts it", () => {
    const limit = planLimitOf(
      refusal({ cap: "databases", plan: "free", limit: 1, usage: 1, upgrade_to: "starter" }),
    );
    expect(limit).toEqual({
      cap: "databases",
      plan: "free",
      limit: 1,
      usage: 1,
      upgrade_to: "starter",
      message: "The Free plan keeps 1 database.",
    });
    expect(capTitles[limit!.cap]).toBe("Database limit reached");
    expect(capSummary(limit!)).toBe("Free keeps 1 database");
    expect(liftLine(limit!)).toBe("Starter lifts it.");
  });

  it("is null for any other error, and for details it cannot read", () => {
    expect(planLimitOf(new ApiError(409, { code: "slug_taken", message: "taken" }))).toBeNull();
    expect(planLimitOf(new Error("boom"))).toBeNull();
    expect(planLimitOf(refusal({ cap: "galaxies", plan: "free" }))).toBeNull();
    expect(planLimitOf(refusal(undefined))).toBeNull();
    expect(isPlanLimit(null)).toBe(false);
  });

  it("says when no plan lifts it, and fills missing numbers with null", () => {
    const limit = planLimitOf(refusal({ cap: "playground_size", plan: "business" }))!;
    expect(limit.limit).toBeNull();
    expect(limit.upgrade_to).toBeNull();
    expect(liftLine(limit)).toMatch(/contact us/);
  });

  it("phrases every cap in a few words", () => {
    const of = (cap: string, limit: number) =>
      capSummary(planLimitOf(refusal({ cap, plan: "starter", limit }))!);
    expect(of("versions", 500)).toBe("Starter keeps 500 versions per database");
    expect(of("schema_size", 200)).toBe("Starter keeps 200 tables and views per schema version");
    expect(of("history", 365)).toBe("Starter shows 1 year of history");
    expect(of("playground_hours", 72_000)).toBe("Starter has 20 playground hours a month");
    expect(of("approvals", 1)).toBe("Starter allows 1 required approval");
    expect(of("members", 1)).toBe("Starter has 1 member");
  });
});

describe("showUpgrade", () => {
  afterEach(closeUpgrade);

  it("takes plan limits only, so call sites fall through to their own errors", () => {
    expect(showUpgrade(new ApiError(500, { code: "internal", message: "x" }))).toBe(false);
    expect(showUpgrade(refusal({ cap: "approvals", plan: "free", limit: 0 }))).toBe(true);
  });
});
