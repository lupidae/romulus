import { describe, expect, it } from "vitest";

import { findConflicts, resolution } from "./conflicts";

const text = `CREATE TABLE users (id bigint);
-- <<<<<<< yours
CREATE TABLE orders (id bigint, total numeric);
-- ======= v3
CREATE TABLE orders (id bigint, placed date);
-- >>>>>>> v3
-- <<<<<<< yours (both can be kept)
-- ======= v3
COMMENT ON TABLE users IS 'People';
-- >>>>>>> v3
-- ======= v9
`;

describe("conflict blocks", () => {
  it("are found with their sides, label and whether both can stay", () => {
    const blocks = findConflicts(text);
    expect(blocks).toHaveLength(2);
    const [first, second] = blocks as [(typeof blocks)[0], (typeof blocks)[0]];
    expect(text.slice(first.yours.from, first.yours.to)).toBe(
      "CREATE TABLE orders (id bigint, total numeric);",
    );
    expect(text.slice(first.theirs.from, first.theirs.to)).toBe(
      "CREATE TABLE orders (id bigint, placed date);",
    );
    expect(first.label).toBe("v3");
    expect(first.both).toBe(false);
    expect(text.slice(first.from, first.to)).toMatch(/^-- <<<<<<< yours\n[\s\S]*-- >>>>>>> v3$/);
    expect(second.both).toBe(true);
    expect(second.yours.from).toBe(second.yours.to);
  });

  it("resolve to one side, both, or nothing", () => {
    const [first, second] = findConflicts(text) as [
      ReturnType<typeof findConflicts>[0],
      ReturnType<typeof findConflicts>[0],
    ];
    expect(resolution(text, first, "theirs").insert).toBe(
      "CREATE TABLE orders (id bigint, placed date);",
    );
    expect(resolution(text, second, "both").insert).toBe("COMMENT ON TABLE users IS 'People';");
    // Nothing kept: the line goes too.
    const gone = resolution(text, second, "yours");
    expect(gone.insert).toBe("");
    expect(text[gone.to - 1]).toBe("\n");
  });

  it("are not looked for in text without markers", () => {
    expect(findConflicts("CREATE TABLE a (id int); -- <<< not one")).toEqual([]);
  });
});
