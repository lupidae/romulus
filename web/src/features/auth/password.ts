import { queryOptions, useMutation, useQueryClient } from "@tanstack/react-query";

import { api, ApiError, succeed, unwrap } from "@/api/client";
import type { components } from "@/api/schema.gen";
import { track } from "@/features/marketing/analytics";

import { meQuery } from "./queries";

export type SignedUp = components["schemas"]["SignedUp"];
export type SignUpLink = components["schemas"]["SignUpLink"];
export type ResetLink = components["schemas"]["ResetLink"];

/** The API's rule (crates/romulus-api/src/auth/password/policy.rs): checked there too. */
export const MIN_PASSWORD_LENGTH = 12;
export const MAX_PASSWORD_LENGTH = 128;

/** Characters as the API counts them (code points), not UTF-16 units. */
export function passwordLength(password: string): number {
  return [...password.normalize("NFC")].length;
}

/** What a new password lacks before the API is asked, or undefined. */
export function newPasswordProblem(password: string): string | undefined {
  const length = passwordLength(password);
  if (length === 0) return "Choose a password.";
  if (length < MIN_PASSWORD_LENGTH)
    return `Use at least ${MIN_PASSWORD_LENGTH} characters (${length} so far). A few unrelated words make a strong password.`;
  if (length > MAX_PASSWORD_LENGTH) return `Use at most ${MAX_PASSWORD_LENGTH} characters.`;
  return undefined;
}

/** "15 minutes", "a minute", "40 seconds": how long a 429 asks to wait. */
export function formatWait(seconds: number): string {
  if (seconds >= 90) return `${Math.ceil(seconds / 60)} minutes`;
  if (seconds > 45) return "a minute";
  return `${Math.max(1, Math.round(seconds))} seconds`;
}

/** The wait a 429 asks for, in seconds (`details.retry_after`). */
export function retryAfter(error: unknown): number | undefined {
  if (!(error instanceof ApiError) || error.status !== 429) return undefined;
  const details = error.details as { retry_after?: unknown } | undefined;
  return typeof details?.retry_after === "number" ? details.retry_after : undefined;
}

/** After any sign-in: the cached "signed out" answer must not send anyone back here. */
function useSignedIn() {
  const queryClient = useQueryClient();
  return () => queryClient.invalidateQueries({ queryKey: meQuery.queryKey });
}

export function usePasswordSignIn() {
  const signedIn = useSignedIn();
  return useMutation({
    mutationFn: (body: { email: string; password: string }) =>
      succeed(api.POST("/api/auth/password/sign-in", { body })),
    onSuccess: signedIn,
  });
}

export function useSignUp() {
  const signedIn = useSignedIn();
  return useMutation({
    mutationFn: (body: components["schemas"]["PasswordSignUp"]) =>
      unwrap(api.POST("/api/auth/password/sign-up", { body })),
    onSuccess: async (result) => {
      track("signup_started", { method: "password" });
      if (result.outcome === "signed_in") {
        track("signup_completed", { method: "password" });
        await signedIn();
      }
    },
  });
}

export const signUpLinkQuery = (token: string) =>
  queryOptions({
    queryKey: ["auth", "sign-up-link", token],
    queryFn: () => unwrap(api.POST("/api/auth/password/sign-up/check", { body: { token } })),
    staleTime: Infinity,
    retry: false,
  });

export function useFinishSignUp() {
  const signedIn = useSignedIn();
  return useMutation({
    mutationFn: (body: { token: string; name: string; password: string }) =>
      unwrap(api.POST("/api/auth/password/sign-up/finish", { body })),
    onSuccess: async () => {
      track("signup_completed", { method: "password" });
      await signedIn();
    },
  });
}

export function useForgotPassword() {
  return useMutation({
    mutationFn: (email: string) =>
      succeed(api.POST("/api/auth/password/forgot", { body: { email } })),
  });
}

export const resetLinkQuery = (token: string) =>
  queryOptions({
    queryKey: ["auth", "reset-link", token],
    queryFn: () => unwrap(api.POST("/api/auth/password/reset/check", { body: { token } })),
    staleTime: Infinity,
    retry: false,
  });

export function useResetPassword() {
  const signedIn = useSignedIn();
  return useMutation({
    mutationFn: (body: { token: string; password: string }) =>
      succeed(api.POST("/api/auth/password/reset", { body })),
    onSuccess: signedIn,
  });
}

export function useChangePassword() {
  return useMutation({
    mutationFn: (body: { current_password: string; new_password: string }) =>
      succeed(api.POST("/api/auth/password/change", { body })),
  });
}

/** The token a one-time link carries in its fragment (`#token=…`). */
export function readLinkToken(): string | undefined {
  return new URLSearchParams(window.location.hash.slice(1)).get("token") ?? undefined;
}

/**
 * Drops the fragment from the address bar once the page has the token, so
 * the token stays out of the history and of screenshots. In an effect: a
 * state initializer may run twice (StrictMode) and must stay pure.
 */
export function forgetLinkToken() {
  if (!window.location.hash) return;
  const { pathname, search } = window.location;
  window.history.replaceState(window.history.state, "", `${pathname}${search}`);
}

/** Field errors from a 422 `validation`, by field; anything else as one message. */
export function formErrors(error: unknown): { fields: Record<string, string>; message?: string } {
  if (error instanceof ApiError && error.code === "validation" && Object.keys(error.fields).length)
    return { fields: error.fields };
  if (error instanceof ApiError) return { fields: {}, message: error.message };
  return { fields: {}, message: "romulus did not answer. Check your connection and try again." };
}
