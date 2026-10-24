// Where a sign-up came from (docs/MVP.md "Signup sources and analytics"):
// the campaign parameters (utm_*) and the referring site's origin, carried in
// URLs only, never in the browser's storage. The homepage adds them to its
// links into the app (app.js: the referrer as `ref_origin`); a page of the
// app reads them from the address it was loaded at (or the referrer, when
// the visit came straight from another site) and keeps them in memory for
// its life; the sign-in page puts them in `next`, so the identity
// provider's way back (or a password sign-in) lands on a page that has them,
// which reports them once signed in (`POST /api/me/signup-source`). A
// password sign-up sends them with its form instead. First touch across
// visits is not kept.

import { api, unwrap } from "@/api/client";
import { safePath } from "@/lib/safe-path";

import { externalOrigin, REF_ORIGIN, track, UTM_FIELDS } from "./analytics";

export { REF_ORIGIN };
/** Every query parameter a source is made of. */
export const SOURCE_PARAMS = [...UTM_FIELDS, REF_ORIGIN] as const;
const MAX = 200;
const BASE = "http://romulus.invalid";

type UtmField = (typeof UTM_FIELDS)[number];

/** A source as the API takes it (`SignupSource`). */
export interface Source extends Partial<Record<UtmField, string>> {
  /** The referring site's origin, when it was another site. */
  referrer?: string;
}

/** What the URL `href` carries, else the referrer's origin when it is another site's. */
export function sourceOf(href: string, referrer = ""): Source | undefined {
  let url: URL;
  try {
    url = new URL(href, BASE);
  } catch {
    return undefined;
  }
  const source: Source = {};
  for (const field of UTM_FIELDS) {
    const value = url.searchParams.get(field)?.trim().slice(0, MAX);
    if (value) source[field] = value;
  }
  const origin =
    externalOrigin(url.searchParams.get(REF_ORIGIN) ?? "", url.origin) ??
    externalOrigin(referrer, url.origin);
  if (origin) source.referrer = origin;
  return Object.keys(source).length ? source : undefined;
}

/** `path` (an app path, `/app/…?…#…`) with `source`'s parameters, unless it has some already. */
export function withSource(path: string, source: Source | undefined): string {
  if (!source) return path;
  const url = new URL(path, BASE);
  if (SOURCE_PARAMS.some((param) => url.searchParams.has(param))) return path;
  for (const field of UTM_FIELDS) {
    const value = source[field];
    if (value) url.searchParams.set(field, value);
  }
  if (source.referrer) url.searchParams.set(REF_ORIGIN, source.referrer);
  return `${url.pathname}${url.search}${url.hash}`;
}

/** `path` without a source's parameters: what may be kept in storage. */
export function withoutSource(path: string | undefined): string | undefined {
  if (!path) return path;
  const url = new URL(path, BASE);
  for (const param of SOURCE_PARAMS) url.searchParams.delete(param);
  return `${url.pathname}${url.search}${url.hash}`;
}

/** The source of the address this page was loaded at (main.tsx reads it once). */
let landing: Source | undefined;

export function captureLanding(href = window.location.href, referrer = document.referrer): void {
  landing = sourceOf(href, referrer);
}

/** This page load's source, else the one `next` carries (a sign-up page opened with it). */
export function currentSource(next?: string): Source | undefined {
  const path = safePath(next);
  return landing ?? (path ? sourceOf(path) : undefined);
}

/** The sign-in page's `next`: where it was going (home by default), with this visit's source. */
export function nextWithSource(next: string | undefined): string | undefined {
  const source = currentSource(next);
  return source ? withSource(next ?? "/app/", source) : next;
}

/** The accounts this page load reported (or is reporting). */
const reporting = new Set<string>();

/**
 * Once per page load and account, after signing in: reports this page's
 * source (or that it has none). The server keeps it only for an account made
 * within the hour, so for anyone else it is one look-up; a sign-up the server
 * says just completed is counted.
 */
export async function reportSignup(userId: string): Promise<void> {
  if (reporting.has(userId)) return;
  reporting.add(userId);
  try {
    const result = await unwrap(api.POST("/api/me/signup-source", { body: landing ?? {} }));
    if (result.new_account) track("signup_completed");
  } catch {
    // Tried again on the next navigation.
    reporting.delete(userId);
  }
}

/** Tests only: a page load without a source, nothing reported. */
export function resetSource(): void {
  landing = undefined;
  reporting.clear();
}
