import * as React from "react";

import { ApiError } from "@/api/client";
import { Button } from "@/components/ui/button";
import { Combobox } from "@/components/ui/combobox";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Field } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import { RadioCard, RadioGroup } from "@/components/ui/radio-group";
import { Consent, DocumentLink } from "@/features/legal/components/consent";
import { useLocale } from "@/features/legal/locale";
import { documentsFor, type LegalLinks } from "@/features/legal/queries";

import { type Buyer, checkoutCopy, checkoutProblems, copyVersions } from "../copy";
import { countries, matchCountries } from "../countries";
import { planNames, priceLine } from "../format";
import { goTo, useStartCheckout } from "../mutations";
import type { Interval, PlanOffer } from "../queries";

interface CheckoutDialogProps {
  org: string;
  /** The plan being bought; the dialog is open while it is set. */
  offer: PlanOffer | null;
  interval: Interval;
  /** The Terms in force and the other documents (the billing page's). */
  documents: LegalLinks;
  onOpenChange: (open: boolean) => void;
}

/**
 * Before Stripe Checkout (Terms 3.4 c, 3.5, 9.3): the price and the
 * commitment; as whom the plan is bought, chosen (never preselected) when
 * consumers may buy, a business otherwise; a business's name, country and
 * statement, or a consumer's two requests; the Terms, in the reader's
 * language. The API records all of it, then answers the Checkout URL the
 * browser goes to.
 */
export function CheckoutDialog({
  org,
  offer,
  interval,
  documents,
  onOpenChange,
}: CheckoutDialogProps) {
  return (
    <Dialog open={offer !== null} onOpenChange={onOpenChange}>
      <DialogContent fit className="max-w-lg">
        {offer && (
          <CheckoutForm
            key={`${offer.plan}:${interval}`}
            org={org}
            offer={offer}
            interval={interval}
            documents={documents}
            onCancel={() => onOpenChange(false)}
          />
        )}
      </DialogContent>
    </Dialog>
  );
}

function CheckoutForm({
  org,
  offer,
  interval,
  documents,
  onCancel,
}: {
  org: string;
  offer: PlanOffer;
  interval: Interval;
  documents: LegalLinks;
  onCancel: () => void;
}) {
  const locale = useLocale();
  const copy = checkoutCopy[locale];
  const docs = documentsFor(documents, locale);
  const consumerSales = documents.consumer_sales;
  // Businesses only (the professional edition): its own box and words.
  const pro = !consumerSales;
  const start = useStartCheckout(org);
  const id = React.useId();
  const [chosen, setBuyer] = React.useState<Buyer | undefined>();
  // Businesses only: nothing to choose.
  const buyer: Buyer | undefined = consumerSales ? chosen : "business";
  const [companyName, setCompanyName] = React.useState("");
  const [countryText, setCountryText] = React.useState("");
  const [country, setCountry] = React.useState<string>();
  const [taxId, setTaxId] = React.useState("");
  const [acceptTerms, setAcceptTerms] = React.useState(false);
  const [businessConfirmed, setBusinessConfirmed] = React.useState(false);
  const [immediateStart, setImmediateStart] = React.useState(false);
  const [withdrawal, setWithdrawal] = React.useState(false);
  const [submitted, setSubmitted] = React.useState(false);
  const [failure, setFailure] = React.useState<string>();
  const allCountries = React.useMemo(() => countries(locale), [locale]);
  const problems = checkoutProblems(
    {
      pro,
      buyer,
      acceptTerms,
      businessConfirmed,
      companyName,
      country,
      immediateStart,
      withdrawal,
    },
    locale,
  );
  const shown = submitted ? problems : {};
  const name = planNames[offer.plan];

  const submit = (event: React.FormEvent) => {
    event.preventDefault();
    setSubmitted(true);
    setFailure(undefined);
    if (Object.values(problems).some(Boolean)) return;
    const business = buyer === "business";
    start.mutate(
      {
        plan: offer.plan,
        interval,
        accept_terms: true,
        buyer,
        business_confirmed: business,
        company_name: business ? companyName.trim() : null,
        company_country: business ? (country ?? null) : null,
        company_tax_id: business && taxId.trim() ? taxId.trim() : null,
        immediate_start: !business && immediateStart,
        withdrawal_acknowledged: !business && withdrawal,
        copy_version: copyVersions[documents.edition],
        locale,
      },
      {
        onSuccess: ({ url }) => goTo(url),
        onError: (error) =>
          setFailure(
            error instanceof ApiError
              ? error.code === "checkout_copy_changed"
                ? copy.copyChanged
                : error.message
              : copy.unreachable,
          ),
      },
    );
  };

  return (
    <form noValidate onSubmit={submit} className="grid gap-4" lang={locale}>
      <DialogHeader>
        <DialogTitle>{copy.title(name)}</DialogTitle>
        <DialogDescription>
          {priceLine(offer, interval, locale)}, {copy.commitment(interval)}
        </DialogDescription>
      </DialogHeader>

      {pro && <p className="text-sm font-medium text-foreground">{copy.pro.intro}</p>}

      {consumerSales && (
        <fieldset className="grid gap-2">
          <legend className="mb-2 text-sm font-medium text-foreground">{copy.buyerLegend}</legend>
          <RadioGroup
            value={chosen ?? ""}
            onValueChange={(value) => setBuyer(value as Buyer)}
            aria-invalid={shown.buyer ? true : undefined}
            aria-describedby={shown.buyer ? `${id}-buyer-error` : undefined}
            className="sm:grid-cols-2"
          >
            <RadioCard
              value="business"
              label={copy.business.label}
              description={copy.business.description}
            />
            <RadioCard
              value="consumer"
              label={copy.consumer.label}
              description={copy.consumer.description}
            />
          </RadioGroup>
          {shown.buyer && (
            <p id={`${id}-buyer-error`} role="alert" className="text-sm text-destructive">
              {shown.buyer}
            </p>
          )}
        </fieldset>
      )}

      {buyer === "business" && (
        <div className="grid items-start gap-3 sm:grid-cols-2">
          <Field label={copy.companyName} error={shown.companyName} className="sm:col-span-2">
            <Input
              value={companyName}
              onChange={(event) => setCompanyName(event.target.value)}
              autoComplete="organization"
              maxLength={200}
            />
          </Field>
          <Field label={copy.country} error={shown.country}>
            <Combobox
              value={countryText}
              onValueChange={(text) => {
                setCountryText(text);
                // Typed over: the country is the one picked, or none.
                const exact = allCountries.find(
                  (c) => c.label.toLowerCase() === text.trim().toLowerCase(),
                );
                setCountry(exact?.value);
              }}
              options={matchCountries(allCountries, countryText)}
              onPick={(option) => {
                setCountry(option.value);
                setCountryText(option.label);
              }}
              empty={copy.countryNone}
              placeholder={copy.countryPlaceholder}
              autoComplete="off"
              selectOnFocus
            />
          </Field>
          <Field label={copy.taxId} description={copy.taxIdHint}>
            <Input
              value={taxId}
              onChange={(event) => setTaxId(event.target.value)}
              maxLength={40}
              autoComplete="off"
              spellCheck={false}
            />
          </Field>
        </div>
      )}

      <div className="grid gap-3">
        <Consent
          id={`${id}-terms`}
          checked={acceptTerms}
          onCheckedChange={setAcceptTerms}
          error={shown.terms}
        >
          {copy.termsBefore}{" "}
          <DocumentLink href={docs.terms} newTab={copy.newTab}>
            {copy.termsLink}
          </DocumentLink>{" "}
          {copy.termsAfter(documents.terms_version)}
        </Consent>
        <p className="pl-6.5 text-sm text-muted-foreground">
          {copy.privacyBefore}{" "}
          <DocumentLink href={docs.privacy} newTab={copy.newTab}>
            {copy.privacyLink}
          </DocumentLink>
          .
        </p>
        {buyer === "business" && (
          <Consent
            id={`${id}-business`}
            checked={businessConfirmed}
            onCheckedChange={setBusinessConfirmed}
            error={shown.business}
          >
            {pro ? copy.pro.businessBefore : copy.businessBefore}
            {locale === "fr" ? "" : " "}
            <DocumentLink href={docs.dpa} newTab={copy.newTab}>
              {pro ? copy.pro.businessLink : copy.businessLink}
            </DocumentLink>{" "}
            {pro ? copy.pro.businessAfter : copy.businessAfter}
          </Consent>
        )}
        {buyer === "consumer" && (
          <div className="grid gap-3 rounded-lg border border-border bg-muted/40 p-3">
            <Consent
              id={`${id}-start`}
              checked={immediateStart}
              onCheckedChange={setImmediateStart}
              invalid={!!shown.consumer && !immediateStart}
            >
              {copy.immediateStart}
            </Consent>
            <Consent
              id={`${id}-withdrawal`}
              checked={withdrawal}
              onCheckedChange={setWithdrawal}
              invalid={!!shown.consumer && !withdrawal}
            >
              {copy.withdrawal}
            </Consent>
            {shown.consumer && (
              <p role="alert" className="text-sm text-destructive">
                {shown.consumer}
              </p>
            )}
          </div>
        )}
      </div>

      <p className="text-sm text-muted-foreground">{pro ? copy.pro.next : copy.next}</p>
      {failure && (
        <p role="alert" className="text-sm text-destructive">
          {failure}
        </p>
      )}
      <DialogFooter>
        <Button onClick={onCancel}>{copy.cancel}</Button>
        <Button type="submit" variant="primary" loading={start.isPending}>
          {copy.submit}
        </Button>
      </DialogFooter>
    </form>
  );
}
