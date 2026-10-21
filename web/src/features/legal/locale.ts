import { useQuery } from "@tanstack/react-query";

import type { components } from "@/api/schema.gen";

import { termsQuery } from "./queries";

/**
 * The languages of the hosted service's contract: the Terms, checkout,
 * billing, withdrawal and cancellation, and their emails. The rest of the app
 * is in English.
 */
export type Locale = components["schemas"]["Locale"];

export const LOCALES: readonly Locale[] = ["en", "fr"];

/** The browser's first language romulus speaks there; English otherwise. */
export function browserLocale(
  languages: readonly string[] = typeof navigator === "undefined"
    ? []
    : navigator.languages?.length
      ? navigator.languages
      : [navigator.language],
): Locale {
  for (const tag of languages) {
    const lower = tag.toLowerCase();
    if (lower === "fr" || lower.startsWith("fr-")) return "fr";
    if (lower === "en" || lower.startsWith("en-")) return "en";
  }
  return "en";
}

/** The `Intl` locale of a language: dates and amounts as it writes them. */
export function intlLocale(locale: Locale): string {
  return locale === "fr" ? "fr-FR" : "en-US";
}

/**
 * The signed-in person's language for these flows: the one their account
 * keeps (chosen when accepting the Terms, changed in account settings), else
 * the browser's.
 */
export function useLocale(): Locale {
  const terms = useQuery(termsQuery);
  return terms.data?.locale ?? browserLocale();
}
