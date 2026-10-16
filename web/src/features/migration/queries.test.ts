import { describe, expect, it } from "vitest";

import { downloadName, lineRanges } from "./queries";

const file = (path: string) => ({ path, content: "", role: "up" as const, destructive: [] });

describe("downloadName", () => {
  it("downloads a single top-level file as itself", () => {
    expect(downloadName([file("V5__add.sql")])).toBe("V5__add.sql");
  });

  it("zips a folder or several files, named after the first", () => {
    expect(downloadName([file("20261016093000_add/migration.sql")])).toBe("20261016093000_add.zip");
    expect(
      downloadName([file("20261016093000_add.up.sql"), file("20261016093000_add.down.sql")]),
    ).toBe("20261016093000_add.zip");
  });
});

describe("lineRanges", () => {
  it("turns 1-based inclusive lines into character ranges", () => {
    const content = "a\nbb\nccc\n";
    expect(lineRanges(content, [{ from: 2, to: 3 }])).toEqual([{ from: 2, to: 8 }]);
    expect(content.slice(2, 8)).toBe("bb\nccc");
  });
});
