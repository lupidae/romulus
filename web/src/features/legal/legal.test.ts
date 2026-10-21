import { browserLocale } from "./locale";
import { needsTerms, postponeTerms, type TermsStatus } from "./queries";

const status = (over: Partial<TermsStatus>): TermsStatus => ({
  required: true,
  accepted: false,
  accepted_version: null,
  accepted_at: null,
  locale: null,
  documents: {
    terms_version: "1.1",
    edition: "professional",
    consumer_sales: false,
    copy_version: "v",
    contact: "legal@romulus.example",
    en: { terms: "t", privacy: "p", dpa: "d" },
    fr: { terms: "t", privacy: "p", dpa: "d" },
  },
  ...over,
});

describe("the Terms before anything else", () => {
  afterEach(() => sessionStorage.clear());

  it("are asked of whoever has not accepted those in force, never on a server without them", () => {
    expect(needsTerms(status({}))).toBe(true);
    expect(needsTerms(status({ accepted: true }))).toBe(false);
    expect(needsTerms(status({ required: false }))).toBe(false);
  });

  it("can wait for the session only after an earlier version was accepted", () => {
    postponeTerms("1.1");
    expect(needsTerms(status({}))).toBe(true);
    expect(needsTerms(status({ accepted_version: "1.0" }))).toBe(false);
    sessionStorage.clear();
    expect(needsTerms(status({ accepted_version: "1.0" }))).toBe(true);
  });
});

describe("browserLocale", () => {
  it("takes the first language romulus speaks", () => {
    expect(browserLocale(["fr-FR", "en"])).toBe("fr");
    expect(browserLocale(["de-DE", "fr"])).toBe("fr");
    expect(browserLocale(["en-GB", "fr"])).toBe("en");
    expect(browserLocale(["de"])).toBe("en");
    expect(browserLocale([])).toBe("en");
  });
});
