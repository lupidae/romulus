import { useQuery, useQueryClient } from "@tanstack/react-query";
import { createFileRoute, useNavigate } from "@tanstack/react-router";
import * as React from "react";

import { ApiError } from "@/api/client";
import { CreditCardIcon } from "@/components/icons";
import { PageHeader } from "@/components/shell/page-header";
import { Alert } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { EmptyState } from "@/components/ui/empty-state";
import { Skeleton } from "@/components/ui/skeleton";
import { toast } from "@/components/ui/toaster";
import { InvoicesCard } from "@/features/billing/components/invoices-card";
import { PlanPicker } from "@/features/billing/components/plan-picker";
import { PlanSummary } from "@/features/billing/components/plan-summary";
import { UsageCard } from "@/features/billing/components/usage-card";
import { billingKeys, billingQuery } from "@/features/billing/queries";

interface BillingSearch {
  /** Back from Stripe Checkout. */
  checkout?: "success" | "cancelled";
  /** The renewal reminder's link: straight to the Cancel button. */
  action?: "cancel";
}

// The plan, usage against every cap, the plans, invoices. Every member reads
// it; admins pay, change plans and cancel (Stripe's pages).
export const Route = createFileRoute("/$org/settings/billing")({
  validateSearch: (search: Record<string, unknown>): BillingSearch => ({
    checkout:
      search.checkout === "success" || search.checkout === "cancelled"
        ? search.checkout
        : undefined,
    action: search.action === "cancel" ? "cancel" : undefined,
  }),
  head: () => ({ meta: [{ title: "Billing – romulus" }] }),
  component: Billing,
});

/** Brings `element` into view within the page's own scroll area (scrollIntoView would move the app's frame too). */
function scrollToInPage(element: HTMLElement) {
  let scroller = element.parentElement;
  while (scroller && !["auto", "scroll"].includes(getComputedStyle(scroller).overflowY)) {
    scroller = scroller.parentElement;
  }
  if (!scroller) return;
  const top =
    element.getBoundingClientRect().top - scroller.getBoundingClientRect().top + scroller.scrollTop;
  scroller.scrollTo({ top: Math.max(0, top - 96) });
}

function Billing() {
  const { org } = Route.useParams();
  const { checkout, action } = Route.useSearch();
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  // Back from checkout, the webhook may land a moment after: look again for a while.
  const [waiting, setWaiting] = React.useState(checkout === "success");
  const billing = useQuery({
    ...billingQuery(org),
    refetchInterval: (query) => (waiting && query.state.data?.plan === "free" ? 2_000 : false),
  });
  const cancelRef = React.useRef<HTMLButtonElement>(null);

  // Said once (StrictMode runs effects twice), then dropped from the URL.
  const told = React.useRef(false);
  React.useEffect(() => {
    if (!checkout || told.current) return;
    told.current = true;
    if (checkout === "success") {
      toast.success("Payment received", {
        description: "Your plan shows here as soon as Stripe confirms it.",
      });
    } else {
      toast.info("Checkout cancelled", { description: "Nothing was charged." });
    }
    void navigate({ to: ".", search: { action }, replace: true });
  }, [checkout, action, navigate]);

  React.useEffect(() => {
    if (!waiting) return;
    const done = setTimeout(() => setWaiting(false), 30_000);
    return () => clearTimeout(done);
  }, [waiting]);

  // The paid plan arrived: its first invoice too.
  const plan = billing.data?.plan;
  React.useEffect(() => {
    if (waiting && plan && plan !== "free") {
      void queryClient.invalidateQueries({ queryKey: billingKeys.invoices(org) });
    }
  }, [waiting, plan, queryClient, org]);

  const loaded = billing.isSuccess;
  React.useEffect(() => {
    if (action !== "cancel" || !loaded) return;
    const button = cancelRef.current;
    if (!button) return;
    scrollToInPage(button);
    button.focus({ preventScroll: true });
  }, [action, loaded]);

  return (
    <>
      <PageHeader
        title="Billing"
        description="Your plan, what it allows and what you use. Cancel any time."
      />
      {billing.isPending ? (
        <BillingSkeleton />
      ) : billing.isError ? (
        billing.error instanceof ApiError && billing.error.status === 404 ? (
          <Card>
            <EmptyState
              icon={<CreditCardIcon />}
              title="No billing on this server"
              description="This server has no plans: nothing is capped, and there is nothing to pay."
            />
          </Card>
        ) : (
          <Alert
            variant="destructive"
            title="Could not load billing"
            action={
              <Button size="sm" loading={billing.isFetching} onClick={() => void billing.refetch()}>
                Retry
              </Button>
            }
          >
            {billing.error.message}
          </Alert>
        )
      ) : (
        <>
          <PlanSummary org={org} billing={billing.data} cancelRef={cancelRef} />
          <UsageCard billing={billing.data} />
          <PlanPicker org={org} billing={billing.data} />
          {billing.data.can_manage && <InvoicesCard org={org} />}
        </>
      )}
    </>
  );
}

function BillingSkeleton() {
  return (
    <div aria-hidden className="grid gap-6">
      <Card className="grid gap-3 p-4">
        <Skeleton className="h-6 w-32" />
        <Skeleton className="h-4 w-72" />
      </Card>
      <Card className="grid gap-4 p-4">
        {[0, 1, 2, 3].map((i) => (
          <div key={i} className="grid gap-2">
            <div className="flex justify-between">
              <Skeleton className="h-4 w-40" />
              <Skeleton className="h-4 w-16" />
            </div>
            <Skeleton className="h-1.5 w-full" />
          </div>
        ))}
      </Card>
    </div>
  );
}
