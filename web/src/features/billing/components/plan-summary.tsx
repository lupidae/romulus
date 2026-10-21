import * as React from "react";

import { ApiError } from "@/api/client";
import { CreditCardIcon } from "@/components/icons";
import { Alert } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { ConfirmDialog } from "@/components/ui/confirm-dialog";
import { toast } from "@/components/ui/toaster";
import { type Locale, useLocale } from "@/features/legal/locale";

import { billingCopy } from "../copy";
import { formatLongDate, formatMoment, isLive, money, planNames, statusOf } from "../format";
import { goTo, useOpenPortal, useWithdraw } from "../mutations";
import type { OrgBilling } from "../queries";

/**
 * The plan in force: its price, when it renews or ends, a payment overdue.
 * Admins manage it in Stripe's portal, and cancel in one click from here
 * (the portal's cancel page, which ends it at the period's end). A consumer
 * may withdraw for 14 days (Terms 9.4): one button, one confirmation, as easy
 * as subscribing; a business never sees it.
 */
export function PlanSummary({
  org,
  orgName,
  billing,
  cancelRef,
}: {
  org: string;
  orgName: string;
  billing: OrgBilling;
  /** The Cancel button, focused when an email or the footer asks (`?action=cancel`). */
  cancelRef?: React.Ref<HTMLButtonElement>;
}) {
  const locale = useLocale();
  const t = billingCopy[locale].summary;
  const portal = useOpenPortal(org);
  const [opening, setOpening] = React.useState<"home" | "cancel" | null>(null);
  const subscription = isLive(billing.subscription) ? billing.subscription : null;
  const status = subscription ? statusOf(subscription, locale) : null;
  const open = (flow: "home" | "cancel") => {
    setOpening(flow);
    portal.mutate(flow === "cancel" ? { type: "cancel" } : null, {
      onSuccess: ({ url }) => goTo(url),
      onError: (error) => {
        setOpening(null);
        toast.error(t.portalFailed, {
          description: error instanceof ApiError ? error.message : t.checkConnection,
        });
      },
    });
  };
  const price =
    subscription?.amount_cents != null && subscription.interval
      ? t.price(
          money(subscription.amount_cents, subscription.currency ?? "eur", locale),
          subscription.interval,
        )
      : null;

  return (
    <Card aria-labelledby="plan-title" lang={locale}>
      <CardHeader>
        <CardTitle id="plan-title">{t.title}</CardTitle>
      </CardHeader>
      <CardContent className="grid gap-4">
        <div className="flex flex-wrap items-center gap-x-3 gap-y-1">
          <p className="text-xl font-semibold text-foreground">{planNames[billing.plan]}</p>
          {status && <Badge variant={status.tone}>{status.label}</Badge>}
          {subscription && subscription.plan !== billing.plan && (
            <Badge variant="warning">{t.onHold}</Badge>
          )}
        </div>
        <div className="grid gap-1 text-base text-muted-foreground">
          {!subscription && !billing.withdrawal?.withdrawn && <p>{t.nothingToPay}</p>}
          {subscription && (
            <p>
              {price && <span className="text-foreground tabular-nums">{price}</span>}
              {price && subscription.current_period_end && !subscription.grace_until && " · "}
              {subscription.current_period_end &&
                !subscription.grace_until &&
                (subscription.cancel_at_period_end
                  ? t.ends(formatLongDate(subscription.current_period_end, locale))
                  : t.renews(formatLongDate(subscription.current_period_end, locale)))}
            </p>
          )}
          {subscription?.scheduled && (
            <p>
              {t.switches(
                planNames[subscription.scheduled.plan],
                subscription.scheduled.interval ?? null,
                formatLongDate(subscription.scheduled.at, locale),
              )}
            </p>
          )}
        </div>
        {subscription?.grace_until && (
          <Alert variant="warning" title={t.overdue}>
            <p>
              {t.overdueBody(
                planNames[subscription.plan],
                formatLongDate(subscription.grace_until, locale),
              )}
            </p>
            {/* Under the sentence, not beside it: on a phone the text keeps its width. */}
            {billing.can_manage && (
              <div className="mt-2">
                <Button size="sm" loading={opening === "home"} onClick={() => open("home")}>
                  {t.updatePayment}
                </Button>
              </div>
            )}
          </Alert>
        )}
        <Withdrawal org={org} orgName={orgName} billing={billing} locale={locale} />
      </CardContent>
      {billing.can_manage && subscription ? (
        <div className="flex flex-col gap-3 border-t border-border px-4 py-3 sm:flex-row sm:items-center sm:justify-between">
          <p className="text-sm text-muted-foreground">
            {subscription.cancel_at_period_end ? t.renewHint : t.cancelHint}
          </p>
          <div className="flex flex-col-reverse gap-2 sm:flex-row">
            {!subscription.cancel_at_period_end && (
              <Button ref={cancelRef} loading={opening === "cancel"} onClick={() => open("cancel")}>
                {t.cancel}
              </Button>
            )}
            <Button loading={opening === "home"} onClick={() => open("home")}>
              <CreditCardIcon />
              {t.manage}
            </Button>
          </div>
        </div>
      ) : (
        !billing.can_manage && (
          <p className="border-t border-border px-4 py-3 text-sm text-muted-foreground">
            {t.adminsOnly}
          </p>
        )
      )}
    </Card>
  );
}

/**
 * A consumer's right of withdrawal (Terms 9.4; L221-21): while it runs, what
 * it does and the "Withdraw from contract here" button, then "Confirm
 * withdrawal"; once used, when and what is refunded. Nothing for a business.
 */
function Withdrawal({
  org,
  orgName,
  billing,
  locale,
}: {
  org: string;
  orgName: string;
  billing: OrgBilling;
  locale: Locale;
}) {
  const t = billingCopy[locale].withdrawal;
  const withdraw = useWithdraw(org);
  const [confirming, setConfirming] = React.useState(false);
  const info = billing.withdrawal;
  if (!info) return null;

  if (info.withdrawn) {
    const done = info.withdrawn;
    return (
      <Alert variant="info" title={t.withdrawnTitle(formatMoment(done.requested_at, locale))}>
        {t.withdrawnBody(money(done.refund_cents, done.currency, locale), done.completed)}
      </Alert>
    );
  }
  if (!info.until || !billing.can_manage) return null;
  // The last day it can be used: the period ends at midnight after it.
  const lastDay = new Date(new Date(info.until).getTime() - 1000).toISOString();
  const plan = planNames[billing.subscription?.plan ?? billing.plan];

  return (
    <section
      aria-labelledby="withdrawal-title"
      className="grid gap-3 rounded-lg border border-border p-4"
    >
      <div className="grid gap-1">
        <h3 id="withdrawal-title" className="text-base font-semibold text-foreground">
          {t.title}
        </h3>
        <p className="text-sm text-muted-foreground">
          {t.body(formatLongDate(lastDay, locale, "UTC"))}
        </p>
      </div>
      <div>
        <Button onClick={() => setConfirming(true)}>{t.button}</Button>
      </div>
      <ConfirmDialog
        open={confirming}
        onOpenChange={setConfirming}
        title={t.confirmTitle(plan)}
        description={t.confirmBody(orgName)}
        confirmLabel={t.confirm}
        cancelLabel={t.keep}
        onConfirm={() =>
          withdraw.mutateAsync(locale).then(
            (done) =>
              toast.success(t.done, {
                description: t.doneBody(money(done.refund_cents, done.currency, locale)),
              }),
            (error: unknown) => {
              toast.error(t.failed, {
                description: error instanceof ApiError ? error.message : undefined,
              });
              throw error;
            },
          )
        }
      />
    </section>
  );
}
