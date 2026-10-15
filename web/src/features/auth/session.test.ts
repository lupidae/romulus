import { appPath, devSigninUrl, loginUrl } from "./session";

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
