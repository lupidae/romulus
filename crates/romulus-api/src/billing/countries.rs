//! The countries a business may name at checkout: every ISO 3166-1 alpha-2
//! code, as the web's list has them (`web/src/features/billing/countries.ts`;
//! a test keeps the two the same).

/// Every code, space-separated, in alphabetical order.
const CODES: &str = "AD AE AF AG AI AL AM AO AQ AR AS AT AU AW AX AZ BA BB BD BE BF BG BH BI BJ BL BM BN BO BQ BR BS BT BV BW BY BZ CA CC CD CF CG CH CI CK CL CM CN CO CR CU CV CW CX CY CZ DE DJ DK DM DO DZ EC EE EG EH ER ES ET FI FJ FK FM FO FR GA GB GD GE GF GG GH GI GL GM GN GP GQ GR GS GT GU GW GY HK HM HN HR HT HU ID IE IL IM IN IO IQ IR IS IT JE JM JO JP KE KG KH KI KM KN KP KR KW KY KZ LA LB LC LI LK LR LS LT LU LV LY MA MC MD ME MF MG MH MK ML MM MN MO MP MQ MR MS MT MU MV MW MX MY MZ NA NC NE NF NG NI NL NO NP NR NU NZ OM PA PE PF PG PH PK PL PM PN PR PS PT PW PY QA RE RO RS RU RW SA SB SC SD SE SG SH SI SJ SK SL SM SN SO SR SS ST SV SX SY SZ TC TD TF TG TH TJ TK TL TM TN TO TR TT TV TW TZ UA UG UM US UY UZ VA VC VE VG VI VN VU WF WS YE YT ZA ZM ZW";

/// `code` (two capital letters) is a country of ISO 3166-1.
pub fn is_country(code: &str) -> bool {
    code.len() == 2 && CODES.split(' ').any(|c| c == code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_codes_are_the_web_lists_and_iso_3166_1s() {
        let web = include_str!("../../../../web/src/features/billing/countries.ts");
        assert!(
            web.contains(&format!("\"{CODES}\"")),
            "countries.ts lists other codes"
        );
        assert_eq!(CODES.split(' ').count(), 249);
        assert!(is_country("FR") && is_country("DE") && is_country("US"));
        assert!(!is_country("XX") && !is_country("UK") && !is_country("EU") && !is_country("fr"));
    }
}
