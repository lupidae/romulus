import { changeSections, statusCounts } from "@/features/diff/changes";

import { diffOf, largeDiffFixture } from "./synthetic-diff";

describe("the 300-table diff fixture", () => {
  it("changes about a tenth of the tables, removes a few and adds a dozen", () => {
    const { before, after, diff, sql } = largeDiffFixture();
    const counts = statusCounts(changeSections(diff));
    expect(counts.added).toBe(12);
    expect(counts.removed).toBeGreaterThan(0);
    expect(counts.changed).toBeGreaterThan(15);
    expect(counts.changed).toBeLessThan(60);
    expect(diff.summary.text).toBe(
      `12 tables added, ${counts.changed} changed, ${counts.removed} removed`,
    );
    expect(after.entities!.length).toBe(before.entities!.length + 12 - counts.removed);
    // Every object of either side has a status, for the graph.
    const statuses = Object.values(diff.status.entities).flatMap((t) => Object.keys(t));
    expect(statuses.length).toBe(before.entities!.length + 12);
    expect(sql.after).toContain("CREATE TABLE");
    expect(sql.after).not.toBe(sql.before);
  });

  it("is the same every time, and nothing differs from itself", () => {
    expect(largeDiffFixture().diff).toEqual(largeDiffFixture().diff);
    const { before } = largeDiffFixture();
    expect(diffOf(before, before).summary.text).toBe("no changes");
  });

  it("can change every table, the change list's stress case", () => {
    const { before, diff } = largeDiffFixture(300, { every: true });
    const tables = before.entities!.filter((e) => e.kind === "r").length;
    expect(statusCounts(changeSections(diff)).changed).toBeGreaterThanOrEqual(tables);
  });
});
