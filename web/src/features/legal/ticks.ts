import type { components } from "@/api/schema.gen";

type LegalLinks = components["schemas"]["LegalLinks"];

/** What the person ticked on the Terms boxes, never ticked in advance. */
export interface TermsTicks {
  terms: boolean;
  professional: boolean;
}

export const NO_TICKS: TermsTicks = { terms: false, professional: false };

/** Everything the edition in force asks is ticked. */
export function termsTicked(links: LegalLinks, ticks: TermsTicks): boolean {
  return ticks.terms && (links.consumer_sales || ticks.professional);
}
