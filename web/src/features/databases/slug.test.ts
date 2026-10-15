import { databaseSlugProblem, slugify } from "./slug";

describe("database slugs", () => {
  it("follow the API's rules", () => {
    expect(databaseSlugProblem("billing-prod")).toBeUndefined();
    expect(databaseSlugProblem("")).toMatch(/Choose/);
    expect(databaseSlugProblem("Billing")).toMatch(/lowercase/);
    expect(databaseSlugProblem("a--b")).toMatch(/single hyphens/);
    expect(databaseSlugProblem("settings")).toMatch(/organization's own pages/);
  });

  it("come from the name", () => {
    expect(slugify("Billing (prod)")).toBe("billing-prod");
  });
});
