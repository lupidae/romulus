import { describe, expect, it } from "vitest";

import { isArchive, lineRanges, NUMBERED } from "./queries";

const file = (path: string) => ({ path, content: "", role: "up" as const, marks: [] });

describe("isArchive", () => {
  it("downloads a single top-level file as itself", () => {
    expect(isArchive([file("V5__add.sql")])).toBe(false);
  });

  it("zips a folder or several files", () => {
    expect(isArchive([file("20261016093000_add/migration.sql")])).toBe(true);
    expect(isArchive([file("1_add.up.sql"), file("1_add.down.sql")])).toBe(true);
  });
});

describe("NUMBERED", () => {
  it("accepts the versions each tool reads", () => {
    expect(NUMBERED.flyway!.pattern.test("1.2.3")).toBe(true);
    expect(NUMBERED.flyway!.pattern.test("1..2")).toBe(false);
    expect(NUMBERED.golang_migrate!.pattern.test("000042")).toBe(true);
    expect(NUMBERED.golang_migrate!.pattern.test("4.2")).toBe(false);
  });
});

describe("lineRanges", () => {
  it("turns 1-based inclusive lines into character ranges", () => {
    const content = "a\nbb\nccc\n";
    expect(lineRanges(content, [{ from: 2, to: 3 }])).toEqual([{ from: 2, to: 8 }]);
    expect(content.slice(2, 8)).toBe("bb\nccc");
  });
});
