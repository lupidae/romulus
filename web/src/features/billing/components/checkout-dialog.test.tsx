import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { vi } from "vitest";

import type * as mutations from "../mutations";
import { goTo } from "../mutations";
import type { PlanOffer } from "../queries";
import { CheckoutDialog } from "./checkout-dialog";

vi.mock("../mutations", async (original) => ({
  ...(await original<typeof mutations>()),
  goTo: vi.fn(),
}));

const starter: PlanOffer = {
  plan: "starter",
  name: "Starter",
  monthly_cents: 1900,
  yearly_cents: 19000,
  limits: {
    monthly_cents: 1900,
    members: null,
    databases: 3,
    versions_per_database: 500,
    schema_objects: 200,
    history_days: 365,
    playground_size: "small",
    playground_hours: 20,
    approvals: 1,
    approvals_override: false,
  },
};

function setup() {
  const fetch = vi.fn((_request: Request) =>
    Promise.resolve(Response.json({ url: "https://checkout.stripe.com/c/pay/cs_test" })),
  );
  vi.stubGlobal("fetch", fetch);
  // Radix's radio cards measure themselves; jsdom has no ResizeObserver.
  vi.stubGlobal(
    "ResizeObserver",
    class {
      observe() {}
      unobserve() {}
      disconnect() {}
    },
  );
  render(
    <QueryClientProvider client={new QueryClient()}>
      <CheckoutDialog
        org="acme"
        offer={starter}
        interval="year"
        termsUrl="https://romulus.example/terms"
        onOpenChange={() => {}}
      />
    </QueryClientProvider>,
  );
  return { fetch, user: userEvent.setup() };
}

afterEach(() => {
  vi.unstubAllGlobals();
  vi.mocked(goTo).mockReset();
});

describe("CheckoutDialog", () => {
  it("says the price and the commitment, and links the Terms in a new tab", () => {
    setup();
    expect(screen.getByRole("dialog", { name: "Upgrade to Starter" })).toHaveAccessibleDescription(
      /€190 a year, renewed every year until you cancel/,
    );
    const terms = screen.getByRole("link", { name: /Terms of Service/ });
    expect(terms).toHaveAttribute("href", "https://romulus.example/terms");
    expect(terms).toHaveAttribute("target", "_blank");
  });

  it("sends nothing until the Terms are accepted", async () => {
    const { fetch, user } = setup();
    await user.click(screen.getByRole("button", { name: "Continue to payment" }));
    expect(screen.getByRole("alert")).toHaveTextContent("Accept the Terms of Service to continue.");
    expect(screen.getByRole("checkbox", { name: /I accept the/ })).toHaveAttribute(
      "aria-invalid",
      "true",
    );
    expect(fetch).not.toHaveBeenCalled();
  });

  it("asks a consumer for both requests, then records them and goes to Stripe", async () => {
    const { fetch, user } = setup();
    await user.click(screen.getByRole("radio", { name: /An individual/ }));
    await user.click(screen.getByRole("checkbox", { name: /I accept the/ }));
    await user.click(screen.getByRole("checkbox", { name: /start now/ }));
    await user.click(screen.getByRole("button", { name: "Continue to payment" }));
    expect(screen.getByRole("alert")).toHaveTextContent("Tick both");
    expect(fetch).not.toHaveBeenCalled();

    await user.click(screen.getByRole("checkbox", { name: /if I withdraw/ }));
    await user.click(screen.getByRole("button", { name: "Continue to payment" }));
    await vi.waitFor(() => expect(goTo).toHaveBeenCalledWith(expect.stringContaining("stripe")));
    const request = fetch.mock.calls[0]![0];
    expect(new URL(request.url).pathname).toBe("/api/orgs/acme/billing/checkout");
    expect(await request.json()).toEqual({
      plan: "starter",
      interval: "year",
      accept_terms: true,
      buyer: "consumer",
      immediate_start: true,
      withdrawal_acknowledged: true,
    });
  });

  it("asks a business for the Terms only", async () => {
    const { fetch, user } = setup();
    expect(screen.queryByRole("checkbox", { name: /start now/ })).not.toBeInTheDocument();
    await user.click(screen.getByRole("checkbox", { name: /I accept the/ }));
    await user.click(screen.getByRole("button", { name: "Continue to payment" }));
    await vi.waitFor(() => expect(goTo).toHaveBeenCalled());
    const request = fetch.mock.calls[0]![0];
    expect(await request.json()).toMatchObject({
      buyer: "business",
      immediate_start: false,
      withdrawal_acknowledged: false,
    });
  });
});
