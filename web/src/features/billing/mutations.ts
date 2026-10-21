import { useMutation } from "@tanstack/react-query";

import { api, unwrap } from "@/api/client";
import type { components } from "@/api/schema.gen";

type StartCheckout = components["schemas"]["StartCheckout"];
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
