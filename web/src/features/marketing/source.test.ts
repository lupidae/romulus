import { afterEach, describe, expect, it, vi } from "vitest";

import {
  pageUrl,
  resetAnalytics,
  startAnalytics,
  track,
  trackOnce,
  trackPageView,
} from "./analytics";
import {
  captureLanding,
  currentSource,
  nextWithSource,
  reportSignup,
  resetSource,
  sourceOf,
  withoutSource,
  withSource,
} from "./source";

const HOME = "https://romulus.example.com";

/** Stands in for Umami's script: records what the app hands it. */
function fakeTracker() {
  const sent: Record<string, unknown>[] = [];
  window.umami = {
    track: (build) => sent.push(build({ website: "w", url: "x" } as never) as never),
  };
  startAnalytics({ script_url: "https://stats.example.com/script.js", website_id: "w" });
  document.head.querySelector("script")?.dispatchEvent(new Event("load"));
  return sent;
}

afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  document.head.querySelectorAll("script").forEach((s) => s.remove());
  delete window.umami;
  resetSource();
  resetAnalytics();
  localStorage.clear();
});

describe("the source, carried in URLs", () => {
  it("is the campaign parameters and another site's origin, nothing else", () => {
    expect(
      sourceOf(
        `${HOME}/app/signin?utm_source=newsletter&utm_medium=email&utm_campaign=launch&utm_content=hero&utm_term=x&email=a@b.c`,
        "https://news.example.org/item?id=42",
      ),
    ).toEqual({
      utm_source: "newsletter",
      utm_medium: "email",
      utm_campaign: "launch",
      utm_content: "hero",
      referrer: "https://news.example.org",
    });
    // The homepage names the site before it; this site, or none, is no referrer.
    expect(
      sourceOf(
        `${HOME}/app/signin?ref_origin=${encodeURIComponent("https://hn.example/x")}`,
        `${HOME}/`,
      ),
    ).toEqual({ referrer: "https://hn.example" });
    expect(sourceOf(`${HOME}/app/signin`, `${HOME}/pricing`)).toBeUndefined();
    expect(sourceOf(`${HOME}/app/?ref_origin=javascript:alert(1)`)).toBeUndefined();
  });

  it("rides in next through sign-in, and never into what is stored", () => {
    const source = { utm_source: "hn", referrer: "https://hn.example" };
    expect(withSource("/app/signup?plan=team&interval=year#x", source)).toBe(
      "/app/signup?plan=team&interval=year&utm_source=hn&ref_origin=https%3A%2F%2Fhn.example#x",
    );
    // Parameters already there are the source: never mixed with another.
    expect(withSource("/app/?utm_source=ads", source)).toBe("/app/?utm_source=ads");
    expect(withSource("/app/new", undefined)).toBe("/app/new");
    expect(withoutSource("/app/signup?plan=team&utm_source=hn&ref_origin=x")).toBe(
      "/app/signup?plan=team",
    );
    expect(withoutSource(undefined)).toBeUndefined();
  });

  it("the sign-in page lands where it was going, with the visit's source, home by default", () => {
    expect(nextWithSource(undefined)).toBeUndefined();
    expect(nextWithSource("/app/new")).toBe("/app/new");

    captureLanding(
      `${HOME}/app/signin?utm_source=newsletter&ref_origin=https://news.example.org`,
      "",
    );
    expect(nextWithSource(undefined)).toBe(
      "/app/?utm_source=newsletter&ref_origin=https%3A%2F%2Fnews.example.org",
    );
    expect(nextWithSource("/app/signup?plan=team")).toBe(
      "/app/signup?plan=team&utm_source=newsletter&ref_origin=https%3A%2F%2Fnews.example.org",
    );
  });

  it("a sign-up page opened with next has its source, without a landing one", () => {
    expect(currentSource("/app/?utm_campaign=launch")).toEqual({ utm_campaign: "launch" });
    expect(currentSource("https://evil.example/?utm_campaign=x")).toBeUndefined();
    expect(currentSource(undefined)).toBeUndefined();
  });

  it("is never written to the browser's storage", async () => {
    const setItem = vi.spyOn(Storage.prototype, "setItem");
    vi.stubGlobal(
      "fetch",
      vi.fn(() => Promise.resolve(Response.json({ recorded: true, new_account: false }))),
    );
    captureLanding(`${HOME}/app/?utm_source=hn`, "https://hn.example/");
    nextWithSource(undefined);
    await reportSignup("user-1");
    expect(setItem).not.toHaveBeenCalled();
    expect(localStorage.length).toBe(0);
  });
});

describe("the report after signing in", () => {
  it("sends the landing source once per page load and counts a new account", async () => {
    const fetch = vi.fn((_request: Request) =>
      Promise.resolve(Response.json({ recorded: true, new_account: true })),
    );
    vi.stubGlobal("fetch", fetch);
    const sent = fakeTracker();
    captureLanding(`${HOME}/app/?utm_source=hn&ref_origin=https://hn.example`, "");

    await reportSignup("user-1");
    await reportSignup("user-1");
    expect(fetch).toHaveBeenCalledTimes(1);
    const request = fetch.mock.calls[0]![0];
    expect(new URL(request.url).pathname).toBe("/api/me/signup-source");
    expect(await request.json()).toEqual({ utm_source: "hn", referrer: "https://hn.example" });
    expect(sent).toEqual([expect.objectContaining({ name: "signup_completed", url: "/app/" })]);
  });

  it("reports an empty source too (the server knows whether the account is new), again after a failure", async () => {
    const fetch = vi.fn((_request: Request): Promise<Response> =>
      Promise.reject(new Error("offline")),
    );
    vi.stubGlobal("fetch", fetch);
    await reportSignup("user-1");
    fetch.mockImplementation(() =>
      Promise.resolve(Response.json({ recorded: false, new_account: false })),
    );
    await reportSignup("user-1");
    expect(fetch).toHaveBeenCalledTimes(2);
    expect(await fetch.mock.calls[1]![0].json()).toEqual({});
  });
});

describe("analytics", () => {
  it("sends route patterns with campaign parameters only", () => {
    expect(pageUrl("/$org/$db/v/$number", "?utm_source=hn&token=secret&ref_origin=x")).toBe(
      "/app/$org/$db/v/$number?utm_source=hn",
    );
    expect(pageUrl("/", "")).toBe("/app/");
    expect(pageUrl("/signin/", "?next=/app/acme")).toBe("/app/signin");
  });

  it("takes the homepage's ref_origin as the first page's referrer", () => {
    const sent = fakeTracker();
    trackPageView("/signin", "?utm_source=hn&ref_origin=https%3A%2F%2Fhn.example%2Fitem");
    trackPageView("/new", "");
    expect(sent).toEqual([
      expect.objectContaining({ url: "/app/signin?utm_source=hn", referrer: "https://hn.example" }),
      expect.objectContaining({ url: "/app/new", referrer: "" }),
    ]);
  });

  it("loads nothing when the server sets no script, and drops what was waiting", () => {
    track("signup_started");
    trackOnce("first_draft_compiled");
    startAnalytics(null);
    trackPageView("/");
    trackOnce("first_draft_compiled");
    expect(document.head.querySelector("script")).toBeNull();
    // Off: the one-time flag is never written.
    expect(localStorage.getItem("romulus.analytics.once")).toBeNull();
  });

  it("writes a one-time event's flag only when the event goes", () => {
    trackPageView("/$org", "?utm_campaign=launch");
    trackOnce("first_draft_compiled");
    // Waiting for the tracker: nothing stored yet.
    expect(localStorage.getItem("romulus.analytics.once")).toBeNull();

    startAnalytics({ script_url: "https://stats.example.com/script.js", website_id: "site-1" });
    const script = document.head.querySelector("script")!;
    expect(script.src).toBe("https://stats.example.com/script.js");
    expect(script.dataset).toMatchObject({
      websiteId: "site-1",
      autoTrack: "false",
      doNotTrack: "true",
    });
    const sent: unknown[] = [];
    window.umami = {
      track: (build) =>
        sent.push(
          build({ website: "site-1", url: "https://x/app/acme#token=1", title: "acme" } as never),
        ),
    };
    script.dispatchEvent(new Event("load"));
    trackOnce("first_draft_compiled");
    expect(sent).toEqual([
      expect.objectContaining({
        url: "/app/$org?utm_campaign=launch",
        title: "romulus",
        website: "site-1",
      }),
      expect.objectContaining({
        name: "first_draft_compiled",
        url: "/app/$org?utm_campaign=launch",
      }),
    ]);
    expect(JSON.parse(localStorage.getItem("romulus.analytics.once")!)).toEqual([
      "first_draft_compiled",
    ]);
  });

  it("sends no one-time event where storage throws, and does not throw itself", () => {
    const sent = fakeTracker();
    const getItem = vi.spyOn(Storage.prototype, "getItem").mockImplementation(() => {
      throw new DOMException("blocked", "SecurityError");
    });
    expect(() => trackOnce("first_draft_compiled")).not.toThrow();
    getItem.mockReturnValue(null);
    vi.spyOn(Storage.prototype, "setItem").mockImplementation(() => {
      throw new DOMException("full", "QuotaExceededError");
    });
    expect(() => trackOnce("first_draft_compiled")).not.toThrow();
    expect(sent).toEqual([]);
  });
});
