import { termsCopy } from "@/features/legal/copy";

import boxes from "./checkout-boxes.json";
import { checkoutCopy } from "./copy";

// What the screens show must read exactly as checkout-boxes.json says: the
// API records that file's version (per edition) with each acceptance and
// quotes it in the confirmation email.
describe.each(["en", "fr"] as const)("the %s boxes", (locale) => {
  const copy = checkoutCopy[locale];
  const full = boxes.full[locale];
  const pro = boxes.professional[locale];
  const glue = locale === "fr" ? "" : " ";

  it("of the full edition say what the shared file says", () => {
    expect(`${copy.termsBefore} ${copy.termsLink} ${copy.termsAfter("1.0")}`).toBe(
      full.terms.replace("{version}", "1.0"),
    );
    expect(`${copy.businessBefore}${glue}${copy.businessLink} ${copy.businessAfter}`).toBe(
      full.business,
    );
    expect(copy.immediateStart).toBe(full.immediate_start);
    expect(copy.withdrawal).toBe(full.withdrawal);
  });

  it("of the professional edition say what the shared file says", () => {
    expect(
      `${copy.pro.businessBefore}${glue}${copy.pro.businessLink} ${copy.pro.businessAfter}`,
    ).toBe(pro.business);
    expect(pro.terms).toBe(full.terms);
    const terms = termsCopy[locale];
    expect(terms.professionalUse).toBe(pro.professional_use);
    expect(`${terms.termsBefore} ${terms.termsLink} ${terms.termsAfter("1.0")}`).toBe(
      pro.terms.replace("{version}", "1.0"),
    );
  });
});

it("names the order button on Stripe's page in both languages and editions", () => {
  expect(checkoutCopy.en.next).toContain(
    '"Subscribe" button places an order with an obligation to pay',
  );
  expect(checkoutCopy.fr.next).toContain("« S'abonner » vaut commande avec obligation de paiement");
  expect(checkoutCopy.en.pro.next).toContain("excluding taxes");
  expect(checkoutCopy.fr.pro.next).toContain("hors taxes");
  expect(boxes.professional.version).not.toBe(boxes.full.version);
});
