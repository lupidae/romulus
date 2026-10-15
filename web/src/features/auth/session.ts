import type { QueryClient } from "@tanstack/react-query";
import { redirect } from "@tanstack/react-router";

import { api, ApiError, unwrap } from "@/api/client";

import { meQuery } from "./queries";

/** The router's basepath (main.tsx): `next` targets carry it, the API redirects to them. */
export const APP_BASE = "/app";

/** `/app` + a router path: the form the API and the sign-in page take for `next`. */
export function appPath(routerHref: string): string {
  return routerHref.startsWith(`${APP_BASE}/`) || routerHref === APP_BASE
    ? routerHref
    : `${APP_BASE}${routerHref.startsWith("/") ? "" : "/"}${routerHref}`;
}

/** The API route that starts SSO, coming back to `next`. */
export function loginUrl(next?: string, options: { switchAccount?: boolean } = {}): string {
  const params = new URLSearchParams();
  if (next) params.set("next", next);
  if (options.switchAccount) params.set("prompt", "select_account");
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
 * invitation is for another address". `devSignin` sends people to the sign-in
 * page (which lists the dev users) instead of straight to SSO.
 */
export async function switchAccount(next: string, devSignin: boolean): Promise<void> {
  await unwrap(api.POST("/api/auth/logout"));
  window.location.assign(
    devSignin
      ? `${APP_BASE}/signin?${new URLSearchParams({ next }).toString()}`
      : loginUrl(next, { switchAccount: true }),
  );
}

/**
 * For `beforeLoad`: the signed-in user, or a redirect to sign-in that comes
 * back to `href` (the router location) afterwards.
 */
export async function requireUser(queryClient: QueryClient, href: string) {
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
