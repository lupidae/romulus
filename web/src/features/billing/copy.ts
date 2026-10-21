// The words the checkout dialog asks people to agree to, in one place so the
// legal wording can change without touching the components. DRAFT: awaiting
// counsel (Terms sections 7 to 9, French Consumer Code L221-18, L221-25 and
// L221-28). What a box says is what the audit log records as accepted: the
// API keeps who, when and which Terms version (`terms_accepted`).

export const checkoutCopy = {
  buyerLegend: "Buying as",
  business: {
    label: "A business",
    description: "A company or a professional. Add your business name and VAT number at checkout.",
  },
  consumer: {
    label: "An individual",
    description: "For personal use, as a consumer.",
  },
  /** Before the link to the Terms. */
  termsBefore: "I accept the",
  termsLink: "Terms of Service",
  termsMissing: "Accept the Terms of Service to continue.",
  /** The consumer's express request that the service start at once (L221-25). */
  immediateStart: "I ask for my plan to start now, before the end of the 14-day withdrawal period.",
  /** The acknowledgement of what that request costs (L221-28 1°, L221-25). */
  withdrawal:
    "I understand that if I withdraw within those 14 days, I pay for the service provided until then. Once the service has been fully provided, I can no longer withdraw.",
  consumerMissing: "Tick both to start your plan now.",
  /** Under the boxes: what happens next, and the price's tax status. */
  next: "You enter your payment details on Stripe's secure page. Prices are in euros, VAT not applicable (French tax code, art. 293 B du CGI).",
  commitment: (interval: "month" | "year") =>
    `renewed every ${interval} until you cancel; cancelling keeps the plan until the period ends.`,
} as const;

/** What a checkout may be sent with: the Terms, and a consumer's two requests. */
export function checkoutProblems(form: {
  acceptTerms: boolean;
  consumer: boolean;
  immediateStart: boolean;
  withdrawal: boolean;
}): { terms?: string; consumer?: string } {
  return {
    terms: form.acceptTerms ? undefined : checkoutCopy.termsMissing,
    consumer:
      form.consumer && !(form.immediateStart && form.withdrawal)
        ? checkoutCopy.consumerMissing
        : undefined,
  };
}
