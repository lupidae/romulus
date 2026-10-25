import { queryOptions } from "@tanstack/react-query";

import { api, unwrap } from "@/api/client";
import type { components } from "@/api/schema.gen";

export type OrgBilling = components["schemas"]["OrgBilling"];
export type Plan = components["schemas"]["Plan"];
export type Interval = components["schemas"]["Interval"];
export type PlanOffer = components["schemas"]["PlanOffer"];
export type PlanLimits = components["schemas"]["Limits"];
export type Usage = components["schemas"]["Usage"];
export type Meter = components["schemas"]["Meter"];
export type HoursMeter = components["schemas"]["HoursMeter"];
export type SubscriptionInfo = components["schemas"]["SubscriptionInfo"];
export type InvoiceSummary = components["schemas"]["InvoiceSummary"];
export type Buyer = components["schemas"]["Buyer"];
export type PlanLimitDetails = components["schemas"]["PlanLimitDetails"];
export type Cap = components["schemas"]["Cap"];
export type FoundingOffer = components["schemas"]["FoundingOffer"];

export const billingKeys = {
  one: (org: string) => ["orgs", org, "billing"] as const,
  invoices: (org: string) => ["orgs", org, "billing", "invoices"] as const,
};

/** The plan, the subscription and usage against every cap (any member). */
export const billingQuery = (org: string) =>
  queryOptions({
    queryKey: billingKeys.one(org),
    queryFn: () => unwrap(api.GET("/api/orgs/{org}/billing", { params: { path: { org } } })),
  });

/** The organization's invoices, newest first (admins). */
export const invoicesQuery = (org: string) =>
  queryOptions({
    queryKey: billingKeys.invoices(org),
    queryFn: () =>
      unwrap(api.GET("/api/orgs/{org}/billing/invoices", { params: { path: { org } } })),
  });
