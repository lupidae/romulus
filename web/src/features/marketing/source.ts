// Where a visitor came from, kept in this browser for the sign-up that may
// follow (docs/MVP.md "Signup sources"): the first visit's campaign
// parameters and the referring site's origin, first touch wins, for 30 days.
// The homepage's app.js writes the same record (same origin, same key and
// shape: keep them in step); the app writes it on a first visit straight to
// /app. After signing in, the app reports it once per account
// (`POST /api/me/signup-source`), which also says whether the account is new.
// Nothing else about the visitor is kept.

import { api, unwrap } from "@/api/client";

import { externalOrigin, track, UTM_FIELDS } from "./analytics";

export const SOURCE_KEY = "romulus.source";
export const SOURCE_TTL = 30 * 24 * 60 * 60 * 1000;
const MAX = 200;

type UtmField = (typeof UTM_FIELDS)[number];

export interface KeptSource extends Partial<Record<UtmField, string>> {
  v: 1;
  /** The referring site's origin, when it was another site. */
  referrer?: string;
  /** When this browser first saw the visit (ms since the epoch). */
  at: number;
  /** The accounts that already reported it. */
  reported?: string[];
}

/** localStorage, or nothing where reading it throws (blocked storage). */
function browserStorage(): Storage | undefined {
  try {
    return window.localStorage;
  } catch {
    return undefined;
  }
}

/** The visit `href` came from, as it is kept. */
export function sourceOf(href: string, referrer: string, now: number): KeptSource {
  const url = new URL(href);
  const kept: KeptSource = { v: 1, at: now };
  for (const field of UTM_FIELDS) {
    const value = url.searchParams.get(field)?.trim().slice(0, MAX);
    if (value) kept[field] = value;
  }
  const origin = externalOrigin(referrer, url.origin);
  if (origin) kept.referrer = origin;
  return kept;
}

/** The kept record, if it reads and is younger than 30 days. */
export function readSource(storage: Storage, now: number): KeptSource | undefined {
  try {
    const raw = storage.getItem(SOURCE_KEY);
    if (!raw) return undefined;
    const kept = JSON.parse(raw) as Partial<KeptSource> | null;
    if (kept?.v !== 1 || typeof kept.at !== "number" || now - kept.at > SOURCE_TTL)
      return undefined;
    if (now < kept.at) return undefined;
    const clean: KeptSource = { v: 1, at: kept.at };
    for (const field of [...UTM_FIELDS, "referrer"] as const) {
      const value = kept[field];
      if (typeof value === "string" && value) clean[field] = value.slice(0, MAX);
    }
    if (Array.isArray(kept.reported))
      clean.reported = kept.reported.filter((id) => typeof id === "string").slice(-5);
    return clean;
  } catch {
    return undefined;
  }
}

/**
 * On every page load: keeps this visit as the source unless a younger one is
 * kept already (first touch wins). Storage refused: nothing is kept.
 */
export function captureSource(
  storage = browserStorage(),
  href = window.location.href,
  referrer = document.referrer,
  now = Date.now(),
): void {
  if (!storage) return;
  try {
    if (readSource(storage, now)) return;
    storage.setItem(SOURCE_KEY, JSON.stringify(sourceOf(href, referrer, now)));
  } catch {
    // A private window, or storage blocked: no source.
  }
}

/** What to report for `userId`, or nothing when it already did. */
export function pendingReport(storage: Storage | undefined, userId: string, now: number) {
  const kept = storage ? readSource(storage, now) : undefined;
  if (kept?.reported?.includes(userId)) return undefined;
  return {
    utm_source: kept?.utm_source,
    utm_medium: kept?.utm_medium,
    utm_campaign: kept?.utm_campaign,
    utm_content: kept?.utm_content,
    referrer: kept?.referrer,
    // Nothing kept (storage refused, or it expired): a visit seen now.
    seen_seconds_ago: kept ? Math.max(0, Math.floor((now - kept.at) / 1000)) : 0,
  };
}

function markReported(storage: Storage | undefined, userId: string, now: number) {
  if (!storage) return;
  try {
    const kept = readSource(storage, now) ?? sourceOf(window.location.href, "", now);
    kept.reported = [...(kept.reported ?? []), userId].slice(-5);
    storage.setItem(SOURCE_KEY, JSON.stringify(kept));
  } catch {
    // Reported again next time: the server answers it the same.
  }
}

/** The accounts this page load reported (or is reporting). */
const reporting = new Set<string>();

/**
 * Once per account in this browser, after signing in: reports the kept
 * source; a sign-up the server says just completed is counted.
 */
export async function reportSignup(userId: string, storage = browserStorage()): Promise<void> {
  if (reporting.has(userId)) return;
  const body = pendingReport(storage, userId, Date.now());
  if (!body) return;
  // Once per page load at most, even where storage is blocked.
  reporting.add(userId);
  try {
    const result = await unwrap(api.POST("/api/me/signup-source", { body }));
    markReported(storage, userId, Date.now());
    if (result.new_account) track("signup_completed");
  } catch {
    // Tried again on the next navigation.
    reporting.delete(userId);
  }
}
