import * as React from "react";

import { ApiError } from "@/api/client";
import { CheckIcon } from "@/components/icons";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardHeader, CardTitle } from "@/components/ui/card";
import { toast } from "@/components/ui/toaster";
import { ToggleGroup, ToggleGroupItem } from "@/components/ui/toggle-group";
import { cn } from "@/lib/utils";

import { direction, euros, isLive, planFeatures } from "../format";
import { goTo, useOpenPortal } from "../mutations";
import type { Interval, OrgBilling, PlanOffer } from "../queries";
import { CheckoutDialog } from "./checkout-dialog";

/**
 * Every plan, monthly or yearly (a year at ten months). The current one is
 * marked; admins move: a Free organization through checkout (the Terms
 * first), a subscribed one through Stripe's portal (upgrades now, prorated;
 * downgrades at the end of the period). Members read it.
 */
export function PlanPicker({ org, billing }: { org: string; billing: OrgBilling }) {
  const subscription = isLive(billing.subscription) ? billing.subscription : null;
  const [interval, setPeriod] = React.useState<Interval>(subscription?.interval ?? "month");
  const [buying, setBuying] = React.useState<PlanOffer | null>(null);
  const portal = useOpenPortal(org);
  const [switching, setSwitching] = React.useState<string | null>(null);

  const change = (offer: PlanOffer) => {
    setSwitching(offer.plan);
    portal.mutate(
      { type: "change_plan", plan: offer.plan, interval },
      {
        onSuccess: ({ url }) => goTo(url),
        onError: (error) => {
          setSwitching(null);
          toast.error("Could not open billing", {
            description: error instanceof ApiError ? error.message : "Check your connection.",
          });
        },
      },
    );
  };

  return (
    <Card aria-labelledby="plans-title">
      <CardHeader className="flex-wrap items-center">
        <CardTitle id="plans-title">Plans</CardTitle>
        <ToggleGroup
          type="single"
          value={interval}
          aria-label="Billing period"
          onValueChange={(value) => value && setPeriod(value as Interval)}
        >
          <ToggleGroupItem value="month">Monthly</ToggleGroupItem>
          <ToggleGroupItem value="year">Yearly, 2 months free</ToggleGroupItem>
        </ToggleGroup>
      </CardHeader>
      <ul className="grid gap-3 p-4 sm:grid-cols-2 lg:grid-cols-4">
        {billing.plans.map((offer) => {
          const current = offer.plan === billing.plan;
          const sameInterval = !subscription || subscription.interval === interval;
          const cents = interval === "month" ? offer.monthly_cents : offer.yearly_cents;
          const move = direction(billing.plan, offer.plan);
          let action: React.ReactNode = null;
          let note: string | null = null;
          if (billing.can_manage && offer.plan !== "free") {
            if (!subscription) {
              action = (
                <Button className="w-full" onClick={() => setBuying(offer)}>
                  Upgrade to {offer.name}
                </Button>
              );
            } else if (!(current && sameInterval)) {
              const label =
                current || move === "same"
                  ? `Switch to ${interval === "year" ? "yearly" : "monthly"}`
                  : `${move === "up" ? "Upgrade" : "Downgrade"} to ${offer.name}`;
              action = (
                <Button
                  className="w-full"
                  loading={switching === offer.plan}
                  onClick={() => change(offer)}
                >
                  {label}
                </Button>
              );
              // Stripe moves downgrades and shorter periods to the renewal.
              const shorter = subscription.interval === "year" && interval === "month";
              note =
                move === "down" || shorter ? "From your next renewal." : "Charged now, prorated.";
            }
          } else if (offer.plan === "free" && subscription && !current) {
            note = "What the organization keeps after cancelling.";
          }
          return (
            <li
              key={offer.plan}
              aria-current={current ? "true" : undefined}
              className={cn(
                "flex flex-col gap-3 rounded-lg border p-4",
                current ? "border-primary bg-primary/5" : "border-border",
              )}
            >
              <div className="flex items-center justify-between gap-2">
                <h3 className="text-base font-semibold text-foreground">{offer.name}</h3>
                {current && <Badge variant="primary">Current plan</Badge>}
              </div>
              <p className="flex items-baseline gap-1.5">
                <span className="text-xl font-semibold text-foreground tabular-nums">
                  {euros(cents ?? 0)}
                </span>
                <span className="text-sm text-muted-foreground">
                  {cents == null ? "forever" : `a ${interval}`}
                </span>
              </p>
              <ul className="grid gap-1.5 text-sm text-muted-foreground">
                {planFeatures(offer.limits).map((feature) => (
                  <li key={feature} className="flex items-start gap-2">
                    <CheckIcon aria-hidden className="mt-0.5 size-3.5 shrink-0 text-success" />
                    <span>{feature}</span>
                  </li>
                ))}
              </ul>
              {(action || note) && (
                <div className="mt-auto grid gap-1.5 pt-1">
                  {action}
                  {note && <p className="text-sm text-muted-foreground">{note}</p>}
                </div>
              )}
            </li>
          );
        })}
      </ul>
      <p className="border-t border-border px-4 py-3 text-sm text-muted-foreground">
        Prices in euros, VAT not applicable (French tax code, art. 293 B du CGI). Upgrades apply at
        once, prorated by Stripe; downgrades at the end of the period. Cancel any time.
      </p>
      <CheckoutDialog
        org={org}
        offer={buying}
        interval={interval}
        termsUrl={billing.terms_url}
        onOpenChange={(open) => !open && setBuying(null)}
      />
    </Card>
  );
}
