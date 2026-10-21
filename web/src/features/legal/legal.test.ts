import { browserLocale } from "./locale";
import { needsTerms, postponeTerms, type TermsStatus } from "./queries";
import { NO_TICKS, termsTicked } from "./ticks";

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

it("asks the professional edition for both boxes, the full one for the Terms alone", () => {
  const pro = status({}).documents!;
  const full = { ...pro, edition: "full" as const, consumer_sales: true };
  expect(termsTicked(pro, NO_TICKS)).toBe(false);
  expect(termsTicked(pro, { terms: true, professional: false })).toBe(false);
  expect(termsTicked(pro, { terms: true, professional: true })).toBe(true);
  expect(termsTicked(full, { terms: true, professional: false })).toBe(true);
  expect(termsTicked(full, { terms: false, professional: true })).toBe(false);
});
