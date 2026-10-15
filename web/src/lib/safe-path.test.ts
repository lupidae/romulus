import { safePath } from "./safe-path";

describe("safePath", () => {
  it("keeps paths on this origin", () => {
    expect(safePath("/app/acme/billing?tab=sql#v12")).toBe("/app/acme/billing?tab=sql#v12");
  });

  it.each([
    ["another origin", "https://evil.example"],
    ["protocol-relative", "//evil.example"],
    ["backslash host", "/\\evil.example"],
    ["backslash later", "/app\\..\\evil"],
    ["tab", "/\t/evil.example"],
    ["newline", "/app\n"],
    ["relative", "app/acme"],
    ["javascript", "javascript:alert(1)"],
    ["not a string", 42],
  ])("refuses %s", (_, value) => {
    expect(safePath(value)).toBeUndefined();
  });
});
