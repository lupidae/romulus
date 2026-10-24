import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  pageUrl,
  resetAnalytics,
  startAnalytics,
  track,
  trackOnce,
  trackPageView,
} from "./analytics";
import {
  captureSource,
  pendingReport,
  readSource,
  reportSignup,
  SOURCE_KEY,
  SOURCE_TTL,
  sourceOf,
} from "./source";

const DAY = 24 * 60 * 60 * 1000;
const NOW = Date.parse("2026-10-24T10:00:00Z");
const HOME = "https://romulus.example.com";

describe("the first touch", () => {
  beforeEach(() => localStorage.clear());

  it("keeps the campaign parameters and another site's origin, nothing else", () => {
    expect(
      sourceOf(
        `${HOME}/?utm_source=newsletter&utm_medium=email&utm_campaign=launch&utm_content=hero&utm_term=x&email=a@b.c`,
        "https://news.example.org/item?id=42",
        NOW,
      ),
    ).toEqual({
      v: 1,
      at: NOW,
      utm_source: "newsletter",
      utm_medium: "email",
      utm_campaign: "launch",
      utm_content: "hero",
      referrer: "https://news.example.org",
    });
    // This site, or no page, is no referrer.
    expect(sourceOf(`${HOME}/app/signin`, `${HOME}/pricing`, NOW)).toEqual({ v: 1, at: NOW });
    expect(sourceOf(`${HOME}/`, "", NOW)).toEqual({ v: 1, at: NOW });
  });

  it("wins for 30 days, then the next visit is the first touch", () => {
    captureSource(localStorage, `${HOME}/?utm_source=hn`, "", NOW);
    captureSource(localStorage, `${HOME}/?utm_source=ads`, "https://ads.example.com/", NOW + DAY);
    expect(readSource(localStorage, NOW + DAY)?.utm_source).toBe("hn");
    // A direct first visit wins too.
    localStorage.clear();
    captureSource(localStorage, `${HOME}/`, "", NOW);
    captureSource(localStorage, `${HOME}/?utm_source=ads`, "", NOW + DAY);
    expect(readSource(localStorage, NOW + DAY)?.utm_source).toBeUndefined();

    const later = NOW + SOURCE_TTL + 1;
    captureSource(localStorage, `${HOME}/?utm_source=ads`, "", later);
    expect(readSource(localStorage, later)).toEqual({ v: 1, at: later, utm_source: "ads" });
  });

  it("reads what the homepage wrote, and ignores what does not read", () => {
    // The shape site/public/app.js writes (its e2e test checks the same).
    localStorage.setItem(
      SOURCE_KEY,
      JSON.stringify({ v: 1, at: NOW, utm_source: "newsletter", referrer: "https://x.example" }),
    );
    expect(pendingReport(localStorage, "user-1", NOW + 90_000)).toEqual({
      utm_source: "newsletter",
      referrer: "https://x.example",
      seen_seconds_ago: 90,
    });
    for (const junk of ["{", "null", '{"v":2,"at":1}', `{"v":1,"at":${NOW + DAY}}`]) {
      localStorage.setItem(SOURCE_KEY, junk);
      expect(readSource(localStorage, NOW)).toBeUndefined();
    }
    // Nothing kept: still reported (a visit seen now).
    expect(pendingReport(undefined, "user-1", NOW)).toEqual({ seen_seconds_ago: 0 });
  });
});

describe("the report after signing in", () => {
  beforeEach(() => {
    localStorage.clear();
    resetAnalytics();
  });
  afterEach(() => {
    vi.unstubAllGlobals();
    document.head.querySelectorAll("script").forEach((s) => s.remove());
    delete window.umami;
  });

  it("is sent once per account and counts a new account as a completed sign-up", async () => {
    localStorage.setItem(SOURCE_KEY, JSON.stringify({ v: 1, at: Date.now(), utm_source: "hn" }));
    const fetch = vi.fn((_request: Request) =>
      Promise.resolve(
        new Response(JSON.stringify({ recorded: true, new_account: true }), {
          headers: { "content-type": "application/json" },
        }),
      ),
    );
    vi.stubGlobal("fetch", fetch);
    const sent: unknown[] = [];
    window.umami = { track: (build) => sent.push(build({ website: "w", url: "x" } as never)) };
    startAnalytics({ script_url: "https://stats.example.com/script.js", website_id: "w" });
    document.head.querySelector("script")?.dispatchEvent(new Event("load"));

    await reportSignup("user-1");
    await reportSignup("user-1");
    expect(fetch).toHaveBeenCalledTimes(1);
    const request = fetch.mock.calls[0]![0];
    expect(new URL(request.url).pathname).toBe("/api/me/signup-source");
    expect(await request.json()).toMatchObject({ utm_source: "hn" });
    expect(sent).toEqual([expect.objectContaining({ name: "signup_completed", url: "/app/" })]);
    expect(readSource(localStorage, Date.now())?.reported).toEqual(["user-1"]);
  });
});

describe("analytics", () => {
  beforeEach(() => {
    localStorage.clear();
    resetAnalytics();
  });
  afterEach(() => {
    document.head.querySelectorAll("script").forEach((s) => s.remove());
    delete window.umami;
  });

  it("sends route patterns with campaign parameters only", () => {
    expect(pageUrl("/$org/$db/v/$number", "?utm_source=hn&token=secret")).toBe(
      "/app/$org/$db/v/$number?utm_source=hn",
    );
    expect(pageUrl("/", "")).toBe("/app/");
    expect(pageUrl("/signin/", "?next=/app/acme")).toBe("/app/signin");
  });

  it("loads nothing when the server sets no script, and drops what was waiting", () => {
    track("signup_started");
    startAnalytics(null);
    trackPageView("/");
    expect(document.head.querySelector("script")).toBeNull();
    window.umami = { track: vi.fn() };
    trackOnce("first_draft_compiled");
    expect(window.umami.track).not.toHaveBeenCalled();
    expect(localStorage.getItem("romulus.analytics.once")).toBeNull();
  });

  it("loads the tracker without its own page views, then sends what waited", () => {
    trackPageView("/$org", "?utm_campaign=launch");
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
  });
});
