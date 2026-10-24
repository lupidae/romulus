import { useQuery, useQueryClient, useSuspenseQuery } from "@tanstack/react-query";
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
import { billingCopy } from "@/features/billing/copy";
import { isLive, planNames } from "@/features/billing/format";
import { billingKeys, billingQuery } from "@/features/billing/queries";
import { type Offer, readSubscribe, type Subscribe } from "@/features/billing/subscribe";
import { useLocale } from "@/features/legal/locale";
import { orgQuery } from "@/features/orgs/queries";

interface BillingSearch {
  /** Back from Stripe Checkout. */
  checkout?: "success" | "cancelled";
  /** The renewal reminder's link, the footer's "Cancel your contract": straight to the Cancel button. */
  action?: "cancel";
  /** A paid plan chosen on the homepage (/app/signup): its checkout opens here. */
  subscribe?: Subscribe["plan"];
  interval?: Subscribe["interval"];
  /** The founding-team offer's link (Team only). */
  offer?: Offer;
}

// The plan, usage against every cap, the plans, invoices. Every member reads
// it; admins pay, change plans and cancel (Stripe's pages).
export const Route = createFileRoute("/$org/settings/billing")({
  validateSearch: (search: Record<string, unknown>): BillingSearch => {
    const asked = readSubscribe({
      plan: search.subscribe,
      interval: search.interval,
      offer: search.offer,
    });
    return {
      checkout:
        search.checkout === "success" || search.checkout === "cancelled"
          ? search.checkout
          : undefined,
      action: search.action === "cancel" ? "cancel" : undefined,
      subscribe: asked?.plan,
      interval: asked?.interval,
      offer: asked?.offer,
    };
  },
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
  const { data: current } = useSuspenseQuery(orgQuery(org));
  const locale = useLocale();
  const words = billingCopy[locale];
  const t = words.page;
  const { checkout, action, subscribe, interval, offer } = Route.useSearch();
  // The plan chosen on the homepage, kept once the URL forgets it (below).
  const [asked] = React.useState<Subscribe | undefined>(
    subscribe && interval ? { plan: subscribe, interval, offer } : undefined,
  );
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
      toast.success(t.paid, { description: t.paidBody });
    } else {
      toast.info(t.checkoutCancelled, { description: t.nothingCharged });
    }
    void navigate({ to: ".", search: { action }, replace: true });
  }, [checkout, action, navigate, t]);

  // The choice is the dialog's from now on: a reload does not open it again.
  React.useEffect(() => {
    if (subscribe && !checkout) void navigate({ to: ".", search: { action }, replace: true });
  }, [subscribe, checkout, action, navigate]);

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

  // Asked to subscribe where a checkout cannot start: say why.
  const cannotSubscribe =
    asked && billing.isSuccess
      ? !billing.data.can_manage
        ? "admin"
        : isLive(billing.data.subscription)
          ? "subscribed"
          : null
      : null;

  // Asked to cancel (an email, the footer) with nothing to cancel here: say so.
  const nothingToCancel =
    action === "cancel" &&
    billing.isSuccess &&
    !(billing.data.can_manage && isLive(billing.data.subscription));

  return (
    <div lang={locale} className="contents">
      <PageHeader title={t.title} description={t.description} />
      {billing.isPending ? (
        <BillingSkeleton />
      ) : billing.isError ? (
        billing.error instanceof ApiError && billing.error.status === 404 ? (
          <Card>
            <EmptyState
              icon={<CreditCardIcon />}
              title={t.noBilling}
              description={t.noBillingBody}
            />
          </Card>
        ) : (
          <Alert
            variant="destructive"
            title={t.loadFailed}
            action={
              <Button size="sm" loading={billing.isFetching} onClick={() => void billing.refetch()}>
                {t.retry}
              </Button>
            }
          >
            {billing.error.message}
          </Alert>
        )
      ) : (
        <>
          {nothingToCancel && (
            <Alert
              variant="info"
              title={billing.data.can_manage ? t.nothingToCancel : t.notAdmin(current.name)}
            >
              {billing.data.can_manage
                ? t.nothingToCancelBody(current.name, billing.data.documents.contact)
                : null}
            </Alert>
          )}
          {cannotSubscribe === "admin" && (
            <Alert variant="info" title={words.subscribe.notAdmin(current.name)}>
              {null}
            </Alert>
          )}
          {cannotSubscribe === "subscribed" && asked && (
            <Alert variant="info" title={words.subscribe.alreadySubscribed(current.name)}>
              {words.subscribe.alreadySubscribedBody(planNames[asked.plan])}
            </Alert>
          )}
          <PlanSummary
            org={org}
            orgName={current.name}
            billing={billing.data}
            cancelRef={cancelRef}
          />
          <UsageCard billing={billing.data} />
          <PlanPicker org={org} billing={billing.data} asked={asked} />
          {billing.data.can_manage && <InvoicesCard org={org} />}
        </>
      )}
    </div>
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
