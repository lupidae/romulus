import {
  afterSignIn,
  appPath,
  devSigninUrl,
  loginUrl,
  rememberSignUpNext,
  takeSignUpNext,
} from "./session";

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
    expect(loginUrl("/app/invite/x", { loginHint: "bob@romulus.test" })).toBe(
      "/api/auth/login?next=%2Fapp%2Finvite%2Fx&login_hint=bob%40romulus.test",
    );
    expect(devSigninUrl("bob@romulus.test", "/app/")).toBe(
      "/api/auth/dev-signin?email=bob%40romulus.test&next=%2Fapp%2F",
    );
  });
});

describe("the next an emailed sign-up link follows", () => {
  afterEach(() => localStorage.clear());

  it("is kept once, for the day the link works", () => {
    rememberSignUpNext("/app/signup?plan=team&interval=year", 1_000);
    expect(takeSignUpNext(2_000)).toBe("/app/signup?plan=team&interval=year");
    expect(takeSignUpNext(3_000)).toBeUndefined();

    rememberSignUpNext("/app/signup?plan=team", 0);
    expect(takeSignUpNext(24 * 60 * 60 * 1000 + 1)).toBeUndefined();
  });

  it("forgets an older one when the sign-up has none, and refuses other origins", () => {
    rememberSignUpNext("/app/signup?plan=team", 0);
    rememberSignUpNext(undefined);
    expect(takeSignUpNext(1)).toBeUndefined();

    localStorage.setItem("romulus.signUpNext", JSON.stringify({ next: "//evil.test", at: 0 }));
    expect(takeSignUpNext(1)).toBeUndefined();
    localStorage.setItem("romulus.signUpNext", "not json");
    expect(takeSignUpNext(1)).toBeUndefined();
  });
});
