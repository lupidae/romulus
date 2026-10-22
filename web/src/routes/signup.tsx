import { useQueries, type UseQueryResult, useSuspenseQuery } from "@tanstack/react-query";
import { createFileRoute, Link, redirect } from "@tanstack/react-router";
import type * as React from "react";

import { ArrowRightIcon, PlusIcon } from "@/components/icons";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";
import { instanceQuery } from "@/features/auth/queries";
import { requireUser } from "@/features/auth/session";
import { AuthPage, authLinkClass } from "@/features/auth/components/auth-page";
import { billingCopy } from "@/features/billing/copy";
import { isLive, planNames, priceLine } from "@/features/billing/format";
import { billingQuery, type OrgBilling } from "@/features/billing/queries";
import { readSubscribe, type Subscribe, subscribeSearch } from "@/features/billing/subscribe";
import { useLocale } from "@/features/legal/locale";
import { type Org, orgsQuery } from "@/features/orgs/queries";
import { canManageMembers } from "@/features/orgs/roles";
import { cn } from "@/lib/utils";

// The homepage's plan buttons: /app/signup?plan=team&interval=year. Signed
// out, sign-in (or sign-up) comes back here with the choice in `next`; then
// the Terms (requireUser); then the organization it is for: the first one
// made now, or one of the person's own. Its billing page opens the checkout.
// Without billing (self-hosted) or without a paid plan, the usual way in.
export const Route = createFileRoute("/signup")({
  validateSearch: (search: Record<string, unknown>): Partial<Subscribe> =>
    subscribeSearch(readSubscribe(search)),
  beforeLoad: async ({ context: { queryClient }, location, search }) => {
    const choice = readSubscribe(search);
    const instance = await queryClient.ensureQueryData(instanceQuery).catch(() => undefined);
    if (!choice || !instance?.billing) throw redirect({ to: "/", replace: true });
    await requireUser(queryClient, location.href);
    const orgs = await queryClient.ensureQueryData(orgsQuery);
    if (orgs.length === 0) throw redirect({ to: "/new", search: choice, replace: true });
    return { choice };
  },
  loader: ({ context: { queryClient } }) => queryClient.ensureQueryData(orgsQuery),
  head: () => ({ meta: [{ title: "Subscribe – romulus" }] }),
  component: ChooseOrganization,
});

function ChooseOrganization() {
  const { choice } = Route.useRouteContext();
  const locale = useLocale();
  const t = billingCopy[locale].subscribe;
  const { data: orgs } = useSuspenseQuery(orgsQuery);
  const admins = orgs.filter((o) => canManageMembers(o.role));
  // Each organization's plan, for those the person may subscribe.
  const billings = useQueries({ queries: admins.map((o) => billingQuery(o.slug)) });
  const billingOf = (slug: string) => billings[admins.findIndex((o) => o.slug === slug)];
  const offer = billings.find((b) => b.data)?.data?.plans.find((p) => p.plan === choice.plan);
  const price = offer ? priceLine(offer, choice.interval, locale) : null;
  const name = planNames[choice.plan];
  // The price says the period once the plans load; until then, in words.

  return (
    <div lang={locale} className="contents">
      <AuthPage
        title={t.title(name)}
        description={
          <>
            <span className="font-medium text-foreground">
              {price ?? t.billed(choice.interval)}
            </span>
            . {t.choose}
          </>
        }
        footer={
          <Link to="/" className={authLinkClass}>
            {t.notNow}
          </Link>
        }
      >
        <nav aria-label={t.organizations} className="grid gap-2">
          <ul className="grid gap-2">
            {orgs.map((org) => {
              const billing = canManageMembers(org.role) ? billingOf(org.slug) : undefined;
              return (
                <li key={org.slug}>
                  <OrgChoice org={org} choice={choice} billing={billing} />
                </li>
              );
            })}
          </ul>
          <Button variant="secondary" size="lg" className="mt-2 w-full" asChild>
            <Link to="/new" search={choice}>
              <PlusIcon />
              {t.newOrg}
            </Link>
          </Button>
        </nav>
      </AuthPage>
    </div>
  );
}

const rowClass =
  "flex min-h-14 items-center gap-3 rounded-lg border border-border bg-card px-4 py-3 text-left";

function OrgChoice({
  org,
  choice,
  billing,
}: {
  org: Org;
  choice: Subscribe;
  /** Its billing, for an organization the person administers. */
  billing: UseQueryResult<OrgBilling> | undefined;
}) {
  const locale = useLocale();
  const t = billingCopy[locale].subscribe;
  if (!billing) {
    return (
      <div aria-disabled className={cn(rowClass, "opacity-70")}>
        <OrgLabel name={org.name} detail={t.adminsOnly} />
      </div>
    );
  }
  if (billing.isPending) {
    return (
      <div aria-busy className={rowClass}>
        <OrgLabel name={org.name} detail={<Skeleton className="h-4 w-24" />} />
      </div>
    );
  }
  const plan = billing.data?.plan;
  const subscribed = isLive(billing.data?.subscription);
  return (
    <Link
      to="/$org/settings/billing"
      params={{ org: org.slug }}
      search={{ subscribe: choice.plan, interval: choice.interval }}
      className={cn(
        rowClass,
        "outline-none hover:border-border-strong hover:bg-muted focus-visible:ring-2 focus-visible:ring-ring",
      )}
    >
      <OrgLabel
        name={org.name}
        detail={subscribed ? t.subscribed : undefined}
        badge={plan ? t.onPlan(planNames[plan]) : undefined}
      />
      <ArrowRightIcon aria-hidden className="size-4 shrink-0 text-muted-foreground" />
    </Link>
  );
}

function OrgLabel({
  name,
  detail,
  badge,
}: {
  name: string;
  detail?: React.ReactNode;
  badge?: string;
}) {
  return (
    <span className="grid min-w-0 flex-1 gap-0.5">
      <span className="flex items-center gap-2">
        <span className="truncate text-base font-medium text-foreground">{name}</span>
        {badge && <Badge>{badge}</Badge>}
      </span>
      {detail && <span className="text-sm text-muted-foreground">{detail}</span>}
    </span>
  );
}
