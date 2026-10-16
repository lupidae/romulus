import { describe, expect, it } from "vitest";

import {
  approvalsLabel,
  approved,
  names,
  resetApprovals,
  type Review,
  reviewing,
  sha256Hex,
  standing,
} from "./review";

const bob = { user_id: "bob", name: "Bob", at: "2026-10-16T10:00:00Z" };
const carol = { user_id: "carol", name: "Carol", at: "2026-10-16T10:05:00Z" };

function review(patch: Partial<Review> = {}): Review {
  return { required: 2, approvals: [], outdated: [], editors: [], requested: null, ...patch };
}

describe("review", () => {
  it("is off when the organization requires no approval", () => {
    expect(reviewing(null)).toBe(false);
    expect(reviewing(review({ required: 0 }))).toBe(false);
    expect(reviewing(review())).toBe(true);
  });

  it("counts approvals against the requirement", () => {
    expect(approved(review({ approvals: [bob] }))).toBe(false);
    expect(approved(review({ approvals: [bob, carol] }))).toBe(true);
    expect(approvalsLabel(review({ approvals: [bob] }))).toBe("1 of 2 approvals");
    expect(approvalsLabel(review({ required: 1, approvals: [bob] }))).toBe("1 of 1 approval");
  });

  it("says what each person may do", () => {
    const r = review({ approvals: [bob], editors: ["alice"] });
    expect(standing(r, "alice", "developer")).toBe("editor");
    expect(standing(r, "bob", "reviewer")).toBe("approved");
    expect(standing(r, "carol", "reviewer")).toBe("can_approve");
  });

  it("tells a reset by an edit from a withdrawal", () => {
    const before = review({ approvals: [bob, carol] });
    // Bob's approval is of the old text now; Carol withdrew hers.
    const after = review({ outdated: [bob] });
    expect(resetApprovals(before, after)).toEqual([bob]);
    expect(resetApprovals(null, after)).toEqual([]);
  });

  it("names people the way a sentence does", () => {
    expect(names([])).toBe("");
    expect(names([bob])).toBe("Bob");
    expect(names([bob, carol, { name: "Dan" }])).toBe("Bob, Carol and Dan");
  });

  it("hashes the SQL as the server does", async () => {
    expect(await sha256Hex("")).toBe(
      "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
    );
  });
});
