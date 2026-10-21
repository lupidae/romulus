import { queryOptions, useMutation, useQueryClient } from "@tanstack/react-query";

import { api, unwrap } from "@/api/client";
import type { components } from "@/api/schema.gen";

export type TermsStatus = components["schemas"]["TermsStatus"];
export type LegalLinks = components["schemas"]["LegalLinks"];
export type DocumentLinks = components["schemas"]["DocumentLinks"];
type Locale = components["schemas"]["Locale"];
type AcceptTerms = components["schemas"]["AcceptTerms"];

/** The person's acceptance of the Terms, their language, the documents (signed in). */
export const termsQuery = queryOptions({
  queryKey: ["me", "terms"],
  queryFn: () => unwrap(api.GET("/api/me/terms")),
  staleTime: 60_000,
});

/** Accepts the Terms in force, in the language they were read in. */
export function useAcceptTerms() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (body: AcceptTerms) => unwrap(api.POST("/api/me/terms", { body })),
    onSuccess: (status) => queryClient.setQueryData(termsQuery.queryKey, status),
  });
}

/** The language of the person's contract and billing emails. */
export function useSetLocale() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (locale: Locale) => unwrap(api.PATCH("/api/me/preferences", { body: { locale } })),
    onSuccess: (status) => queryClient.setQueryData(termsQuery.queryKey, status),
  });
}

/** The pages of one language (the DPA is in English only). */
export function documentsFor(links: LegalLinks, locale: Locale): DocumentLinks {
  return locale === "fr" ? links.fr : links.en;
}

const LATER = "romulus:terms-later:";

/**
 * Someone who accepted an earlier version may read the new one later: until
 * they accept it, the version they accepted applies (Terms 21.4). Asked again
 * at the next sign-in (this browser session).
 */
export function postponeTerms(version: string) {
  try {
    sessionStorage.setItem(LATER + version, "1");
  } catch {
    // Storage refused: asked again on the next page, which is only a nuisance.
  }
}

function postponed(version: string): boolean {
  try {
    return sessionStorage.getItem(LATER + version) === "1";
  } catch {
    return false;
  }
}

/** Whether the app shows the Terms before anything else. */
export function needsTerms(status: TermsStatus): boolean {
  if (!status.required || status.accepted) return false;
  const version = status.documents?.terms_version;
  // The first acceptance cannot wait: it is the contract.
  return !(status.accepted_version && version && postponed(version));
}
