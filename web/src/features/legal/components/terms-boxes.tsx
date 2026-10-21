import type { components } from "@/api/schema.gen";

import { termsCopy } from "../copy";
import type { Locale } from "../locale";
import { documentsFor } from "../queries";
import type { TermsTicks } from "../ticks";
import { Consent, DocumentLink } from "./consent";

type LegalLinks = components["schemas"]["LegalLinks"];

/**
 * The boxes that conclude the Free plan's contract (Terms 3.4 b), in
 * `locale`: the Terms box, the line on personal data (information, nothing
 * to accept) and, in the professional edition, the professional-use box
 * with its help. The first-sign-in screen and the password sign-up form ask
 * the same words (`checkout-boxes.json`).
 */
export function TermsBoxes({
  id,
  links,
  locale,
  ticks,
  onChange,
  showErrors,
}: {
  id: string;
  links: LegalLinks;
  locale: Locale;
  ticks: TermsTicks;
  onChange: (ticks: TermsTicks) => void;
  /** After a submit: say what is missing under each box. */
  showErrors: boolean;
}) {
  const t = termsCopy[locale];
  const docs = documentsFor(links, locale);
  const pro = !links.consumer_sales;
  return (
    // Its own language inside a page that may speak another; no box of its
    // own (`contents`): the boxes sit in the form's grid.
    <div lang={locale} className="contents">
      <Consent
        id={`${id}-terms`}
        checked={ticks.terms}
        onCheckedChange={(terms) => onChange({ ...ticks, terms })}
        error={showErrors && !ticks.terms ? t.missing : undefined}
      >
        {t.termsBefore}{" "}
        <DocumentLink href={docs.terms} newTab={t.newTab}>
          {t.termsLink}
        </DocumentLink>{" "}
        {t.termsAfter(links.terms_version)}
      </Consent>
      <p className="pl-6.5 text-sm text-muted-foreground">
        {t.privacyBefore}{" "}
        <DocumentLink href={docs.privacy} newTab={t.newTab}>
          {t.privacyLink}
        </DocumentLink>
        .
      </p>
      {pro && (
        <div className="grid gap-1">
          <Consent
            id={`${id}-pro`}
            checked={ticks.professional}
            onCheckedChange={(professional) => onChange({ ...ticks, professional })}
            error={showErrors && !ticks.professional ? t.proMissing : undefined}
          >
            {t.professionalUse}
          </Consent>
          <p className="pl-6.5 text-sm text-muted-foreground">
            {t.personalBefore}{" "}
            <DocumentLink href="https://github.com/lupidae/romulus" newTab={t.newTab}>
              {t.personalLink}
            </DocumentLink>
            {locale === "fr" ? "" : " "}
            {t.personalAfter}
          </p>
        </div>
      )}
    </div>
  );
}
