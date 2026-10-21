import * as React from "react";

import { ApiError } from "@/api/client";
import { OpenNewWindowIcon } from "@/components/icons";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Label } from "@/components/ui/label";
import { RadioCard, RadioGroup } from "@/components/ui/radio-group";

import { checkoutCopy, checkoutProblems } from "../copy";
import { planNames, priceLine } from "../format";
import { goTo, useStartCheckout } from "../mutations";
import type { Buyer, Interval, PlanOffer } from "../queries";

interface CheckoutDialogProps {
  org: string;
  /** The plan being bought; the dialog is open while it is set. */
  offer: PlanOffer | null;
  interval: Interval;
  termsUrl: string;
  onOpenChange: (open: boolean) => void;
}

/**
 * Before Stripe Checkout: the Terms accepted (required), whether a business
 * or a consumer buys, and a consumer's request to start now with what it
 * means for withdrawal. The API records the acceptance, then answers the
 * Checkout URL the browser goes to.
 */
export function CheckoutDialog({
  org,
  offer,
  interval,
  termsUrl,
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
            termsUrl={termsUrl}
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
  termsUrl,
  onCancel,
}: {
  org: string;
  offer: PlanOffer;
  interval: Interval;
  termsUrl: string;
  onCancel: () => void;
}) {
  const start = useStartCheckout(org);
  const id = React.useId();
  const [buyer, setBuyer] = React.useState<Buyer>("business");
  const [acceptTerms, setAcceptTerms] = React.useState(false);
  const [immediateStart, setImmediateStart] = React.useState(false);
  const [withdrawal, setWithdrawal] = React.useState(false);
  const [submitted, setSubmitted] = React.useState(false);
  const [failure, setFailure] = React.useState<string>();
  const consumer = buyer === "consumer";
  const problems = checkoutProblems({ acceptTerms, consumer, immediateStart, withdrawal });
  const shown = submitted ? problems : {};
  const name = planNames[offer.plan];

  const submit = (event: React.FormEvent) => {
    event.preventDefault();
    setSubmitted(true);
    setFailure(undefined);
    if (problems.terms || problems.consumer) return;
    start.mutate(
      {
        plan: offer.plan,
        interval,
        accept_terms: true,
        buyer,
        immediate_start: consumer && immediateStart,
        withdrawal_acknowledged: consumer && withdrawal,
      },
      {
        onSuccess: ({ url }) => goTo(url),
        onError: (error) =>
          setFailure(
            error instanceof ApiError ? error.message : "Could not reach billing. Try again.",
          ),
      },
    );
  };

  return (
    <form noValidate onSubmit={submit} className="grid gap-4">
      <DialogHeader>
        <DialogTitle>Upgrade to {name}</DialogTitle>
        <DialogDescription>
          {priceLine(offer, interval)}, {checkoutCopy.commitment(interval)}
        </DialogDescription>
      </DialogHeader>

      <fieldset className="grid gap-2">
        <legend className="mb-2 text-sm font-medium text-foreground">
          {checkoutCopy.buyerLegend}
        </legend>
        <RadioGroup
          value={buyer}
          onValueChange={(value) => setBuyer(value as Buyer)}
          className="sm:grid-cols-2"
        >
          <RadioCard
            value="business"
            label={checkoutCopy.business.label}
            description={checkoutCopy.business.description}
          />
          <RadioCard
            value="consumer"
            label={checkoutCopy.consumer.label}
            description={checkoutCopy.consumer.description}
          />
        </RadioGroup>
      </fieldset>

      <div className="grid gap-3">
        <Consent
          id={`${id}-terms`}
          checked={acceptTerms}
          onCheckedChange={setAcceptTerms}
          error={shown.terms}
        >
          {checkoutCopy.termsBefore}{" "}
          <a
            href={termsUrl}
            target="_blank"
            rel="noreferrer"
            className="inline-flex items-center gap-0.5 font-medium text-primary underline-offset-4 hover:underline"
          >
            {checkoutCopy.termsLink}
            <OpenNewWindowIcon aria-hidden className="size-3" />
            <span className="sr-only"> (opens in a new tab)</span>
          </a>
          .
        </Consent>
        {consumer && (
          <div className="grid gap-3 rounded-lg border border-border bg-muted/40 p-3">
            <Consent
              id={`${id}-start`}
              checked={immediateStart}
              onCheckedChange={setImmediateStart}
              invalid={!!shown.consumer && !immediateStart}
            >
              {checkoutCopy.immediateStart}
            </Consent>
            <Consent
              id={`${id}-withdrawal`}
              checked={withdrawal}
              onCheckedChange={setWithdrawal}
              invalid={!!shown.consumer && !withdrawal}
            >
              {checkoutCopy.withdrawal}
            </Consent>
            {shown.consumer && (
              <p role="alert" className="text-sm text-destructive">
                {shown.consumer}
              </p>
            )}
          </div>
        )}
      </div>

      <p className="text-sm text-muted-foreground">{checkoutCopy.next}</p>
      {failure && (
        <p role="alert" className="text-sm text-destructive">
          {failure}
        </p>
      )}
      <DialogFooter>
        <Button onClick={onCancel}>Cancel</Button>
        <Button type="submit" variant="primary" loading={start.isPending}>
          Continue to payment
        </Button>
      </DialogFooter>
    </form>
  );
}

/** A box to tick and what ticking it says; its error under it. */
function Consent({
  id,
  checked,
  onCheckedChange,
  error,
  invalid,
  children,
}: {
  id: string;
  checked: boolean;
  onCheckedChange: (checked: boolean) => void;
  error?: string;
  invalid?: boolean;
  children: React.ReactNode;
}) {
  const errorId = `${id}-error`;
  return (
    <div className="grid gap-1">
      <div className="flex items-start gap-2.5">
        <Checkbox
          id={id}
          checked={checked}
          onCheckedChange={(value) => onCheckedChange(value === true)}
          aria-invalid={error || invalid ? true : undefined}
          aria-describedby={error ? errorId : undefined}
          className="mt-0.5"
        />
        <Label htmlFor={id} className="leading-snug font-normal">
          {children}
        </Label>
      </div>
      {error && (
        <p id={errorId} role="alert" className="pl-6.5 text-sm text-destructive">
          {error}
        </p>
      )}
    </div>
  );
}
