import { useQuery, useSuspenseQuery } from "@tanstack/react-query";
import { createFileRoute, Link, notFound } from "@tanstack/react-router";

import { OpenNewWindowIcon } from "@/components/icons";
import { PageHeader } from "@/components/shell/page-header";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Field } from "@/components/ui/field";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { toast } from "@/components/ui/toaster";
import { instanceQuery } from "@/features/auth/queries";
import { formatLongDate } from "@/features/billing/format";
import { contractCopy } from "@/features/legal/copy";
import { browserLocale, type Locale } from "@/features/legal/locale";
import { documentsFor, termsQuery, useSetLocale } from "@/features/legal/queries";
import { orgsQuery } from "@/features/orgs/queries";
import { canManageMembers } from "@/features/orgs/roles";

// The person's contract with the hosted service (/app/settings/contract): the
// Terms they accepted and the documents, the language of their contract and
// emails, and how to end it (each organization's Cancel button, or asking for
// the account's deletion). Not on a server without Terms.
export const Route = createFileRoute("/settings/contract")({
  loader: async ({ context: { queryClient } }) => {
    const instance = await queryClient.ensureQueryData(instanceQuery);
    if (!instance.legal) throw notFound();
    await queryClient.ensureQueryData(termsQuery);
  },
  head: () => ({ meta: [{ title: "Your contract – romulus" }] }),
  component: ContractPage,
});

function ContractPage() {
  const { data: terms } = useSuspenseQuery(termsQuery);
  const { data: orgs } = useSuspenseQuery(orgsQuery);
  const billing = useQuery(instanceQuery).data?.billing === true;
  const setLocale = useSetLocale();
  const locale: Locale = terms.locale ?? browserLocale();
  const t = contractCopy[locale];
  const links = terms.documents;
  if (!links) return null;
  const docs = documentsFor(links, locale);

  return (
    <div lang={locale} className="grid w-full max-w-3xl gap-6">
      <PageHeader title={t.title} description={t.description} />

      <Card aria-labelledby="contract-terms">
        <CardHeader>
          <CardTitle id="contract-terms">{t.termsTitle}</CardTitle>
        </CardHeader>
        <CardContent className="grid gap-4">
          <p className="text-base text-foreground">
            {terms.accepted && terms.accepted_version && terms.accepted_at
              ? t.accepted(terms.accepted_version, formatLongDate(terms.accepted_at, locale))
              : t.notAccepted}
          </p>
          <ul className="grid gap-1.5 text-sm">
            {[
              [t.terms, docs.terms],
              [t.privacy, docs.privacy],
              [t.dpa, docs.dpa],
            ].map(([label, href]) => (
              <li key={href}>
                <a
                  href={href}
                  target="_blank"
                  rel="noreferrer"
                  className="inline-flex items-center gap-1 font-medium text-primary underline-offset-4 hover:underline"
                >
                  {label}
                  <OpenNewWindowIcon aria-hidden className="size-3" />
                </a>
              </li>
            ))}
          </ul>
          <Field label={t.language} description={t.languageHint} className="max-w-xs">
            <Select
              value={locale}
              onValueChange={(value) =>
                setLocale.mutate(value as Locale, {
                  onSuccess: () => toast.success(contractCopy[value as Locale].saved),
                  onError: () => toast.error(t.saveFailed),
                })
              }
            >
              <SelectTrigger>
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="en">English</SelectItem>
                <SelectItem value="fr">Français</SelectItem>
              </SelectContent>
            </Select>
          </Field>
        </CardContent>
      </Card>

      <Card aria-labelledby="contract-cancel">
        <CardHeader>
          <CardTitle id="contract-cancel">{t.cancelTitle}</CardTitle>
        </CardHeader>
        <CardContent className="grid gap-4">
          {billing && <p className="text-base text-muted-foreground">{t.cancelBody}</p>}
          {billing &&
            (orgs.length === 0 ? (
              <p className="text-sm text-muted-foreground">{t.noOrg}</p>
            ) : (
              <ul className="grid gap-2">
                {orgs.map((org) => (
                  <li key={org.id} className="text-sm">
                    {canManageMembers(org.role) ? (
                      <Link
                        to="/$org/settings/billing"
                        params={{ org: org.slug }}
                        search={{ action: "cancel" }}
                        className="font-medium text-primary underline-offset-4 hover:underline"
                      >
                        {t.cancelOrg(org.name)}
                      </Link>
                    ) : (
                      <span className="text-muted-foreground">{t.adminsCancel(org.name)}</span>
                    )}
                  </li>
                ))}
              </ul>
            ))}
          <p className="text-sm text-muted-foreground">{t.account(links.contact)}</p>
        </CardContent>
      </Card>
    </div>
  );
}
