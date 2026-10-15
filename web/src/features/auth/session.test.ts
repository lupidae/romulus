import { afterSignIn, appPath, devSigninUrl, loginUrl } from "./session";

describe("afterSignIn", () => {
  it("goes on to a path in the app", () => {
    expect(afterSignIn("/app/acme/settings?tab=1")).toBe("/acme/settings?tab=1");
    expect(afterSignIn(undefined)).toBe("/");
    expect(afterSignIn("/app/")).toBe("/");
  });

  it.each(["/app//evil.test", "/app/\\evil.test", "https://evil.test/app/x", "/apple", "/api/me"])(
    "refuses %s",
    (next) => {
      expect(afterSignIn(next)).toBe("/");
    },
  );
});

describe("auth URLs", () => {
  it("prefixes router paths with the app's base, once", () => {
    expect(appPath("/acme/settings")).toBe("/app/acme/settings");
    expect(appPath("/app/acme")).toBe("/app/acme");
    expect(appPath("/")).toBe("/app/");
    expect(appPath("/apple")).toBe("/app/apple");
  });

  it("builds the SSO and dev sign-in links with where to come back", () => {
    expect(loginUrl()).toBe("/api/auth/login");
    expect(loginUrl("/app/invite/x", { switchAccount: true })).toBe(
      "/api/auth/login?next=%2Fapp%2Finvite%2Fx&prompt=select_account",
    );
    expect(devSigninUrl("bob@romulus.test", "/app/")).toBe(
      "/api/auth/dev-signin?email=bob%40romulus.test&next=%2Fapp%2F",
    );
  });
});
