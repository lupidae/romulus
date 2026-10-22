import type { QueryClient } from "@tanstack/react-query";
import { redirect } from "@tanstack/react-router";

import { api, ApiError, unwrap } from "@/api/client";
import { needsTerms, termsQuery } from "@/features/legal/queries";
import { safePath } from "@/lib/safe-path";

import { meQuery } from "./queries";

/** The router's basepath (main.tsx): `next` targets carry it, the API redirects to them. */
export const APP_BASE = "/app";

/** `/app` + a router path: the form the API and the sign-in page take for `next`. */
export function appPath(routerHref: string): string {
  return routerHref.startsWith(`${APP_BASE}/`) || routerHref === APP_BASE
    ? routerHref
    : `${APP_BASE}${routerHref.startsWith("/") ? "" : "/"}${routerHref}`;
}

/**
 * Where someone already signed in goes from the sign-in page: `next` as a
 * router path (without the /app base), or home. Checked here, not left to
 * the router: `/app//evil.test` must not become `//evil.test`.
 */
export function afterSignIn(next: string | undefined): string {
  const path = next?.startsWith(`${APP_BASE}/`) ? next.slice(APP_BASE.length) : undefined;
  return safePath(path) ?? "/";
}

// An emailed sign-up link (`/signin/welcome#token=…`) carries no `next`:
// the sign-up form keeps it in this browser until the link is opened (it
// works for 24 hours), so a plan chosen before signing up still follows.
const SIGN_UP_NEXT = "romulus.signUpNext";
const SIGN_UP_NEXT_TTL = 24 * 60 * 60 * 1000;

/** Keeps `next` for the emailed link that finishes a sign-up (or forgets it). */
export function rememberSignUpNext(next: string | undefined, now = Date.now()): void {
  try {
    if (next) localStorage.setItem(SIGN_UP_NEXT, JSON.stringify({ next, at: now }));
    else localStorage.removeItem(SIGN_UP_NEXT);
  } catch {
    // Storage refused (a private window): the link lands on the home page.
  }
}

/** The `next` a sign-up kept, once, if it is still fresh and on this origin. */
export function takeSignUpNext(now = Date.now()): string | undefined {
  try {
    const raw = localStorage.getItem(SIGN_UP_NEXT);
    localStorage.removeItem(SIGN_UP_NEXT);
    if (!raw) return undefined;
    const kept = JSON.parse(raw) as { next?: unknown; at?: unknown };
    if (typeof kept.at !== "number" || now - kept.at > SIGN_UP_NEXT_TTL) return undefined;
    return safePath(kept.next);
  } catch {
    return undefined;
  }
}

/** The API route that starts SSO, coming back to `next`. */
export function loginUrl(
  next?: string,
  options: { switchAccount?: boolean; loginHint?: string } = {},
): string {
  const params = new URLSearchParams();
  if (next) params.set("next", next);
  if (options.switchAccount) params.set("prompt", "select_account");
  // The IdP pre-fills it, or offers to create that account.
  if (options.loginHint) params.set("login_hint", options.loginHint);
  const query = params.toString();
  return `/api/auth/login${query ? `?${query}` : ""}`;
}

/** The dev-only sign-in (exists when `ROMULUS_DEV_SIGNIN` is on). */
export function devSigninUrl(email: string, next?: string): string {
  const params = new URLSearchParams({ email });
  if (next) params.set("next", next);
  return `/api/auth/dev-signin?${params.toString()}`;
}

/** Ends the session, then leaves through the IdP's logout (back to sign-in). */
export async function signOut(): Promise<void> {
  const { redirect_url } = await unwrap(api.POST("/api/auth/logout"));
  window.location.assign(redirect_url);
}

/**
 * Ends the session and signs in again, choosing the account: for "this
 * invitation is for another address". Through the IdP (`sso`, the default),
 * dev stacks included (the dev shortcut is only ever an explicit choice on
 * the sign-in page); with `sso: false`, on romulus's own sign-in page.
 */
export async function switchAccount(
  next: string,
  loginHint?: string,
  options: { sso?: boolean } = {},
): Promise<void> {
  await unwrap(api.POST("/api/auth/logout"));
  if (options.sso === false) {
    // Password accounts: our own sign-in page, the address pre-filled.
    const params = new URLSearchParams({ next });
    if (loginHint) params.set("email", loginHint);
    window.location.assign(`${APP_BASE}/signin?${params.toString()}`);
    return;
  }
  window.location.assign(loginUrl(next, { switchAccount: true, loginHint }));
}

/**
 * For `beforeLoad`: the signed-in user, or a redirect to sign-in that comes
 * back to `href` (the router location) afterwards. On the hosted service,
 * someone who has not accepted the Terms goes to them first
 * (`/signin/terms`), and back to `href` once they have.
 */
export async function requireUser(queryClient: QueryClient, href: string) {
  const me = await signedIn(queryClient, href);
  const terms = await queryClient.ensureQueryData(termsQuery);
  if (needsTerms(terms)) {
    const next = appPath(href);
    throw redirect({
      to: "/signin/terms",
      search: { next: next === `${APP_BASE}/` ? undefined : next },
    });
  }
  return me;
}

/** The signed-in user (no Terms check: the Terms page itself), or sign-in. */
export async function signedIn(queryClient: QueryClient, href: string) {
  try {
    return await queryClient.ensureQueryData(meQuery);
  } catch (error) {
    if (error instanceof ApiError && error.status === 401) {
      const next = appPath(href);
      throw redirect({
        to: "/signin",
        search: {
          // The home page is where sign-in lands anyway.
          next: next === `${APP_BASE}/` ? undefined : next,
          error: error.code === "session_expired" ? "session_expired" : undefined,
        },
      });
    }
    throw error;
  }
}
