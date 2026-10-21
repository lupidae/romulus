import { useQuery } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";

import { instanceQuery } from "@/features/auth/queries";
import { billingCopy } from "@/features/billing/copy";

import { useLocale } from "../locale";
import { LegalLinks, legalLinkClass } from "./legal-links";

/**
 * Under every page of the app on the hosted service: the Terms, the Privacy
 * Policy, and "Cancel your contract" (French Consumer Code L215-1-1: a way to
 * end it, as easy to find as the way in), which leads to the organization's
 * Cancel button, or to the account's contract page outside an organization.
 */
export function ContractFooter({ org }: { org?: string }) {
  const locale = useLocale();
  const instance = useQuery(instanceQuery).data;
  if (!instance?.legal) return null;
  const label = billingCopy[locale].contract.cancelYourContract;
  return (
    <footer className="border-t border-border pt-4">
      <LegalLinks locale={locale}>
        {org && instance.billing ? (
          <Link
            to="/$org/settings/billing"
            params={{ org }}
            search={{ action: "cancel" }}
            className={legalLinkClass}
          >
            {label}
          </Link>
        ) : (
          <Link to="/settings/contract" className={legalLinkClass}>
            {label}
          </Link>
        )}
      </LegalLinks>
    </footer>
  );
}
