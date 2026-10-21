import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { vi } from "vitest";

import { termsQuery } from "@/features/legal/queries";
import type { LegalLinks, TermsStatus } from "@/features/legal/queries";

import boxes from "../checkout-boxes.json";
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

function documents(consumerSales: boolean): LegalLinks {
  const base = consumerSales ? "/legal/full" : "/legal";
  return {
    terms_version: "1.0",
    edition: consumerSales ? "full" : "professional",
    consumer_sales: consumerSales,
    copy_version: consumerSales ? boxes.full.version : boxes.professional.version,
    contact: "legal@romulus.example",
    en: {
      terms: `https://romulus.example${base}/terms`,
      privacy: `https://romulus.example${base}/privacy`,
      dpa: "https://romulus.example/legal/dpa",
    },
    fr: {
      terms: `https://romulus.example/fr${base}/conditions`,
      privacy: `https://romulus.example/fr${base}/confidentialite`,
      dpa: "https://romulus.example/legal/dpa",
    },
  };
}

function setup({
  consumerSales = false,
  locale = "en",
}: { consumerSales?: boolean; locale?: "en" | "fr" } = {}) {
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
  const client = new QueryClient();
  const terms: TermsStatus = {
    required: true,
    accepted: true,
    accepted_version: "1.0",
    accepted_at: "2026-10-21T10:00:00Z",
    locale,
    documents: documents(consumerSales),
  };
  client.setQueryData(termsQuery.queryKey, terms);
  render(
    <QueryClientProvider client={client}>
      <CheckoutDialog
        org="acme"
        offer={starter}
        interval="year"
        documents={documents(consumerSales)}
        onOpenChange={() => {}}
      />
    </QueryClientProvider>,
  );
  return { fetch, user: userEvent.setup() };
}

async function business(user: ReturnType<typeof userEvent.setup>) {
  // Pasted, not typed: typing letter by letter is slow under load.
  await user.click(screen.getByRole("textbox", { name: /Business name|Raison sociale/ }));
  await user.paste("Acme SAS");
  const country = screen.getByRole("combobox", { name: /Country|Pays/ });
  await user.click(country);
  await user.paste("Fran");
  await user.click(await screen.findByRole("option", { name: "France" }));
}

afterEach(() => {
  vi.unstubAllGlobals();
  vi.mocked(goTo).mockReset();
});

describe("CheckoutDialog, businesses only (consumer sales off)", () => {
  it("says the price, the commitment and who it is for, with no choice of buyer", () => {
    setup();
    expect(screen.getByRole("dialog", { name: "Upgrade to Starter" })).toHaveAccessibleDescription(
      /€190 a year, renewed every year until you cancel/,
    );
    expect(
      screen.getByText(
        "For professional use only. On the payment page, add the name of your business and, if it has one, its VAT number.",
      ),
    ).toBeInTheDocument();
    expect(screen.queryByRole("radio")).not.toBeInTheDocument();
    expect(screen.queryByRole("checkbox", { name: /start now/ })).not.toBeInTheDocument();
    const terms = screen.getByRole("link", { name: /Terms of Service/ });
    expect(terms).toHaveAttribute("href", "https://romulus.example/legal/terms");
    expect(terms).toHaveAttribute("target", "_blank");
    expect(screen.getByText(/excluding taxes/)).toBeInTheDocument();
  });

  it("sends nothing until the Terms, the attestation, the name and the country are given", async () => {
    const { fetch, user } = setup();
    await user.click(screen.getByRole("button", { name: "Continue to payment" }));
    expect(screen.getByText("Accept the Terms of Service to continue.")).toBeInTheDocument();
    expect(
      screen.getByText("Confirm that you are subscribing for your business."),
    ).toBeInTheDocument();
    expect(screen.getByText("Enter your business's name.")).toBeInTheDocument();
    expect(screen.getByText("Choose your business's country.")).toBeInTheDocument();
    expect(screen.getByRole("checkbox", { name: /I have read and accept the/ })).toHaveAttribute(
      "aria-invalid",
      "true",
    );
    expect(fetch).not.toHaveBeenCalled();
  });

  it("records the business's statement and goes to Stripe", async () => {
    const { fetch, user } = setup();
    await business(user);
    await user.click(screen.getByRole("textbox", { name: /VAT or SIREN number/ }));
    await user.paste(" FR12345678901 ");
    await user.click(screen.getByRole("checkbox", { name: /I have read and accept the/ }));
    await user.click(screen.getByRole("checkbox", { name: /I am not subscribing as a consumer/ }));
    await user.click(screen.getByRole("button", { name: "Continue to payment" }));
    await vi.waitFor(() => expect(goTo).toHaveBeenCalledWith(expect.stringContaining("stripe")));
    const request = fetch.mock.calls[0]![0];
    expect(new URL(request.url).pathname).toBe("/api/orgs/acme/billing/checkout");
    expect(await request.json()).toEqual({
      plan: "starter",
      interval: "year",
      accept_terms: true,
      buyer: "business",
      business_confirmed: true,
      company_name: "Acme SAS",
      company_country: "FR",
      company_tax_id: "FR12345678901",
      immediate_start: false,
      withdrawal_acknowledged: false,
      copy_version: boxes.professional.version,
      locale: "en",
    });
  });
});

describe("CheckoutDialog, consumers too (consumer sales on)", () => {
  it("preselects nobody and asks for a choice", async () => {
    const { fetch, user } = setup({ consumerSales: true });
    for (const radio of screen.getAllByRole("radio")) expect(radio).not.toBeChecked();
    await user.click(screen.getByRole("checkbox", { name: /I have read and accept the/ }));
    await user.click(screen.getByRole("button", { name: "Continue to payment" }));
    expect(
      screen.getByText("Choose whether you are buying as a business or as an individual."),
    ).toBeInTheDocument();
    expect(fetch).not.toHaveBeenCalled();
  });

  it("asks a consumer for both requests, then records them and goes to Stripe", async () => {
    const { fetch, user } = setup({ consumerSales: true });
    await user.click(screen.getByRole("radio", { name: /An individual/ }));
    expect(screen.queryByRole("textbox", { name: /Business name/ })).not.toBeInTheDocument();
    await user.click(screen.getByRole("checkbox", { name: /I have read and accept the/ }));
    await user.click(screen.getByRole("checkbox", { name: /start now/ }));
    await user.click(screen.getByRole("button", { name: "Continue to payment" }));
    expect(screen.getByRole("alert")).toHaveTextContent("tick both boxes");
    expect(fetch).not.toHaveBeenCalled();

    await user.click(screen.getByRole("checkbox", { name: /if I withdraw/ }));
    await user.click(screen.getByRole("button", { name: "Continue to payment" }));
    await vi.waitFor(() => expect(goTo).toHaveBeenCalled());
    expect(await fetch.mock.calls[0]![0].json()).toMatchObject({
      buyer: "consumer",
      business_confirmed: false,
      company_name: null,
      immediate_start: true,
      withdrawal_acknowledged: true,
      copy_version: boxes.full.version,
    });
  });

  it("asks a business for its name, country and statement, never the consumer's boxes", async () => {
    const { fetch, user } = setup({ consumerSales: true });
    await user.click(screen.getByRole("radio", { name: /A business/ }));
    expect(screen.queryByRole("checkbox", { name: /start now/ })).not.toBeInTheDocument();
    await business(user);
    await user.click(screen.getByRole("checkbox", { name: /I have read and accept the/ }));
    await user.click(screen.getByRole("checkbox", { name: /trade, business or profession/ }));
    await user.click(screen.getByRole("button", { name: "Continue to payment" }));
    await vi.waitFor(() => expect(goTo).toHaveBeenCalled());
    expect(await fetch.mock.calls[0]![0].json()).toMatchObject({
      buyer: "business",
      business_confirmed: true,
      company_country: "FR",
      company_tax_id: null,
      immediate_start: false,
    });
  });
});

describe("CheckoutDialog in French", () => {
  it("speaks French and links the French Terms", async () => {
    const { user } = setup({ locale: "fr" });
    expect(screen.getByRole("dialog", { name: "Passer à l'offre Starter" })).toBeInTheDocument();
    expect(
      screen.getByText(/^Réservé aux professionnels\. Sur la page de paiement/),
    ).toBeInTheDocument();
    expect(screen.getByLabelText("Nom ou dénomination de l'entreprise")).toBeInTheDocument();
    expect(
      screen.getByRole("link", { name: /Conditions générales d'utilisation et de vente/ }),
    ).toHaveAttribute("href", "https://romulus.example/fr/legal/conditions");
    await user.click(screen.getByRole("button", { name: "Continuer vers le paiement" }));
    expect(
      screen.getByText("Acceptez les Conditions générales pour continuer."),
    ).toBeInTheDocument();
  });
});
