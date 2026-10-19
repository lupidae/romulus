import { ApiError } from "@/api/client";

import {
  formatWait,
  newPasswordProblem,
  passwordLength,
  readLinkToken,
  retryAfter,
} from "./password";

describe("new passwords", () => {
  it("count characters as the API does, not UTF-16 units", () => {
    expect(passwordLength("🦊🦊")).toBe(2);
    expect(passwordLength("É")).toBe(1);
  });

  it("say what is missing before asking the API", () => {
    expect(newPasswordProblem("")).toBe("Choose a password.");
    expect(newPasswordProblem("short")).toMatch(/^Use at least 12 characters \(5 so far\)/);
    expect(newPasswordProblem("violet tractor mango")).toBeUndefined();
    expect(newPasswordProblem("x".repeat(129))).toBe("Use at most 128 characters.");
  });
});

describe("rate limits", () => {
  it("read the wait from a 429's details", () => {
    const limited = new ApiError(429, {
      code: "rate_limited",
      message: "Too many requests.",
      details: { retry_after: 900 },
    });
    expect(retryAfter(limited)).toBe(900);
    expect(retryAfter(new ApiError(401, { code: "invalid_credentials", message: "" }))).toBe(
      undefined,
    );
  });

  it("say waits the way people do", () => {
    expect(formatWait(900)).toBe("15 minutes");
    expect(formatWait(60)).toBe("a minute");
    expect(formatWait(20)).toBe("20 seconds");
  });
});

describe("one-time links", () => {
  it("carry their token in the fragment", () => {
    window.history.replaceState(null, "", "/app/signin/reset#token=abc");
    expect(readLinkToken()).toBe("abc");
    window.history.replaceState(null, "", "/app/signin/reset");
    expect(readLinkToken()).toBeUndefined();
  });
});
