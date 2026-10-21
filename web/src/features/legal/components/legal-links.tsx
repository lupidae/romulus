import { useQuery } from "@tanstack/react-query";
import type * as React from "react";

import { instanceQuery } from "@/features/auth/queries";
import { cn } from "@/lib/utils";

import { browserLocale, type Locale } from "../locale";
import { documentsFor } from "../queries";

const words: Record<Locale, { terms: string; privacy: string; label: string }> = {
  en: { terms: "Terms of Service", privacy: "Privacy Policy", label: "Legal" },
  fr: {
    terms: "Conditions générales",
    privacy: "Politique de confidentialité",
    label: "Informations légales",
  },
};

export const legalLinkClass =
  "rounded-sm underline-offset-4 outline-none hover:text-foreground hover:underline focus-visible:ring-2 focus-visible:ring-ring";

/**
 * The hosted service's Terms and Privacy Policy, in the reader's language,
 * then whatever the caller adds (the app's "Cancel your contract"). Nothing on
 * a server without Terms (self-hosted).
 */
export function LegalLinks({
  locale = browserLocale(),
  className,
  children,
}: {
  locale?: Locale;
  className?: string;
  children?: React.ReactNode;
}) {
  const legal = useQuery(instanceQuery).data?.legal;
  if (!legal) return null;
  const docs = documentsFor(legal, locale);
  const w = words[locale];
  return (
    <nav
      aria-label={w.label}
      lang={locale}
      className={cn("flex flex-wrap gap-x-4 gap-y-1 text-sm text-muted-foreground", className)}
    >
      <a href={docs.terms} target="_blank" rel="noreferrer" className={legalLinkClass}>
        {w.terms}
      </a>
      <a href={docs.privacy} target="_blank" rel="noreferrer" className={legalLinkClass}>
        {w.privacy}
      </a>
      {children}
    </nav>
  );
}
