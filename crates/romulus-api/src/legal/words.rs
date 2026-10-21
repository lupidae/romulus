//! Dates and amounts as each language writes them, for the contract and
//! billing emails: `March 1, 2027` and `€19.00`; `1er mars 2027` and
//! `19,00 €`.

use chrono::{DateTime, Datelike, NaiveDate, Utc};

use crate::domain::Locale;

const MONTHS_FR: [&str; 12] = [
    "janvier",
    "février",
    "mars",
    "avril",
    "mai",
    "juin",
    "juillet",
    "août",
    "septembre",
    "octobre",
    "novembre",
    "décembre",
];

/// A day (UTC).
pub fn date(locale: Locale, at: DateTime<Utc>) -> String {
    day(locale, at.date_naive())
}

pub fn day(locale: Locale, day: NaiveDate) -> String {
    match locale {
        Locale::En => day.format("%B %-d, %Y").to_string(),
        Locale::Fr => {
            let number = match day.day() {
                1 => "1er".to_owned(),
                n => n.to_string(),
            };
            format!("{number} {} {}", month(day), day.year())
        }
    }
}

/// A day without its year: `March 1`, `1er mars`.
pub fn day_of_year(locale: Locale, day: NaiveDate) -> String {
    match locale {
        Locale::En => day.format("%B %-d").to_string(),
        Locale::Fr => match day.day() {
            1 => format!("1er {}", month(day)),
            n => format!("{n} {}", month(day)),
        },
    }
}

/// The month's name: `March`, `mars`.
pub fn month_name(locale: Locale, day: NaiveDate) -> String {
    match locale {
        Locale::En => day.format("%B").to_string(),
        Locale::Fr => month(day).to_owned(),
    }
}

fn month(day: NaiveDate) -> &'static str {
    MONTHS_FR[day.month0() as usize]
}

/// A moment, to the minute, in UTC: what an acknowledgement is stamped
/// with.
pub fn timestamp(locale: Locale, at: DateTime<Utc>) -> String {
    match locale {
        Locale::En => format!("{} at {} UTC", date(locale, at), at.format("%H:%M:%S")),
        Locale::Fr => format!("{} à {} UTC", date(locale, at), at.format("%H:%M:%S")),
    }
}

/// Money: `€19.00`, `19,00 €`.
pub fn money(locale: Locale, cents: i64, currency: &str) -> String {
    match locale {
        Locale::En => crate::billing::money(cents, currency),
        Locale::Fr => {
            let symbol = match currency.to_ascii_lowercase().as_str() {
                "eur" => "€".to_owned(),
                "usd" => "$".to_owned(),
                other => other.to_ascii_uppercase(),
            };
            let sign = if cents < 0 { "-" } else { "" };
            let cents = cents.unsigned_abs();
            format!("{sign}{},{:02}\u{a0}{symbol}", cents / 100, cents % 100)
        }
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    #[test]
    fn dates_and_amounts_read_as_each_language_writes_them() {
        let at = Utc.with_ymd_and_hms(2027, 3, 1, 9, 5, 7).unwrap();
        assert_eq!(date(Locale::En, at), "March 1, 2027");
        assert_eq!(date(Locale::Fr, at), "1er mars 2027");
        let later = Utc.with_ymd_and_hms(2026, 8, 14, 0, 0, 0).unwrap();
        assert_eq!(date(Locale::Fr, later), "14 août 2026");
        assert_eq!(day_of_year(Locale::Fr, later.date_naive()), "14 août");
        assert_eq!(timestamp(Locale::En, at), "March 1, 2027 at 09:05:07 UTC");
        assert_eq!(timestamp(Locale::Fr, at), "1er mars 2027 à 09:05:07 UTC");
        assert_eq!(money(Locale::En, 1_900, "eur"), "€19.00");
        assert_eq!(money(Locale::Fr, 149_000, "eur"), "1490,00\u{a0}€");
        assert_eq!(money(Locale::Fr, -61, "EUR"), "-0,61\u{a0}€");
    }
}
