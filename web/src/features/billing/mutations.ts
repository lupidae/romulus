import { useMutation, useQueryClient } from "@tanstack/react-query";

import { api, succeed, unwrap } from "@/api/client";
import type { components } from "@/api/schema.gen";

import { billingKeys } from "./queries";

type StartCheckout = components["schemas"]["StartCheckout"];
type Locale = components["schemas"]["Locale"];
type PortalChoice = components["schemas"]["PortalChoice"];

/** Where the browser goes next (Stripe's pages); set apart so tests can watch it. */
export function goTo(url: string) {
  window.location.assign(url);
}

/** The Terms accepted, then Stripe Checkout: answers its URL. */
export function useStartCheckout(org: string) {
  return useMutation({
    mutationFn: (body: StartCheckout) =>
      unwrap(api.POST("/api/orgs/{org}/billing/checkout", { params: { path: { org } }, body })),
  });
}

/**
 * Back from Stripe's payment page unpaid: the founding-team place its
 * checkout held goes back (its session expired first).
 */
export function useCancelCheckout(org: string) {
  return useMutation({
    mutationFn: () =>
      succeed(api.POST("/api/orgs/{org}/billing/checkout/cancel", { params: { path: { org } } })),
  });
}

/** A Customer Portal session: its home page, or straight in a flow (cancel, change plan). */
export function useOpenPortal(org: string) {
  return useMutation({
    mutationFn: (flow: PortalChoice | null) =>
      unwrap(
        api.POST("/api/orgs/{org}/billing/portal", {
          params: { path: { org } },
          body: { flow },
        }),
      ),
  });
}

/**
 * A consumer withdraws from the plan (within 14 days of subscribing): the
 * acknowledgement is emailed in `locale`, the page then shows the withdrawal.
 */
export function useWithdraw(org: string) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (locale: Locale) =>
      unwrap(
        api.POST("/api/orgs/{org}/billing/withdrawal", {
          params: { path: { org } },
          body: { locale },
        }),
      ),
    onSettled: () => queryClient.invalidateQueries({ queryKey: billingKeys.one(org) }),
  });
}
