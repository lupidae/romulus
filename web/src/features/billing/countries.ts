import type { ComboboxOption } from "@/components/ui/combobox";
import { intlLocale, type Locale } from "@/features/legal/locale";

// Every ISO 3166-1 alpha-2 code: a business's country at checkout (the API
// takes any two capital letters; the names are the browser's own).
const CODES =
  "AD AE AF AG AI AL AM AO AQ AR AS AT AU AW AX AZ BA BB BD BE BF BG BH BI BJ BL BM BN BO BQ BR BS BT BV BW BY BZ CA CC CD CF CG CH CI CK CL CM CN CO CR CU CV CW CX CY CZ DE DJ DK DM DO DZ EC EE EG EH ER ES ET FI FJ FK FM FO FR GA GB GD GE GF GG GH GI GL GM GN GP GQ GR GS GT GU GW GY HK HM HN HR HT HU ID IE IL IM IN IO IQ IR IS IT JE JM JO JP KE KG KH KI KM KN KP KR KW KY KZ LA LB LC LI LK LR LS LT LU LV LY MA MC MD ME MF MG MH MK ML MM MN MO MP MQ MR MS MT MU MV MW MX MY MZ NA NC NE NF NG NI NL NO NP NR NU NZ OM PA PE PF PG PH PK PL PM PN PR PS PT PW PY QA RE RO RS RU RW SA SB SC SD SE SG SH SI SJ SK SL SM SN SO SR SS ST SV SX SY SZ TC TD TF TG TH TJ TK TL TM TN TO TR TT TV TW TZ UA UG UM US UY UZ VA VC VE VG VI VN VU WF WS YE YT ZA ZM ZW".split(
    " ",
  );

/** Every country, by name in `locale`, alphabetical. */
export function countries(locale: Locale): ComboboxOption[] {
  const names = new Intl.DisplayNames([intlLocale(locale)], { type: "region" });
  const collator = new Intl.Collator(intlLocale(locale));
  return CODES.map((code) => ({ value: code, label: names.of(code) ?? code })).sort((a, b) =>
    collator.compare(a.label, b.label),
  );
}

/** Folded for matching: "cote d'ivoire" finds "Côte d’Ivoire". */
function fold(text: string): string {
  return text
    .normalize("NFD")
    .replace(/\p{Diacritic}/gu, "")
    .replace(/[’']/g, "'")
    .toLowerCase()
    .trim();
}

/** The countries whose name (or code) starts a word with `query`; the first 8. */
export function matchCountries(options: ComboboxOption[], query: string): ComboboxOption[] {
  const q = fold(query);
  if (!q) return options.slice(0, 8);
  return options
    .filter(
      (o) =>
        o.value.toLowerCase() === q ||
        fold(o.label)
          .split(/[\s'-]+/)
          .some((word) => word.startsWith(q)) ||
        fold(o.label).startsWith(q),
    )
    .slice(0, 8);
}
