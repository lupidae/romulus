import { useQueryClient, useSuspenseQuery } from "@tanstack/react-query";
import { createFileRoute, redirect, useNavigate } from "@tanstack/react-router";
import * as React from "react";

import { ApiError } from "@/api/client";
import { Alert } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { toast } from "@/components/ui/toaster";
import { ToggleGroup, ToggleGroupItem } from "@/components/ui/toggle-group";
import { AuthForm, AuthPage } from "@/features/auth/components/auth-page";
import { afterSignIn, signedIn, signOut } from "@/features/auth/session";
import { Consent, DocumentLink } from "@/features/legal/components/consent";
import { copyVersions } from "@/features/billing/copy";
import { termsCopy } from "@/features/legal/copy";
import { browserLocale, type Locale } from "@/features/legal/locale";
import {
  documentsFor,
  needsTerms,
  postponeTerms,
  termsQuery,
  useAcceptTerms,
} from "@/features/legal/queries";
import { safePath } from "@/lib/safe-path";

interface TermsSearch {
  /** Where the person was going (with the /app prefix), once they accepted. */
  next?: string;
}

// The hosted service's Terms, before anything else (Terms 3.1, 3.4 b): at the
// first sign-in, the box concludes the Free plan's contract; after a new
// version, accepting it again (21.4), or later. Everyone signed in without the
// version in force lands here (requireUser), and leaves for `next`.
export const Route = createFileRoute("/signin_/terms")({
  validateSearch: (search: Record<string, unknown>): TermsSearch => ({
    next: safePath(search.next),
  }),
  beforeLoad: async ({ context: { queryClient }, location, search }) => {
    await signedIn(queryClient, location.href);
    const terms = await queryClient.ensureQueryData(termsQuery);
    if (!needsTerms(terms)) throw redirect({ to: afterSignIn(search.next), replace: true });
  },
  head: () => ({ meta: [{ title: "Terms of Service – romulus" }] }),
  component: AcceptTermsPage,
});

function AcceptTermsPage() {
  const { next } = Route.useSearch();
  const { data: terms } = useSuspenseQuery(termsQuery);
  const queryClient = useQueryClient();
  const navigate = useNavigate();
  const accept = useAcceptTerms();
  const id = React.useId();
  const [locale, setLocale] = React.useState<Locale>(() => terms.locale ?? browserLocale());
  const [checked, setChecked] = React.useState(false);
  const [professional, setProfessional] = React.useState(false);
  const [submitted, setSubmitted] = React.useState(false);
  const [failure, setFailure] = React.useState<string>();
  const t = termsCopy[locale];
  const links = terms.documents;
  if (!links) return null;
  const docs = documentsFor(links, locale);
  const version = links.terms_version;
  // Accepted an earlier version: the one accepted applies meanwhile.
  const changed = terms.accepted_version != null;
  // Businesses only: everyone attests professional use too.
  const pro = !links.consumer_sales;

  const leave = () => void navigate({ to: afterSignIn(next), replace: true });
  const submit = (event: React.FormEvent) => {
    event.preventDefault();
    setSubmitted(true);
    setFailure(undefined);
    if (!checked || (pro && !professional)) return;
    accept.mutate(
      {
        version,
        locale,
        professional_use: pro && professional,
        copy_version: copyVersions[links.edition],
      },
      {
        onSuccess: leave,
        onError: (error) => {
          if (error instanceof ApiError && error.code === "terms_changed") {
            setChecked(false);
            setProfessional(false);
            setSubmitted(false);
            setFailure(t.versionChanged);
            void queryClient.invalidateQueries({ queryKey: termsQuery.queryKey });
            return;
          }
          setFailure(error instanceof ApiError ? error.message : t.failed);
        },
      },
    );
  };

  return (
    <div lang={locale}>
      <AuthPage
        legal={false}
        title={changed ? t.changedTitle : pro ? t.proTitle : t.firstTitle}
        description={
          changed
            ? t.changedBody(version, terms.accepted_version ?? "")
            : pro
              ? t.proIntro
              : t.firstBody
        }
      >
        <div className="grid gap-6">
          <ToggleGroup
            type="single"
            value={locale}
            aria-label={`${termsCopy.en.language} / ${termsCopy.fr.language}`}
            onValueChange={(value) => value && setLocale(value as Locale)}
            className="justify-self-start"
          >
            <ToggleGroupItem value="en" lang="en">
              English
            </ToggleGroupItem>
            <ToggleGroupItem value="fr" lang="fr">
              Français
            </ToggleGroupItem>
          </ToggleGroup>

          <AuthForm onSubmit={submit}>
            <Consent
              id={`${id}-terms`}
              checked={checked}
              onCheckedChange={setChecked}
              error={submitted && !checked ? t.missing : undefined}
            >
              {t.termsBefore}{" "}
              <DocumentLink href={docs.terms} newTab={t.newTab}>
                {t.termsLink}
              </DocumentLink>{" "}
              {t.termsAfter(version)}
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
                  checked={professional}
                  onCheckedChange={setProfessional}
                  error={submitted && !professional ? t.proMissing : undefined}
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
            {changed && (
              <p className="text-sm text-muted-foreground">{t.changedRefuse(links.contact)}</p>
            )}
            {failure && (
              <Alert variant="destructive" title={failure}>
                {null}
              </Alert>
            )}
            <Button type="submit" variant="primary" className="w-full" loading={accept.isPending}>
              {pro ? t.proSubmit : t.submit}
            </Button>
            <div className="flex flex-wrap items-center justify-center gap-x-4 gap-y-2">
              {changed && (
                <Button
                  variant="ghost"
                  onClick={() => {
                    postponeTerms(version);
                    leave();
                  }}
                >
                  {t.later}
                </Button>
              )}
              <Button
                variant="ghost"
                onClick={() => void signOut().catch(() => toast.error(t.signOutFailed))}
              >
                {t.signOut}
              </Button>
            </div>
          </AuthForm>
        </div>
      </AuthPage>
    </div>
  );
}
