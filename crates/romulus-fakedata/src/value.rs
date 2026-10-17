//! Generated values, and their text form: what `COPY … (FORMAT text)` and
//! the type's input function read.

use std::{cmp::Ordering, fmt::Write as _};

/// Microseconds in a day.
pub const DAY: i64 = 86_400_000_000;

#[derive(Debug, Clone, PartialEq)]
pub enum V {
    Null,
    Bool(bool),
    Int(i128),
    Float(f64),
    /// `units × 10^-scale`.
    Dec {
        units: i128,
        scale: u32,
    },
    Text(String),
    /// Days since 1970-01-01.
    Date(i64),
    /// Microseconds since 1970-01-01 00:00 UTC.
    Ts(i64),
    /// Microseconds since midnight.
    Time(i64),
    Interval {
        days: i64,
        micros: i64,
    },
    Uuid(u128),
    Bytes(Vec<u8>),
    Array(Vec<V>),
    Record(Vec<V>),
    /// Already in its text form (geometry, vectors, ranges, inet…).
    Raw(String),
}

impl V {
    pub fn is_null(&self) -> bool {
        matches!(self, V::Null)
    }

    /// As a number, when it is one.
    pub fn as_f64(&self) -> Option<f64> {
        #[allow(clippy::cast_precision_loss)]
        match self {
            V::Int(i) => Some(*i as f64),
            V::Float(f) => Some(*f),
            V::Dec { units, scale } => {
                Some(*units as f64 / crate::rng::pow10(i32::try_from(*scale).ok()?))
            }
            V::Date(d) => Some(*d as f64),
            V::Ts(t) => Some(*t as f64),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            V::Text(s) | V::Raw(s) => Some(s),
            _ => None,
        }
    }

    /// Compares two values of comparable kinds.
    pub fn compare(&self, other: &V) -> Option<Ordering> {
        match (self, other) {
            (V::Null, _) | (_, V::Null) => None,
            (V::Bool(a), V::Bool(b)) => Some(a.cmp(b)),
            (V::Int(a), V::Int(b)) => Some(a.cmp(b)),
            (V::Date(a), V::Date(b)) | (V::Ts(a), V::Ts(b)) | (V::Time(a), V::Time(b)) => {
                Some(a.cmp(b))
            }
            (V::Date(a), V::Ts(b)) => Some(a.saturating_mul(DAY).cmp(b)),
            (V::Ts(a), V::Date(b)) => Some(a.cmp(&b.saturating_mul(DAY))),
            (
                V::Dec {
                    units: a,
                    scale: sa,
                },
                V::Dec {
                    units: b,
                    scale: sb,
                },
            ) => {
                let s = (*sa).max(*sb);
                let a = a.checked_mul(10i128.checked_pow(s - sa)?)?;
                let b = b.checked_mul(10i128.checked_pow(s - sb)?)?;
                Some(a.cmp(&b))
            }
            (V::Dec { units, scale }, V::Int(i)) => {
                Some(units.cmp(&i.checked_mul(10i128.checked_pow(*scale)?)?))
            }
            (V::Int(i), V::Dec { units, scale }) => {
                Some(i.checked_mul(10i128.checked_pow(*scale)?)?.cmp(units))
            }
            (V::Text(a), V::Text(b)) => Some(a.cmp(b)),
            (V::Uuid(a), V::Uuid(b)) => Some(a.cmp(b)),
            (
                V::Interval {
                    days: d1,
                    micros: m1,
                },
                V::Interval {
                    days: d2,
                    micros: m2,
                },
            ) => Some((d1 * DAY + m1).cmp(&(d2 * DAY + m2))),
            // Postgres orders NaN above every number, and equal to itself.
            (a, b) => {
                let (x, y) = (a.as_f64()?, b.as_f64()?);
                match (x.is_nan(), y.is_nan()) {
                    (true, true) => Some(Ordering::Equal),
                    (true, false) => Some(Ordering::Greater),
                    (false, true) => Some(Ordering::Less),
                    (false, false) => x.partial_cmp(&y),
                }
            }
        }
    }

    /// The value in Postgres's text form; `None` for NULL.
    pub fn text(&self, tz: bool) -> Option<String> {
        let mut out = String::new();
        self.write(&mut out, tz).then_some(out)
    }

    /// Writes the text form; false for NULL.
    fn write(&self, out: &mut String, tz: bool) -> bool {
        match self {
            V::Null => return false,
            V::Bool(b) => out.push(if *b { 't' } else { 'f' }),
            V::Int(i) => {
                let _ = write!(out, "{i}");
            }
            V::Float(f) => {
                if f.is_nan() {
                    out.push_str("NaN");
                } else if f.is_infinite() {
                    out.push_str(if *f > 0.0 { "Infinity" } else { "-Infinity" });
                } else {
                    let _ = write!(out, "{f}");
                }
            }
            V::Dec { units, scale } => write_decimal(out, *units, *scale),
            V::Text(s) | V::Raw(s) => out.push_str(s),
            V::Date(d) => write_date(out, *d),
            V::Ts(t) => write_timestamp(out, *t, tz),
            V::Time(t) => {
                write_time(out, t.rem_euclid(DAY));
                if tz {
                    out.push_str("+00");
                }
            }
            V::Interval { days, micros } => {
                let _ = write!(out, "{days} days");
                if *micros != 0 {
                    out.push(' ');
                    write_time(out, micros.rem_euclid(DAY));
                }
            }
            V::Uuid(u) => {
                let h = format!("{u:032x}");
                let _ = write!(
                    out,
                    "{}-{}-{}-{}-{}",
                    &h[0..8],
                    &h[8..12],
                    &h[12..16],
                    &h[16..20],
                    &h[20..32]
                );
            }
            V::Bytes(b) => {
                out.push_str("\\x");
                for byte in b {
                    let _ = write!(out, "{byte:02x}");
                }
            }
            V::Array(items) => {
                out.push('{');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    match item.text(tz) {
                        None => out.push_str("NULL"),
                        Some(s) => array_element(out, &s),
                    }
                }
                out.push('}');
            }
            V::Record(fields) => {
                out.push('(');
                for (i, field) in fields.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    if let Some(s) = field.text(tz) {
                        record_field(out, &s);
                    }
                }
                out.push(')');
            }
        }
        true
    }
}

fn write_decimal(out: &mut String, units: i128, scale: u32) {
    if scale == 0 {
        let _ = write!(out, "{units}");
        return;
    }
    let negative = units < 0;
    let digits = units.unsigned_abs().to_string();
    let scale = scale as usize;
    let digits = if digits.len() <= scale {
        format!("{}{digits}", "0".repeat(scale + 1 - digits.len()))
    } else {
        digits
    };
    if negative {
        out.push('-');
    }
    let (int, frac) = digits.split_at(digits.len() - scale);
    out.push_str(int);
    out.push('.');
    out.push_str(frac);
}

/// (year, month, day) of a day count since 1970-01-01 (Hinnant's algorithm).
pub fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    (if m <= 2 { y + 1 } else { y }, m as u32, d as u32)
}

/// The day count of a civil date.
pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let m = i64::from(m);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn write_date(out: &mut String, days: i64) {
    if days == i64::MAX || days == i64::MIN {
        out.push_str(if days == i64::MAX {
            "infinity"
        } else {
            "-infinity"
        });
        return;
    }
    let (y, m, d) = civil(days);
    if y <= 0 {
        let _ = write!(out, "{:04}-{m:02}-{d:02} BC", 1 - y);
    } else {
        let _ = write!(out, "{y:04}-{m:02}-{d:02}");
    }
}

fn write_time(out: &mut String, micros: i64) {
    let secs = micros / 1_000_000;
    let frac = micros % 1_000_000;
    let _ = write!(
        out,
        "{:02}:{:02}:{:02}",
        secs / 3600,
        (secs / 60) % 60,
        secs % 60
    );
    if frac != 0 {
        let _ = write!(out, ".{frac:06}");
    }
}

fn write_timestamp(out: &mut String, micros: i64, tz: bool) {
    if micros == i64::MAX || micros == i64::MIN {
        out.push_str(if micros == i64::MAX {
            "infinity"
        } else {
            "-infinity"
        });
        return;
    }
    let days = micros.div_euclid(DAY);
    let time = micros.rem_euclid(DAY);
    let (y, m, d) = civil(days);
    let _ = write!(out, "{y:04}-{m:02}-{d:02} ");
    write_time(out, time);
    if tz {
        out.push_str("+00");
    }
}

/// An array element, double-quoted when the array syntax needs it.
fn array_element(out: &mut String, s: &str) {
    let needs = s.is_empty()
        || s.eq_ignore_ascii_case("null")
        || s.chars()
            .any(|c| matches!(c, '{' | '}' | ',' | '"' | '\\') || c.is_whitespace());
    if !needs {
        out.push_str(s);
        return;
    }
    out.push('"');
    for c in s.chars() {
        if c == '"' || c == '\\' {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
}

/// A composite field, double-quoted when the record syntax needs it (an
/// empty field is NULL, so an empty string is `""`).
fn record_field(out: &mut String, s: &str) {
    let needs = s.is_empty()
        || s.chars()
            .any(|c| matches!(c, '(' | ')' | ',' | '"' | '\\') || c.is_whitespace());
    if !needs {
        out.push_str(s);
        return;
    }
    out.push('"');
    for c in s.chars() {
        if c == '"' || c == '\\' {
            out.push(c);
        }
        out.push(c);
    }
    out.push('"');
}

/// Parses a date (`2026-01-01`) into days.
pub fn parse_date(s: &str) -> Option<i64> {
    let s = s.trim();
    match s {
        "infinity" => return Some(i64::MAX),
        "-infinity" => return Some(i64::MIN),
        _ => {}
    }
    let mut parts = s.splitn(3, '-');
    let y: i64 = parts.next()?.parse().ok()?;
    let m: u32 = parts.next()?.parse().ok()?;
    let d: u32 = parts.next()?.get(..2)?.parse().ok()?;
    ((1..=12).contains(&m) && (1..=31).contains(&d)).then(|| days_from_civil(y, m, d))
}

/// Parses a timestamp as Postgres prints one (`2026-09-01 00:00:00+00`,
/// fractions and offsets optional; no offset reads as UTC).
pub fn parse_timestamp(s: &str) -> Option<i64> {
    let s = s.trim();
    let (date, rest) = match s.split_once([' ', 'T']) {
        Some((d, r)) => (d, r.trim()),
        None => (s, ""),
    };
    let days = parse_date(date)?;
    if days == i64::MAX || days == i64::MIN {
        return Some(days);
    }
    if rest.is_empty() {
        return Some(days * DAY);
    }
    // The offset: the last + or - in the time part.
    let (time, offset) = match rest.rfind(['+', '-']) {
        Some(i) => (&rest[..i], Some(&rest[i..])),
        None => (rest, None),
    };
    let mut hms = time.split(':');
    let h: i64 = hms.next()?.trim().parse().ok()?;
    let mi: i64 = hms.next().unwrap_or("0").parse().ok()?;
    let sec: f64 = hms.next().unwrap_or("0").parse().ok()?;
    #[allow(clippy::cast_possible_truncation)]
    let mut micros = days * DAY + (h * 3600 + mi * 60) * 1_000_000 + (sec * 1e6).round() as i64;
    if let Some(offset) = offset {
        let sign = if offset.starts_with('-') { -1 } else { 1 };
        let mut parts = offset[1..].split(':');
        let oh: i64 = parts.next()?.parse().ok()?;
        let om: i64 = parts.next().unwrap_or("0").parse().ok()?;
        micros -= sign * (oh * 3600 + om * 60) * 1_000_000;
    }
    Some(micros)
}

/// Parses a decimal literal (`-12.50`) into units at `scale`.
pub fn parse_decimal(s: &str, scale: u32) -> Option<i128> {
    let s = s.trim();
    let (negative, s) = match s.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, s.strip_prefix('+').unwrap_or(s)),
    };
    let (int, frac) = s.split_once('.').unwrap_or((s, ""));
    if int.is_empty() && frac.is_empty() {
        return None;
    }
    let mut units: i128 = if int.is_empty() { 0 } else { int.parse().ok()? };
    let scale = scale as usize;
    for i in 0..scale {
        let digit = frac.as_bytes().get(i).map_or(0, |b| i128::from(b - b'0'));
        units = units.checked_mul(10)?.checked_add(digit)?;
    }
    Some(if negative { -units } else { units })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_round_trip() {
        for days in [-800_000, -1, 0, 1, 19_000, 20_454, 2_000_000] {
            let (y, m, d) = civil(days);
            assert_eq!(days_from_civil(y, m, d), days);
        }
        assert_eq!(V::Date(0).text(false).unwrap(), "1970-01-01");
        assert_eq!(
            parse_date("2026-10-17"),
            Some(days_from_civil(2026, 10, 17))
        );
    }

    #[test]
    fn timestamps_print_and_parse() {
        let t = parse_timestamp("2026-09-01 00:00:00+00").unwrap();
        assert_eq!(V::Ts(t).text(true).unwrap(), "2026-09-01 00:00:00+00");
        assert_eq!(parse_timestamp("2026-09-01 02:00:00+02"), Some(t));
        assert_eq!(parse_timestamp("2026-09-01"), Some(t));
        let t = parse_timestamp("2026-09-01 10:11:12.5").unwrap();
        assert_eq!(V::Ts(t).text(false).unwrap(), "2026-09-01 10:11:12.500000");
    }

    #[test]
    fn decimals_print_at_their_scale() {
        let v = V::Dec {
            units: 1205,
            scale: 2,
        };
        assert_eq!(v.text(false).unwrap(), "12.05");
        assert_eq!(
            V::Dec {
                units: -5,
                scale: 2
            }
            .text(false)
            .unwrap(),
            "-0.05"
        );
        assert_eq!(parse_decimal("-12.5", 2), Some(-1250));
        assert_eq!(parse_decimal("0.2", 4), Some(2000));
    }

    #[test]
    fn arrays_and_records_quote_what_needs_it() {
        let a = V::Array(vec![
            V::Text("plain".into()),
            V::Text("with space".into()),
            V::Text("q\"uote".into()),
            V::Null,
            V::Text(String::new()),
        ]);
        assert_eq!(
            a.text(false).unwrap(),
            r#"{plain,"with space","q\"uote",NULL,""}"#
        );
        let r = V::Record(vec![
            V::Text("1 Main St".into()),
            V::Null,
            V::Text("FR".into()),
        ]);
        assert_eq!(r.text(false).unwrap(), r#"("1 Main St",,FR)"#);
    }

    #[test]
    fn values_compare_across_numeric_kinds() {
        assert_eq!(
            V::Dec {
                units: 150,
                scale: 2
            }
            .compare(&V::Int(1)),
            Some(Ordering::Greater)
        );
        assert_eq!(V::Int(1).compare(&V::Float(1.5)), Some(Ordering::Less));
        assert_eq!(V::Null.compare(&V::Int(1)), None);
        assert_eq!(
            V::Float(f64::NAN).compare(&V::Int(1000)),
            Some(Ordering::Greater)
        );
    }
}
