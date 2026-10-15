import { slugify, slugProblem } from "./slug";

describe("slugify", () => {
  it.each([
    ["Acme Inc.", "acme-inc"],
    ["  Café Ünïon  ", "cafe-union"],
    ["R&D — Platform", "r-d-platform"],
    ["日本", ""],
    ["a".repeat(70), "a".repeat(63)],
  ])("%s → %s", (name, slug) => {
    expect(slugify(name)).toBe(slug);
  });

  it("never ends on a hyphen after truncating", () => {
    expect(slugify(`${"a".repeat(62)} b`)).toBe("a".repeat(62));
  });
});

describe("slugProblem", () => {
  it("accepts what slugify makes", () => {
    expect(slugProblem(slugify("Acme Labs 2"))).toBeUndefined();
  });

  it.each(["", "Acme", "acme-", "a--b", "settings", "invite"])("refuses %j", (slug) => {
    expect(slugProblem(slug)).toBeDefined();
  });
});
