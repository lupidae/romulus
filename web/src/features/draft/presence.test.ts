import { describe, expect, it } from "vitest";

import { presenceLine } from "./presence";

const person = (name: string, editing: boolean) => ({ user_id: name, name, editing });

describe("presenceLine", () => {
  it("names who edits, then who watches", () => {
    expect(presenceLine([person("Alice", true), person("Bob", true), person("Carol", false)])).toBe(
      "Alice and Bob editing, Carol watching",
    );
  });

  it("says nothing when nobody is there", () => {
    expect(presenceLine([])).toBe("");
    expect(presenceLine([person("Bob", false)])).toBe("Bob watching");
  });
});
