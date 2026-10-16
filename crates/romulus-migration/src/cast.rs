//! How a column's values get from one type to another: `ALTER COLUMN …
//! TYPE` needs a `USING` expression unless an assignment cast exists, and a
//! reviewer needs to know whether values can be lost or rejected.
//!
//! Types are read from `format_type` text (`character varying(255)`,
//! `timestamp(3) with time zone`, `public.mood[]`), which is what the model
//! carries. Only built-in families are known; anything else converts
//! through text, which parses for every type (each has text I/O) and fails
//! only on values that do not read back.

use remus_core::emit::ident::quote_ident;

/// The built-in families whose casts are known.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Family {
    /// `smallint` 2, `integer` 4, `bigint` 8: the byte width.
    Int(u8),
    Numeric,
    /// `real` 4, `double precision` 8.
    Float(u8),
    Money,
    Text,
    Bool,
    Date,
    Timestamp,
    TimestampTz,
    Time,
    TimeTz,
    Interval,
    Json,
    Jsonb,
    Inet,
    Cidr,
    Bit,
    Enum,
    Other,
}

/// A type as `format_type` prints it, taken apart.
#[derive(Debug)]
struct Ty {
    /// Without modifiers, array brackets or schema: `character varying`.
    base: String,
    /// What was in parentheses: `255`, `10,2`, `Point,4326`.
    modifiers: Option<String>,
    arrays: usize,
    family: Family,
}

impl Ty {
    fn parse(full: &str, is_enum: bool) -> Self {
        let mut rest = full.trim();
        let mut arrays = 0;
        while let Some(inner) = rest.strip_suffix("[]") {
            arrays += 1;
            rest = inner.trim_end();
        }
        // `timestamp(3) with time zone`: modifiers can sit in the middle.
        let mut base = String::with_capacity(rest.len());
        let mut modifiers = None;
        let mut depth = 0;
        let mut current = String::new();
        for c in rest.chars() {
            match c {
                '(' => {
                    depth += 1;
                    if depth > 1 {
                        current.push(c);
                    }
                }
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        modifiers = Some(std::mem::take(&mut current));
                    } else {
                        current.push(c);
                    }
                }
                _ if depth > 0 => current.push(c),
                _ => base.push(c),
            }
        }
        let base = base.split_whitespace().collect::<Vec<_>>().join(" ");
        // `public.citext` reads as `citext`; a quoted name keeps its quotes.
        let unqualified = match base.rsplit_once('.') {
            Some((_, name)) if !base.starts_with('"') => name.to_owned(),
            _ => base.clone(),
        };
        let family = if is_enum {
            Family::Enum
        } else {
            family(&unqualified)
        };
        Self {
            base: unqualified,
            modifiers,
            arrays,
            family,
        }
    }

    fn bounded_text(&self) -> bool {
        self.family == Family::Text && self.modifiers.is_some()
    }
}

fn family(base: &str) -> Family {
    match base {
        "smallint" => Family::Int(2),
        "integer" => Family::Int(4),
        "bigint" => Family::Int(8),
        "numeric" => Family::Numeric,
        "real" => Family::Float(4),
        "double precision" => Family::Float(8),
        "money" => Family::Money,
        "text" | "character varying" | "character" | "citext" | "name" | "\"char\"" => Family::Text,
        "boolean" => Family::Bool,
        "date" => Family::Date,
        "timestamp without time zone" => Family::Timestamp,
        "timestamp with time zone" => Family::TimestampTz,
        "time without time zone" => Family::Time,
        "time with time zone" => Family::TimeTz,
        "json" => Family::Json,
        "jsonb" => Family::Jsonb,
        "inet" => Family::Inet,
        "cidr" => Family::Cidr,
        "bit" | "bit varying" => Family::Bit,
        _ if base.starts_with("interval") => Family::Interval,
        _ => Family::Other,
    }
}

/// How `ALTER COLUMN … TYPE` converts a column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Conversion {
    /// The `USING` expression, when an assignment cast does not do it.
    pub using: Option<String>,
    /// Every value converts, and converts back to the same value.
    pub lossless: bool,
    /// No cast between the two: through text.
    pub via_text: bool,
    pub note: Option<String>,
}

/// Converting `column` from `from` to `to` (both `format_type` text; enum
/// flags say whether either is an enum, which `format_type` does not show).
pub(crate) fn conversion(
    column: &str,
    from: &str,
    from_enum: bool,
    to: &str,
    to_enum: bool,
) -> Conversion {
    let a = Ty::parse(from, from_enum);
    let b = Ty::parse(to, to_enum);
    let col = quote_ident(column);
    if a.arrays == b.arrays && widening(&a, &b) {
        return Conversion {
            using: None,
            lossless: true,
            via_text: false,
            note: None,
        };
    }
    if a.arrays != b.arrays {
        return Conversion {
            using: Some(format!("{col}::text::{to}")),
            lossless: false,
            via_text: true,
            note: Some(format!(
                "Converts {from} to {to} through text: fails unless every value reads as {to}. Convert it yourself if it holds data."
            )),
        };
    }
    if a.arrays == b.arrays && (a.family, b.family) == (Family::Timestamp, Family::TimestampTz) {
        return Conversion {
            using: None,
            lossless: true,
            via_text: false,
            note: Some(
                "Reads the existing values in the session's TimeZone: set it to the one they were written in."
                    .to_owned(),
            ),
        };
    }
    if cast_exists(&a, &b) {
        return Conversion {
            using: Some(format!("{col}::{to}")),
            lossless: false,
            via_text: false,
            note: Some(format!(
                "Converts {from} to {to}: fails on a value that does not convert, and can round or cut values."
            )),
        };
    }
    let text = if a.arrays > 0 {
        format!("text{}", "[]".repeat(a.arrays))
    } else {
        "text".to_owned()
    };
    Conversion {
        using: Some(format!("{col}::{text}::{to}")),
        lossless: false,
        via_text: true,
        note: Some(format!(
            "No cast from {from} to {to}: converted through text, which fails unless every value reads as {to}."
        )),
    }
}

/// Every value of `a` is a value of `b`, and an assignment cast does it.
fn widening(a: &Ty, b: &Ty) -> bool {
    use Family as F;
    let unbounded_text =
        b.family == F::Text && !b.bounded_text() && b.base != "character" && b.base != "\"char\"";
    match (a.family, b.family) {
        (F::Int(x), F::Int(y)) => x <= y,
        (F::Int(_), F::Numeric) => b.modifiers.is_none(),
        (F::Int(x), F::Float(8)) => x <= 4,
        (F::Numeric, F::Numeric) => numeric_widens(a.modifiers.as_deref(), b.modifiers.as_deref()),
        (F::Text, F::Text)
            if !unbounded_text && (a.base == b.base || b.base == "character varying") =>
        {
            length_widens(a.modifiers.as_deref(), b.modifiers.as_deref())
        }
        // To unbounded text: the text output of anything.
        (from, to) => {
            unbounded_text
                || matches!(
                    (from, to),
                    (F::Int(2), F::Float(4))
                        | (F::Float(4), F::Float(8))
                        | (F::Date, F::Timestamp | F::TimestampTz)
                        | (F::Time, F::Interval)
                        | (F::Cidr, F::Inet)
                        | (F::Jsonb, F::Json)
                )
                // Same type, its modifier dropped (`geometry(Point,4326)` → `geometry`).
                || (a.base == b.base && b.modifiers.is_none() && a.family != F::Enum)
        }
    }
}

/// `numeric(p,s)` → `numeric(p2,s2)` keeps every value.
fn numeric_widens(from: Option<&str>, to: Option<&str>) -> bool {
    let Some(to) = to else { return true };
    let Some(from) = from else { return false };
    let parse = |m: &str| -> Option<(i64, i64)> {
        let mut parts = m.split(',').map(|p| p.trim().parse::<i64>());
        let p = parts.next()?.ok()?;
        let s = parts.next().transpose().ok()?.unwrap_or(0);
        Some((p, s))
    };
    match (parse(from), parse(to)) {
        (Some((p, s)), Some((p2, s2))) => p2 - s2 >= p - s && s2 >= s,
        _ => false,
    }
}

/// `varchar(n)` → `varchar(m)` with `m ≥ n`, or no limit.
fn length_widens(from: Option<&str>, to: Option<&str>) -> bool {
    let Some(to) = to else { return true };
    let Some(from) = from else { return false };
    match (from.trim().parse::<u64>(), to.trim().parse::<u64>()) {
        (Ok(n), Ok(m)) => m >= n,
        _ => false,
    }
}

/// An explicit cast exists between the two (built-in casts only).
fn cast_exists(a: &Ty, b: &Ty) -> bool {
    use Family as F;
    let numeric = |f: F| matches!(f, F::Int(_) | F::Numeric | F::Float(_));
    if a.base == b.base && a.family != F::Enum {
        return true;
    }
    let (x, y) = (a.family, b.family);
    (numeric(x) && numeric(y))
        || matches!(
            (x, y),
            (F::Int(_) | F::Numeric, F::Money)
                | (F::Money, F::Numeric)
                | (F::Int(4), F::Bool)
                | (F::Bool, F::Int(4))
                | (F::Int(4 | 8), F::Bit)
                | (F::Bit, F::Int(4 | 8))
                // Text reads as anything; anything writes as text.
                | (F::Text, _)
                | (_, F::Text)
                | (
                    F::Date | F::Timestamp | F::TimestampTz,
                    F::Date | F::Timestamp | F::TimestampTz
                )
                | (F::Timestamp | F::TimestampTz | F::TimeTz | F::Interval, F::Time)
                | (F::TimestampTz | F::Time, F::TimeTz)
                | (F::Time, F::Interval)
                | (F::Json, F::Jsonb)
                | (F::Inet, F::Cidr)
        )
}

#[cfg(test)]
mod tests {
    use super::{Family, Ty, conversion};

    fn using(from: &str, to: &str) -> Option<String> {
        conversion("c", from, false, to, false).using
    }

    #[test]
    fn types_are_taken_apart() {
        let t = Ty::parse("timestamp(3) with time zone", false);
        assert_eq!(t.base, "timestamp with time zone");
        assert_eq!(t.modifiers.as_deref(), Some("3"));
        assert_eq!(t.family, Family::TimestampTz);
        let t = Ty::parse("character varying(20)[]", false);
        assert_eq!((t.base.as_str(), t.arrays), ("character varying", 1));
        assert_eq!(Ty::parse("public.citext", false).family, Family::Text);
        assert_eq!(
            Ty::parse("geometry(Point,4326)", false)
                .modifiers
                .as_deref(),
            Some("Point,4326")
        );
    }

    #[test]
    fn widening_needs_no_using_and_loses_nothing() {
        for (from, to) in [
            ("integer", "bigint"),
            ("smallint", "integer"),
            ("character varying(20)", "character varying(40)"),
            ("character varying(20)", "text"),
            ("text", "character varying"),
            ("numeric(10,2)", "numeric(12,2)"),
            ("numeric(10,2)", "numeric"),
            ("integer", "text"),
            ("uuid", "text"),
            ("date", "timestamp with time zone"),
            ("geometry(Point,4326)", "geometry"),
            ("integer[]", "bigint[]"),
        ] {
            let c = conversion("c", from, false, to, false);
            assert!(c.lossless && c.using.is_none(), "{from} → {to}: {c:?}");
        }
    }

    #[test]
    fn narrowing_and_crossing_cast_explicitly() {
        assert_eq!(using("bigint", "integer").as_deref(), Some("c::integer"));
        assert_eq!(using("text", "integer").as_deref(), Some("c::integer"));
        assert_eq!(
            using("character varying(40)", "character varying(20)").as_deref(),
            Some("c::character varying(20)")
        );
        assert_eq!(using("json", "jsonb").as_deref(), Some("c::jsonb"));
        assert!(!conversion("c", "bigint", false, "integer", false).lossless);
    }

    #[test]
    fn unrelated_types_go_through_text() {
        let c = conversion("c", "uuid", false, "bigint", false);
        assert_eq!(c.using.as_deref(), Some("c::text::bigint"));
        assert!(c.via_text);
        let c = conversion("c", "public.mood", true, "public.state", true);
        assert_eq!(c.using.as_deref(), Some("c::text::public.state"));
        let c = conversion("c", "public.mood[]", true, "public.state[]", true);
        assert_eq!(c.using.as_deref(), Some("c::text[]::public.state[]"));
        let c = conversion("Mixed", "text", false, "public.mood", true);
        assert_eq!(c.using.as_deref(), Some("\"Mixed\"::public.mood"));
    }
}
