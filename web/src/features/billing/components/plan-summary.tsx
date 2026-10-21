import * as React from "react";

import { ApiError } from "@/api/client";
import { CreditCardIcon } from "@/components/icons";
import { Alert } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { toast } from "@/components/ui/toaster";

import { euros, formatLongDate, isLive, planNames, statusOf } from "../format";
import { goTo, useOpenPortal } from "../mutations";
import type { OrgBilling } from "../queries";

function failed(error: unknown) {
  toast.error("Could not open billing", {
    description: error instanceof ApiError ? error.message : "Check your connection.",
  });
}

/**
 * The plan in force: its price, when it renews or ends, a payment overdue.
 * Admins manage it in Stripe's portal, and cancel in one click from here
 * (the portal's cancel page, which ends it at the period's end).
 */
export function PlanSummary({
  org,
  billing,
  cancelRef,
}: {
  org: string;
  billing: OrgBilling;
  /** The Cancel button, focused when the renewal email's link asks (`?action=cancel`). */
  cancelRef?: React.Ref<HTMLButtonElement>;
}) {
  const portal = useOpenPortal(org);
  const [opening, setOpening] = React.useState<"home" | "cancel" | null>(null);
  const subscription = isLive(billing.subscription) ? billing.subscription : null;
  const status = subscription ? statusOf(subscription) : null;
  const open = (flow: "home" | "cancel") => {
    setOpening(flow);
    portal.mutate(flow === "cancel" ? { type: "cancel" } : null, {
      onSuccess: ({ url }) => goTo(url),
      onError: (error) => {
        setOpening(null);
        failed(error);
      },
    });
  };
  const price =
    subscription?.amount_cents != null && subscription.interval
      ? `${euros(subscription.amount_cents)} a ${subscription.interval}`
      : null;

  return (
    <Card aria-labelledby="plan-title">
      <CardHeader>
        <CardTitle id="plan-title">Plan</CardTitle>
      </CardHeader>
      <CardContent className="grid gap-4">
        <div className="flex flex-wrap items-center gap-x-3 gap-y-1">
          <p className="text-xl font-semibold text-foreground">{planNames[billing.plan]}</p>
          {status && <Badge variant={status.tone}>{status.label}</Badge>}
          {subscription && subscription.plan !== billing.plan && (
            <Badge variant="warning">Paid plan on hold</Badge>
          )}
        </div>
        <div className="grid gap-1 text-base text-muted-foreground">
          {!subscription && <p>Nothing to pay. Choose a plan below to raise its limits.</p>}
          {subscription && (
            <p>
              {price && <span className="text-foreground tabular-nums">{price}</span>}
              {price && subscription.current_period_end && !subscription.grace_until && " · "}
              {subscription.current_period_end &&
                !subscription.grace_until &&
                (subscription.cancel_at_period_end
                  ? `Ends on ${formatLongDate(subscription.current_period_end)}, then the organization is on Free. Nothing is deleted.`
                  : `Renews on ${formatLongDate(subscription.current_period_end)}`)}
            </p>
          )}
          {subscription?.scheduled && (
            <p>
              Switches to {planNames[subscription.scheduled.plan]}
              {subscription.scheduled.interval
                ? ` (${subscription.scheduled.interval === "year" ? "yearly" : "monthly"})`
                : ""}{" "}
              on {formatLongDate(subscription.scheduled.at)}.
            </p>
          )}
        </div>
        {subscription?.grace_until && (
          <Alert variant="warning" title="Payment overdue">
            <p>
              The {planNames[subscription.plan]} plan holds until{" "}
              {formatLongDate(subscription.grace_until)}. After that the organization moves to Free:
              nothing is deleted, but creating past Free&apos;s caps waits.
            </p>
            {/* Under the sentence, not beside it: on a phone the text keeps its width. */}
            {billing.can_manage && (
              <div className="mt-2">
                <Button size="sm" loading={opening === "home"} onClick={() => open("home")}>
                  Update payment method
                </Button>
              </div>
            )}
          </Alert>
        )}
      </CardContent>
      {billing.can_manage && subscription ? (
        <div className="flex flex-col gap-3 border-t border-border px-4 py-3 sm:flex-row sm:items-center sm:justify-between">
          <p className="text-sm text-muted-foreground">
            {subscription.cancel_at_period_end
              ? "To keep the plan after that date, renew it from Manage billing."
              : "Cancelling keeps the plan until the end of the period you paid for."}
          </p>
          <div className="flex flex-col-reverse gap-2 sm:flex-row">
            {!subscription.cancel_at_period_end && (
              <Button ref={cancelRef} loading={opening === "cancel"} onClick={() => open("cancel")}>
                Cancel subscription
              </Button>
            )}
            <Button loading={opening === "home"} onClick={() => open("home")}>
              <CreditCardIcon />
              Manage billing
            </Button>
          </div>
        </div>
      ) : (
        !billing.can_manage && (
          <p className="border-t border-border px-4 py-3 text-sm text-muted-foreground">
            Only admins change the plan and see invoices.
          </p>
        )
      )}
    </Card>
  );
}
