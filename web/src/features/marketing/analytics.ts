// Cookieless page views and a few events, sent to an Umami-compatible server
// when this install sets one (`instance.analytics`: ROMULUS_ANALYTICS_*);
// otherwise nothing is loaded and every call here does nothing. What leaves
// the browser is the route's pattern (`/app/$org/$db/v/$number`), never the
// address in the bar (organization and database names, a link's token), with
// only the campaign parameters of the URL, the title "romulus", and the
// referring site's origin on the first page. Do Not Track is honoured.
//
// Events: `signup_started` (a password sign-up's form sent), `signup_completed`
// (a password account made, or the server saying the account signing in is
// new: source.ts), `first_draft_compiled` (this browser's first edit of a
// draft that compiled), each with no data beyond what is listed here. The
// one thing kept in the browser is which one-time events it sent
// (`romulus.analytics.once`), written as one goes: never with analytics off.

/** The campaign parameters kept and sent (never any other part of a URL). */
export const UTM_FIELDS = ["utm_source", "utm_medium", "utm_campaign", "utm_content"] as const;
/** The query parameter the homepage names the referring site's origin with. */
export const REF_ORIGIN = "ref_origin";

/** The referrer's origin if it is another site's http(s) page. */
export function externalOrigin(referrer: string, ownOrigin: string): string | undefined {
  try {
    const url = new URL(referrer);
    if (url.protocol !== "http:" && url.protocol !== "https:") return undefined;
    return url.origin === ownOrigin ? undefined : url.origin;
  } catch {
    return undefined;
  }
}

export type AnalyticsEvent = "signup_started" | "signup_completed" | "first_draft_compiled";

export interface AnalyticsConfig {
  script_url: string;
  website_id: string;
}

/** What Umami's tracker builds for each call (`umami.track(fn)` gets it). */
interface Payload {
  website: string;
  hostname: string;
  screen: string;
  language: string;
  title: string;
  url: string;
  referrer?: string;
  tag?: string;
  id?: string;
  name?: string;
  data?: Record<string, string>;
}

interface Umami {
  track: (payload: (props: Payload) => Payload) => unknown;
}

declare global {
  interface Window {
    umami?: Umami;
  }
}

type State = "unknown" | "off" | "loading" | "on";
let state: State = "unknown";
const queue: Array<(umami: Umami) => void> = [];
/** The page the next event belongs to, as sent. */
let page = "/app/";
/** The first page view carries the referring site, the others none. */
let firstView = true;

const TITLE = "romulus";
const ONCE_KEY = "romulus.analytics.once";

/** The URL sent for a route: `/app` + its pattern + the campaign parameters only. */
export function pageUrl(pattern: string, search: string): string {
  const params = new URLSearchParams(search);
  const kept = new URLSearchParams();
  for (const field of UTM_FIELDS) {
    const value = params.get(field);
    if (value) kept.set(field, value.slice(0, 200));
  }
  const path = pattern === "/" || pattern === "" ? "/" : pattern.replace(/\/$/, "");
  const query = kept.toString();
  return `/app${path}${query ? `?${query}` : ""}`;
}

/** Runs `call` with the tracker once it is loaded; never when analytics are off. */
function whenOn(call: (umami: Umami) => void) {
  if (state === "off") return;
  if (state === "on" && window.umami) call(window.umami);
  else queue.push(call);
}

function send(build: (props: Payload) => Payload) {
  whenOn((umami) => void umami.track(build));
}

/**
 * Called once the instance is known: loads the tracker when it has one, or
 * turns every call off (what was queued is dropped).
 */
export function startAnalytics(config: AnalyticsConfig | null | undefined): void {
  if (state !== "unknown") return;
  if (!config) {
    state = "off";
    queue.length = 0;
    return;
  }
  state = "loading";
  const script = document.createElement("script");
  script.src = config.script_url;
  script.defer = true;
  script.dataset.websiteId = config.website_id;
  // Page views are ours (route patterns), not the tracker's (full URLs).
  script.dataset.autoTrack = "false";
  script.dataset.doNotTrack = "true";
  script.onload = () => {
    const umami = window.umami;
    if (!umami) {
      script.onerror?.(new Event("error"));
      return;
    }
    state = "on";
    for (const call of queue.splice(0)) call(umami);
  };
  script.onerror = () => {
    state = "off";
    queue.length = 0;
  };
  document.head.append(script);
}

/** A page view of the route `pattern` (TanStack's `fullPath`). */
export function trackPageView(pattern: string, search = window.location.search): void {
  const url = pageUrl(pattern, search);
  page = url;
  // The homepage names the site before it (`ref_origin`): it is the referrer here.
  const referrer = firstView
    ? (externalOrigin(new URLSearchParams(search).get(REF_ORIGIN) ?? "", window.location.origin) ??
      externalOrigin(document.referrer, window.location.origin) ??
      "")
    : "";
  firstView = false;
  send((props) => ({ ...props, url, title: TITLE, referrer }));
}

/** An event, on the current page. */
export function track(name: AnalyticsEvent, data?: Record<string, string>): void {
  const url = page;
  send((props) => ({ ...props, url, title: TITLE, referrer: "", name, data }));
}

/**
 * Claims `name` in this browser's list of one-time events: true when it was
 * not there and is now. Without storage (blocked, a private window), "once"
 * cannot be kept, so never.
 */
function claimOnce(name: AnalyticsEvent): boolean {
  try {
    const sent = JSON.parse(localStorage.getItem(ONCE_KEY) ?? "[]") as unknown;
    const names = Array.isArray(sent) ? sent.filter((n) => typeof n === "string") : [];
    if (names.includes(name)) return false;
    localStorage.setItem(ONCE_KEY, JSON.stringify([...names, name]));
    return true;
  } catch {
    return false;
  }
}

/**
 * An event this browser sends once ever (`first_*`). Its flag is written only
 * once analytics are on and the event goes: with them off, or before they
 * load, nothing is stored.
 */
export function trackOnce(name: AnalyticsEvent): void {
  const url = page;
  whenOn((umami) => {
    if (!claimOnce(name)) return;
    void umami.track((props) => ({ ...props, url, title: TITLE, referrer: "", name }));
  });
}

/** Tests only: back to before `startAnalytics`. */
export function resetAnalytics(): void {
  state = "unknown";
  queue.length = 0;
  page = "/app/";
  firstView = true;
}
