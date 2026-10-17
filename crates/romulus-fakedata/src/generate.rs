//! One column's value: by heuristic or by type, within its bounds, made
//! unique by construction when a key needs it.

use rand::RngExt;
use rand_chacha::ChaCha8Rng;

use crate::{
    Profile,
    checks::Bounds,
    heuristics::{self, Heuristic},
    rng::{normal, pick, skewed},
    types::{Base, RangeKind, Type, VectorKind},
    value::{DAY, V},
    words,
};

/// What one column's values must be.
pub struct Spec<'a> {
    pub table: &'a str,
    pub ty: &'a Type,
    pub bounds: &'a Bounds,
    pub heuristic: Option<Heuristic>,
    pub profile: Profile,
    /// Distinct from every other row's (keys, unique columns).
    pub unique: bool,
    /// Ranges that must not overlap from row to row (an exclusion
    /// constraint with `&&`).
    pub disjoint: bool,
    /// Integer amounts in cents (`cents`, `amount_cents`).
    pub cents: bool,
    pub row: u64,
    pub rows: u64,
    pub anchor: i64,
}

/// The column's value for this row.
pub fn value(spec: &Spec, rng: &mut ChaCha8Rng) -> V {
    let mut v = candidate(spec, rng);
    if (spec.unique || spec.disjoint)
        && let Some(u) = unique(spec, v.clone(), rng)
    {
        v = u;
    }
    v
}

#[allow(clippy::too_many_lines)]
fn candidate(spec: &Spec, rng: &mut ChaCha8Rng) -> V {
    let b = spec.bounds;
    if spec.ty.array {
        // Bounds are the elements': `typed` draws each one as a value.
        if let Some(h) = spec.heuristic {
            let cx = heuristics::Ctx {
                row: spec.row,
                anchor: spec.anchor,
            };
            if let Some(v) = heuristics::generate(h, spec.table, spec.ty, rng, &cx)
                .and_then(|v| conform(v, spec.ty))
                && b.admits(&v)
            {
                return v;
            }
        }
        if spec.profile == Profile::EdgeCases
            && rng.random_bool(0.3)
            && let Some(v) = edge(spec, rng)
        {
            return v;
        }
        return typed(spec, rng);
    }
    if let Some(allowed) = &b.allowed {
        let usable: Vec<&V> = allowed.iter().filter(|v| !b.excluded.contains(v)).collect();
        if !usable.is_empty() {
            let v = (*pick(rng, &usable)).clone();
            return conform(v, spec.ty).unwrap_or(V::Null);
        }
    }
    let natural = spec.heuristic.and_then(Heuristic::range);
    if spec.profile == Profile::EdgeCases
        && let Some((lo, hi)) = natural
        && rng.random_bool(0.3)
    {
        let v = V::Float(if rng.random_bool(0.5) { lo } else { hi });
        let v = spec.heuristic.map_or(v.clone(), |h| adjust(h, v, spec));
        if let Some(v) = conform(v, spec.ty)
            && b.admits(&v)
        {
            return v;
        }
    }
    if spec.profile == Profile::EdgeCases && natural.is_none() && rng.random_bool(0.3) {
        for _ in 0..3 {
            if let Some(v) = edge(spec, rng).and_then(|v| conform(v, spec.ty))
                && b.admits(&v)
            {
                return v;
            }
        }
    }
    if let Some(h) = spec.heuristic {
        let cx = heuristics::Ctx {
            row: spec.row,
            anchor: spec.anchor,
        };
        for _ in 0..3 {
            if let Some(v) = heuristics::generate(h, spec.table, spec.ty, rng, &cx)
                .map(|v| adjust(h, v, spec))
                .and_then(|v| conform(v, spec.ty))
                && b.admits(&v)
            {
                return v;
            }
        }
    }
    if let Some(sampler) = b.patterns.iter().find_map(|p| p.sampler.as_ref()) {
        for _ in 0..8 {
            let s: String = rng.sample(sampler);
            let v = V::Text(s);
            if b.admits(&v) {
                return v;
            }
        }
    }
    let mut last = V::Null;
    for _ in 0..6 {
        let v = typed(spec, rng);
        let Some(v) = conform(v, spec.ty) else {
            continue;
        };
        if b.admits(&v) {
            return v;
        }
        last = v;
    }
    last
}

/// Scales a heuristic's value to the column (cents, fractions).
fn adjust(h: Heuristic, v: V, spec: &Spec) -> V {
    match (h, &v) {
        (Heuristic::Amount, V::Float(f)) if spec.cents && spec.ty.is_integer() => {
            V::Float((f * 100.0).round())
        }
        (Heuristic::Percent, V::Float(f)) if max_value(spec.ty).is_some_and(|m| m < 100.0) => {
            V::Float((f / 100.0 * 1e4).round() / 1e4)
        }
        _ => v,
    }
}

/// The largest value a numeric type holds.
fn max_value(ty: &Type) -> Option<f64> {
    match ty.base {
        Base::Int2 => Some(32_767.0),
        Base::Int4 => Some(2_147_483_647.0),
        Base::Numeric {
            precision: Some(p),
            scale,
        } => Some(
            crate::rng::pow10(i32::try_from(p.saturating_sub(scale)).ok()?.min(300))
                - crate::rng::pow10(-i32::try_from(scale).ok()?),
        ),
        _ => None,
    }
}

fn int_range(base: &Base) -> (i128, i128) {
    match base {
        Base::Int2 => (i128::from(i16::MIN), i128::from(i16::MAX)),
        Base::Int4 | Base::Oid => (i128::from(i32::MIN), i128::from(i32::MAX)),
        _ => (i128::from(i64::MIN), i128::from(i64::MAX)),
    }
}

/// Converts a value to the column's type: rounding, clamping to its range
/// and precision, truncating text to its length. `None` when it cannot.
#[allow(clippy::too_many_lines)]
pub fn conform(v: V, ty: &Type) -> Option<V> {
    if v.is_null() {
        return Some(v);
    }
    if ty.array {
        let elem = Type {
            array: false,
            ..ty.clone()
        };
        return match v {
            V::Array(items) => items
                .into_iter()
                .map(|x| conform(x, &elem))
                .collect::<Option<Vec<_>>>()
                .map(V::Array),
            other => conform(other, &elem).map(|x| V::Array(vec![x])),
        };
    }
    match &ty.base {
        Base::Int2 | Base::Int4 | Base::Int8 | Base::Oid => {
            let (lo, hi) = int_range(&ty.base);
            let lo = if ty.base == Base::Oid { 0 } else { lo };
            let i = match v {
                V::Int(i) => i,
                V::Float(f) if f.is_finite() => f.round() as i128,
                V::Dec { units, scale } => {
                    let d = 10i128.checked_pow(scale)?;
                    (units + d / 2 * units.signum()) / d
                }
                _ => return None,
            };
            Some(V::Int(i.clamp(lo, hi)))
        }
        Base::Float4 | Base::Float8 => Some(V::Float(match v {
            V::Float(f) => f,
            other => other.as_f64()?,
        })),
        Base::Numeric { .. } | Base::Money => {
            let (precision, declared) = match ty.base {
                Base::Numeric { precision, scale } => (precision, scale),
                _ => (Some(17), 2),
            };
            // Values carry at most 18 decimals (`numeric(400, 300)` takes
            // fewer), and the integer digits the type allows.
            let scale = declared.min(18);
            let limit =
                precision.and_then(|p| 10i128.checked_pow(p.saturating_sub(declared) + scale));
            let units = match v {
                V::Int(i) => i.checked_mul(10i128.checked_pow(scale)?)?,
                V::Float(f) if f.is_finite() => {
                    (f * crate::rng::pow10(i32::try_from(scale).ok()?)).round() as i128
                }
                V::Float(_) if precision.is_none() => return Some(V::Float(f64::NAN)),
                V::Dec { units, scale: s } => {
                    if s >= scale {
                        let d = 10i128.checked_pow(s - scale)?;
                        (units + d / 2 * units.signum()) / d
                    } else {
                        units.checked_mul(10i128.checked_pow(scale - s)?)?
                    }
                }
                _ => return None,
            };
            let units = match limit {
                Some(limit) => units.clamp(-(limit - 1), limit - 1),
                None => units,
            };
            Some(V::Dec { units, scale })
        }
        Base::Date => match v {
            V::Date(_) => Some(v),
            V::Ts(t) if t == i64::MAX || t == i64::MIN => Some(V::Date(t)),
            V::Ts(t) => Some(V::Date(t.div_euclid(DAY))),
            _ => None,
        },
        Base::Timestamp { precision, .. } => {
            let t = match v {
                V::Ts(t) => t,
                V::Date(d) => d * DAY,
                V::Raw(_) => return Some(v),
                _ => return None,
            };
            if t == i64::MAX || t == i64::MIN {
                return Some(V::Ts(t));
            }
            let step = 10i64.pow(6 - precision.unwrap_or(6).min(6));
            Some(V::Ts(t - t.rem_euclid(step)))
        }
        Base::Text
        | Base::Citext
        | Base::Varchar(_)
        | Base::Char(_)
        | Base::Name
        | Base::InternalChar => {
            let s = match v {
                V::Text(s) | V::Raw(s) => s,
                other => other.text(true)?,
            };
            Some(V::Text(match ty.max_chars() {
                Some(max) if s.chars().count() > max => s
                    .chars()
                    .take(max)
                    .collect::<String>()
                    .trim_end()
                    .to_owned(),
                _ => s,
            }))
        }
        Base::Bool => matches!(v, V::Bool(_)).then_some(v),
        Base::Uuid => matches!(v, V::Uuid(_)).then_some(v),
        Base::Enum { labels, .. } => match &v {
            V::Text(s) if labels.contains(s) => Some(v),
            _ => None,
        },
        Base::Json | Base::Jsonb => match v {
            V::Text(_) | V::Raw(_) => Some(v),
            V::Int(_) | V::Float(_) | V::Dec { .. } | V::Bool(_) => Some(V::Raw(v.text(true)?)),
            _ => None,
        },
        _ => Some(v),
    }
}

/// A value of the column's type, within its bounds where it has some.
#[allow(clippy::too_many_lines)]
pub fn typed(spec: &Spec, rng: &mut ChaCha8Rng) -> V {
    if spec.ty.array {
        let elem = Type {
            array: false,
            ..spec.ty.clone()
        };
        let n = match spec.profile {
            Profile::EdgeCases => rng.random_range(0..8),
            Profile::Realistic => rng.random_range(0..4),
        };
        let inner = Spec {
            ty: &elem,
            heuristic: None,
            unique: false,
            disjoint: false,
            ..*spec
        };
        // Each element as a value of its own: a domain's CHECK holds per
        // element.
        let items = (0..n)
            .map(|_| conform(candidate(&inner, rng), &elem).unwrap_or(V::Null))
            .collect();
        return V::Array(items);
    }
    let b = spec.bounds;
    let edge = spec.profile == Profile::EdgeCases;
    match &spec.ty.base {
        Base::Int2 | Base::Int4 | Base::Int8 | Base::Oid => {
            let (tlo, thi) = int_range(&spec.ty.base);
            let (lo, hi) = int_bounds(b, tlo, thi);
            V::Int(int_in(rng, lo, hi))
        }
        Base::Float4 | Base::Float8 => {
            let (lo, hi) = float_bounds(b);
            let f = match (lo, hi) {
                (Some(l), Some(h)) if h > l => rng.random_range(l..=h),
                (Some(l), _) => l + (skewed(rng, 10_000) as f64) + rng.random::<f64>(),
                (None, Some(h)) => h - (skewed(rng, 10_000) as f64) - rng.random::<f64>(),
                (None, None) => (normal(rng) * 100.0 * 1e4).round() / 1e4,
            };
            V::Float((f * 1e6).round() / 1e6)
        }
        Base::Numeric { precision, scale } => {
            let max = precision.map(|p| {
                crate::rng::pow10(
                    i32::try_from(p.saturating_sub(*scale))
                        .unwrap_or(9)
                        .min(300),
                ) - 1.0
            });
            let (lo, hi) = float_bounds(b);
            let hi = match (hi, max) {
                (Some(h), Some(m)) => Some(h.min(m)),
                (h, m) => h.or(m),
            };
            let lo = lo.or(max.map(|m| if b.lower.is_none() && !edge { 0.0 } else { -m }));
            let f = match (lo, hi) {
                (Some(l), Some(h)) if h > l => {
                    if h - l > 10_000.0 && !edge {
                        l + (skewed(rng, 10_000) as f64) + rng.random::<f64>()
                    } else {
                        rng.random_range(l..=h)
                    }
                }
                (Some(l), _) => l + (skewed(rng, 10_000) as f64) + rng.random::<f64>(),
                (None, Some(h)) => h - (skewed(rng, 10_000) as f64),
                (None, None) => (rng.random_range(0.0..1000.0f64) * 100.0).round() / 100.0,
            };
            V::Float(f)
        }
        Base::Money => V::Float((rng.random_range(1.0..500.0f64) * 100.0).round() / 100.0),
        Base::Text | Base::Citext | Base::Varchar(_) | Base::Name => V::Text(text(spec, rng)),
        Base::Char(n) => {
            const ALNUM: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ0123456789";
            let n = (*n as usize)
                .min(b.max_len.unwrap_or(usize::MAX))
                .max(b.min_len.unwrap_or(1))
                .min(16);
            V::Text(
                (0..n)
                    .map(|_| char::from(ALNUM[rng.random_range(0..ALNUM.len())]))
                    .collect(),
            )
        }
        Base::InternalChar => V::Text(char::from(rng.random_range(b'a'..=b'z')).to_string()),
        Base::Bool => V::Bool(rng.random_bool(0.5)),
        Base::Uuid => V::Uuid(uuid(rng)),
        Base::Date => V::Date(timestamp(b, rng, spec.anchor).div_euclid(DAY)),
        Base::Timestamp { .. } => {
            if edge && b.lower.is_none() && b.upper.is_none() && rng.random_bool(0.1) {
                return V::Ts(if rng.random_bool(0.5) {
                    i64::MAX
                } else {
                    i64::MIN
                });
            }
            V::Ts(timestamp(b, rng, spec.anchor))
        }
        Base::Time { .. } => V::Time(rng.random_range(0..DAY / 1_000_000) * 1_000_000),
        Base::Interval => V::Interval {
            days: skewed(rng, 90) as i64,
            micros: rng.random_range(0..DAY / 1_000_000) * 1_000_000,
        },
        Base::Bytea => {
            let n = if edge {
                rng.random_range(0..64)
            } else {
                rng.random_range(8..33)
            };
            V::Bytes((0..n).map(|_| rng.random::<u8>()).collect())
        }
        Base::Inet => V::Raw(match rng.random_range(0..6) {
            0 => format!("2001:db8::{:x}", rng.random_range(1..0xffffu32)),
            n => format!(
                "{}.{}",
                ["192.0.2", "198.51.100", "203.0.113"][n % 3],
                rng.random_range(1..255)
            ),
        }),
        Base::Cidr => V::Raw(
            [
                "192.0.2.0/24",
                "198.51.100.0/24",
                "203.0.113.0/24",
                "2001:db8::/32",
                "10.0.0.0/8",
            ][rng.random_range(0..5)]
            .to_owned(),
        ),
        Base::Macaddr => V::Raw(mac(rng, 6)),
        Base::Macaddr8 => V::Raw(mac(rng, 8)),
        Base::Json | Base::Jsonb => {
            let id = rng.random_range(1..10_000);
            let word = pick(rng, words::WORDS);
            let flag = rng.random_bool(0.5);
            if edge && rng.random_bool(0.3) {
                V::Raw(
                    pick(
                        rng,
                        &[
                            "{}",
                            "[]",
                            "null",
                            r#"{"nested": {"deeper": {"deepest": [1, 2, {"x": "ü😀"}]}}}"#,
                            r#""just a string""#,
                        ],
                    )
                    .to_string(),
                )
            } else {
                V::Raw(format!(
                    r#"{{"id": {id}, "name": "{word}", "enabled": {flag}}}"#
                ))
            }
        }
        Base::Xml => V::Raw(format!(
            "<note><id>{}</id><text>{}</text></note>",
            spec.row + 1,
            pick(rng, words::WORDS)
        )),
        Base::Bit(n) => V::Raw(
            (0..*n)
                .map(|_| if rng.random_bool(0.5) { '1' } else { '0' })
                .collect(),
        ),
        Base::VarBit(n) => {
            let len = rng.random_range(1..=n.unwrap_or(16).max(1));
            V::Raw(
                (0..len)
                    .map(|_| if rng.random_bool(0.5) { '1' } else { '0' })
                    .collect(),
            )
        }
        Base::TsVector => V::Raw(
            (0..rng.random_range(1..6))
                .map(|_| *pick(rng, words::WORDS))
                .collect::<Vec<_>>()
                .join(" "),
        ),
        Base::TsQuery => V::Raw(format!(
            "{} & {}",
            pick(rng, words::WORDS),
            pick(rng, words::WORDS)
        )),
        Base::Point => {
            let (x, y) = coordinates(rng);
            V::Raw(format!("({x},{y})"))
        }
        Base::Line => V::Raw(format!(
            "{{{},{},{}}}",
            rng.random_range(1..10),
            rng.random_range(-10..10),
            rng.random_range(-100..100)
        )),
        Base::Lseg | Base::Box | Base::Path => {
            let (x, y) = coordinates(rng);
            let (dx, dy) = (
                rng.random_range(0.01..1.0f64),
                rng.random_range(0.01..1.0f64),
            );
            let (x2, y2) = (round6(x + dx), round6(y + dy));
            V::Raw(match spec.ty.base {
                Base::Lseg => format!("[({x},{y}),({x2},{y2})]"),
                Base::Box => format!("({x2},{y2}),({x},{y})"),
                _ => format!("[({x},{y}),({x2},{y}),({x2},{y2})]"),
            })
        }
        Base::Polygon => {
            let (x, y) = coordinates(rng);
            let d = round6(rng.random_range(0.01..1.0f64));
            V::Raw(format!(
                "(({x},{y}),({},{y}),({},{}),({x},{}))",
                round6(x + d),
                round6(x + d),
                round6(y + d),
                round6(y + d)
            ))
        }
        Base::Circle => {
            let (x, y) = coordinates(rng);
            V::Raw(format!(
                "<({x},{y}),{}>",
                round6(rng.random_range(0.1..10.0f64))
            ))
        }
        Base::PgLsn => V::Raw(format!(
            "{:X}/{:X}",
            rng.random_range(0..16u32),
            rng.random::<u32>()
        )),
        Base::Hstore => {
            let n = rng.random_range(1..4);
            let pairs: Vec<String> = (0..n)
                .map(|i| {
                    format!(
                        "\"{}{i}\"=>\"{}\"",
                        pick(rng, words::WORDS),
                        pick(rng, words::WORDS)
                    )
                })
                .collect();
            V::Raw(pairs.join(", "))
        }
        Base::Ltree => {
            let depth = rng.random_range(1..4);
            let mut labels = vec!["top".to_owned()];
            for _ in 0..depth {
                labels.push((*pick(rng, words::WORDS)).to_owned());
            }
            V::Raw(labels.join("."))
        }
        Base::Geometry {
            geography,
            shape,
            srid,
        } => V::Raw(geometry(rng, *geography, shape.as_deref(), *srid, None)),
        Base::Vector { kind, dims } => V::Raw(vector(rng, *kind, dims.unwrap_or(3))),
        Base::Cube => {
            let (x, y) = coordinates(rng);
            V::Raw(format!("({x}, {y})"))
        }
        Base::Range(kind) => V::Raw(range(rng, *kind, spec, None)),
        Base::Multirange(kind) => {
            let a = range_bounds(rng, *kind, spec, None);
            let b2 = range_bounds(rng, *kind, spec, None);
            let (first, second) = if a.0 <= b2.0 { (a, b2) } else { (b2, a) };
            let r1 = format_range(*kind, first.0, first.1);
            let r2 = format_range(*kind, second.0.max(first.1 + 1), second.1.max(first.1 + 2));
            V::Raw(format!("{{{r1},{r2}}}"))
        }
        Base::Enum { labels, .. } => {
            if labels.is_empty() {
                return V::Null;
            }
            let i = if edge && rng.random_bool(0.3) {
                labels.len() - 1
            } else {
                skewed(rng, labels.len() as u64) as usize
            };
            V::Text(labels[i].clone())
        }
        Base::Composite { attributes, .. } => {
            let fields = attributes
                .iter()
                .map(|(name, ty)| {
                    let bounds = crate::checks::domain_bounds(ty);
                    let inner = Spec {
                        ty,
                        bounds: &bounds,
                        heuristic: heuristics::choose(spec.table, name, ty),
                        unique: false,
                        disjoint: false,
                        ..*spec
                    };
                    let not_null = ty.domain.as_ref().is_some_and(|d| d.not_null);
                    if !not_null && rng.random_bool(0.08) {
                        V::Null
                    } else {
                        let v = candidate(&inner, rng);
                        conform(v, ty).unwrap_or(V::Null)
                    }
                })
                .collect();
            V::Record(fields)
        }
        Base::Unknown(_) => V::Null,
    }
}

fn round6(f: f64) -> f64 {
    (f * 1e6).round() / 1e6
}

fn coordinates(rng: &mut ChaCha8Rng) -> (f64, f64) {
    let (_, _, lat, lon) = *pick(rng, words::CITIES);
    (
        round6(lon + normal(rng) * 0.05),
        round6(lat + normal(rng) * 0.05),
    )
}

/// EWKT for a PostGIS column: the shape and SRID from its typmod. `at`
/// places a point on a grid instead (unique points).
pub fn geometry(
    rng: &mut ChaCha8Rng,
    geography: bool,
    shape: Option<&str>,
    srid: Option<i32>,
    at: Option<u64>,
) -> String {
    let shape = shape.unwrap_or("point");
    let (z, m) = if shape.ends_with("zm") {
        (true, true)
    } else if shape.ends_with('z') {
        (true, false)
    } else {
        (false, shape.ends_with('m') && !shape.ends_with("geom"))
    };
    let base = shape.trim_end_matches("zm").trim_end_matches('z');
    let base = if m && !z {
        base.trim_end_matches('m')
    } else {
        base
    };
    let srid = srid.filter(|s| *s > 0).or(geography.then_some(4326));
    let (cx, cy) = match at {
        Some(i) => (
            -179.0 + (i % 3000) as f64 * 0.1,
            -80.0 + (i / 3000 % 1600) as f64 * 0.1,
        ),
        None => coordinates(rng),
    };
    let extra = |s: &mut String| {
        if z {
            s.push_str(" 10");
        }
        if m {
            s.push_str(" 1");
        }
    };
    let pt = |x: f64, y: f64| {
        let mut s = format!("{} {}", round6(x), round6(y));
        extra(&mut s);
        s
    };
    let d = 0.01;
    let polygon = |x: f64, y: f64| {
        format!(
            "(({}, {}, {}, {}, {}))",
            pt(x, y),
            pt(x + d, y),
            pt(x + d, y + d),
            pt(x, y + d),
            pt(x, y)
        )
    };
    let line = |x: f64, y: f64| {
        format!(
            "({}, {}, {})",
            pt(x, y),
            pt(x + d, y + d / 2.0),
            pt(x + 2.0 * d, y + d)
        )
    };
    let dims = match (z, m) {
        (true, true) => " ZM",
        (true, false) => " Z",
        (false, true) => " M",
        (false, false) => "",
    };
    let body = match base {
        "linestring" => format!("LINESTRING{dims}{}", line(cx, cy)),
        "polygon" => format!("POLYGON{dims}{}", polygon(cx, cy)),
        "multipoint" => format!("MULTIPOINT{dims}({}, {})", pt(cx, cy), pt(cx + d, cy + d)),
        "multilinestring" => format!("MULTILINESTRING{dims}({})", line(cx, cy)),
        "multipolygon" => format!("MULTIPOLYGON{dims}({})", polygon(cx, cy)),
        "geometrycollection" => format!("GEOMETRYCOLLECTION{dims}(POINT{dims}({}))", pt(cx, cy)),
        _ => format!("POINT{dims}({})", pt(cx, cy)),
    };
    let _ = rng;
    match srid {
        Some(s) => format!("SRID={s};{body}"),
        None => body,
    }
}

/// A pgvector literal: a unit vector of `dims` dimensions.
fn vector(rng: &mut ChaCha8Rng, kind: VectorKind, dims: u32) -> String {
    let dims = dims.max(1) as usize;
    if kind == VectorKind::SparseVec {
        let n = dims.min(4);
        let mut idx: Vec<usize> = (0..n).map(|_| rng.random_range(1..=dims)).collect();
        idx.sort_unstable();
        idx.dedup();
        let entries: Vec<String> = idx
            .iter()
            .map(|i| format!("{i}:{:.4}", rng.random_range(0.01..1.0f64)))
            .collect();
        return format!("{{{}}}/{dims}", entries.join(","));
    }
    let mut v: Vec<f64> = (0..dims).map(|_| rng.random_range(-1.0..1.0f64)).collect();
    let norm = v
        .iter()
        .map(|x| x * x)
        .sum::<f64>()
        .sqrt()
        .max(f64::MIN_POSITIVE);
    for x in &mut v {
        *x /= norm;
    }
    let mut out = String::with_capacity(dims * 9 + 2);
    out.push('[');
    for (i, x) in v.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        let _ = std::fmt::Write::write_fmt(&mut out, format_args!("{x:.5}"));
    }
    out.push(']');
    out
}

fn mac(rng: &mut ChaCha8Rng, n: usize) -> String {
    (0..n)
        .map(|i| {
            // A locally administered, unicast first octet.
            let b: u8 = if i == 0 {
                (rng.random::<u8>() & 0xfc) | 0x02
            } else {
                rng.random()
            };
            format!("{b:02x}")
        })
        .collect::<Vec<_>>()
        .join(":")
}

pub fn uuid(rng: &mut ChaCha8Rng) -> u128 {
    let mut u: u128 = rng.random();
    // Version 4, RFC 4122 variant.
    u = (u & !(0xf << 76)) | (0x4 << 76);
    u = (u & !(0x3 << 62)) | (0x2 << 62);
    u
}

fn int_bounds(b: &Bounds, tlo: i128, thi: i128) -> (Option<i128>, Option<i128>) {
    let bound = |v: &V, inclusive: bool, upper: bool| -> Option<i128> {
        let step = if upper { -1 } else { 1 };
        if let V::Int(i) = v {
            return Some(if inclusive { *i } else { i + step });
        }
        let f = v.as_f64()?;
        let i = if upper { f.floor() } else { f.ceil() } as i128;
        // A bound exactly on an integer, exclusive, moves past it.
        let on_it = (i as f64 - f).abs() < f64::EPSILON;
        Some(if !inclusive && on_it { i + step } else { i })
    };
    let lo = b.lower.as_ref().and_then(|(v, inc)| bound(v, *inc, false));
    let hi = b.upper.as_ref().and_then(|(v, inc)| bound(v, *inc, true));
    (lo.map(|l| l.max(tlo)), hi.map(|h| h.min(thi)))
}

fn int_in(rng: &mut ChaCha8Rng, lo: Option<i128>, hi: Option<i128>) -> i128 {
    let spread = |rng: &mut ChaCha8Rng| i128::from(skewed(rng, 10_000));
    match (lo, hi) {
        (Some(l), Some(h)) if h >= l => {
            if h - l > 1_000_000 {
                l + spread(rng)
            } else {
                rng.random_range(l..=h)
            }
        }
        (Some(l), _) => l + spread(rng),
        (None, Some(h)) => h - spread(rng),
        (None, None) => 1 + spread(rng),
    }
}

fn float_bounds(b: &Bounds) -> (Option<f64>, Option<f64>) {
    let nudge = |f: f64, inc: bool, up: bool| {
        if inc {
            f
        } else if up {
            f - f.abs().max(1.0) * 1e-6
        } else {
            f + f.abs().max(1.0) * 1e-6
        }
    };
    (
        b.lower
            .as_ref()
            .and_then(|(v, inc)| v.as_f64().map(|f| nudge(f, *inc, false))),
        b.upper
            .as_ref()
            .and_then(|(v, inc)| v.as_f64().map(|f| nudge(f, *inc, true))),
    )
}

/// An instant in the last two years before the anchor, within the bounds.
fn timestamp(b: &Bounds, rng: &mut ChaCha8Rng, anchor: i64) -> i64 {
    let as_ts = |v: &V| match v {
        V::Ts(t) => Some(*t),
        V::Date(d) => Some(d * DAY),
        _ => None,
    };
    let lo = b
        .lower
        .as_ref()
        .and_then(|(v, inc)| as_ts(v).map(|t| if *inc { t } else { t + 1_000_000 }));
    let hi = b
        .upper
        .as_ref()
        .and_then(|(v, inc)| as_ts(v).map(|t| if *inc { t } else { t - 1_000_000 }));
    let (mut from, mut to) = (anchor - 730 * DAY, anchor);
    if let Some(l) = lo {
        from = from.max(l);
    }
    if let Some(h) = hi {
        to = to.min(h);
    }
    if to <= from {
        // The bounds sit outside the window: a year inside them.
        match (lo, hi) {
            (Some(l), Some(h)) if h > l => (from, to) = (l, h),
            (Some(l), _) => (from, to) = (l, l + 365 * DAY),
            (_, Some(h)) => (from, to) = (h - 365 * DAY, h),
            _ => return anchor,
        }
    }
    let t = rng.random_range(from..=to);
    t - t.rem_euclid(1_000_000)
}

/// Words, within the length bounds.
fn text(spec: &Spec, rng: &mut ChaCha8Rng) -> String {
    let b = spec.bounds;
    let max = b.max_len.into_iter().chain(spec.ty.max_chars()).min();
    let min = b.min_len.unwrap_or(0);
    let mut s = String::new();
    let n = rng.random_range(1..4);
    for i in 0..n {
        if i > 0 {
            s.push(' ');
        }
        s.push_str(pick(rng, words::WORDS));
    }
    while s.chars().count() < min {
        s.push(' ');
        s.push_str(pick(rng, words::WORDS));
    }
    if let Some(max) = max
        && s.chars().count() > max
    {
        s = s.chars().take(max).collect::<String>();
        // A shortened text should not end on a space (char(n) trims it).
        while s.ends_with(' ') && s.chars().count() > min.max(1) {
            s.pop();
        }
    }
    s
}

/// An edge-case value: extremes, max-length and empty strings, odd text.
fn edge(spec: &Spec, rng: &mut ChaCha8Rng) -> Option<V> {
    let b = spec.bounds;
    if spec.ty.array {
        let null_elements = !spec.ty.domain.as_ref().is_some_and(|d| d.not_null);
        return Some(match rng.random_range(0..3) {
            0 => V::Array(Vec::new()),
            1 if null_elements => V::Array(vec![V::Null]),
            _ => return None,
        });
    }
    match &spec.ty.base {
        Base::Int2 | Base::Int4 | Base::Int8 => {
            let (tlo, thi) = int_range(&spec.ty.base);
            let (lo, hi) = int_bounds(b, tlo, thi);
            let options = [lo.unwrap_or(tlo), hi.unwrap_or(thi), 0, -1, 1];
            Some(V::Int(*pick(rng, &options)))
        }
        Base::Float4 | Base::Float8 => {
            let (lo, hi) = float_bounds(b);
            let options = [
                lo.unwrap_or(-1e300),
                hi.unwrap_or(1e300),
                0.0,
                -0.0,
                1e-300,
                f64::NAN,
            ];
            Some(V::Float(*pick(rng, &options)))
        }
        Base::Numeric { precision, scale } => {
            let max = precision.map_or(1e12, |p| {
                crate::rng::pow10(
                    i32::try_from(p.saturating_sub(*scale))
                        .unwrap_or(9)
                        .min(300),
                ) - crate::rng::pow10(-i32::try_from(*scale).unwrap_or(0))
            });
            let (lo, hi) = float_bounds(b);
            let options = [lo.unwrap_or(-max), hi.unwrap_or(max), 0.0];
            Some(V::Float(*pick(rng, &options)))
        }
        Base::Text | Base::Citext | Base::Varchar(_) | Base::Name | Base::Char(_) => {
            let max = b.max_len.into_iter().chain(spec.ty.max_chars()).min();
            Some(V::Text(match rng.random_range(0..4) {
                0 => {
                    let n = max.unwrap_or(1000).min(4000);
                    let unit = pick(rng, &["x", "é", "字", "ab"]);
                    unit.repeat(n).chars().take(n).collect()
                }
                1 => String::new(),
                _ => (*pick(rng, words::TRICKY)).to_owned(),
            }))
        }
        Base::Date | Base::Timestamp { .. } => {
            let options = [0, 946_684_800 * 1_000_000, 32_503_593_600 * 1_000_000 - DAY];
            Some(V::Ts(*pick(rng, &options)))
        }
        _ => None,
    }
}

fn range_bounds(
    rng: &mut ChaCha8Rng,
    kind: RangeKind,
    spec: &Spec,
    slot: Option<u64>,
) -> (i64, i64) {
    match (kind, slot) {
        (RangeKind::Int4 | RangeKind::Int8 | RangeKind::Num, Some(i)) => {
            let start = i as i64 * 10;
            (start, start + rng.random_range(1..10))
        }
        (RangeKind::Int4 | RangeKind::Int8 | RangeKind::Num, None) => {
            let start = rng.random_range(0..10_000);
            (start, start + rng.random_range(1..500))
        }
        (RangeKind::Date, Some(i)) => {
            let start = spec.anchor.div_euclid(DAY) - 730 + i as i64 * 2;
            (start, start + 1)
        }
        (RangeKind::Date, None) => {
            let start = spec.anchor.div_euclid(DAY) - rng.random_range(0..730);
            (start, start + rng.random_range(1..30))
        }
        (RangeKind::Ts | RangeKind::TsTz, Some(i)) => {
            // One-hour slots, back to back from two years ago: never overlapping.
            let start = spec.anchor - 730 * DAY + i as i64 * 3_600_000_000;
            (start, start + rng.random_range(15..60) * 60_000_000)
        }
        (RangeKind::Ts | RangeKind::TsTz, None) => {
            let start = spec.anchor - rng.random_range(0..730 * 24) * 3_600_000_000;
            (start, start + rng.random_range(1..48) * 3_600_000_000)
        }
    }
}

fn format_range(kind: RangeKind, lo: i64, hi: i64) -> String {
    match kind {
        RangeKind::Int4 | RangeKind::Int8 => format!("[{lo},{hi})"),
        RangeKind::Num => format!("[{lo}.5,{hi}.25)"),
        RangeKind::Date => format!(
            "[{},{})",
            V::Date(lo).text(false).unwrap_or_default(),
            V::Date(hi).text(false).unwrap_or_default()
        ),
        RangeKind::Ts => format!(
            "[\"{}\",\"{}\")",
            V::Ts(lo).text(false).unwrap_or_default(),
            V::Ts(hi).text(false).unwrap_or_default()
        ),
        RangeKind::TsTz => format!(
            "[\"{}\",\"{}\")",
            V::Ts(lo).text(true).unwrap_or_default(),
            V::Ts(hi).text(true).unwrap_or_default()
        ),
    }
}

fn range(rng: &mut ChaCha8Rng, kind: RangeKind, spec: &Spec, slot: Option<u64>) -> String {
    if spec.profile == Profile::EdgeCases && slot.is_none() && rng.random_bool(0.15) {
        return pick(rng, &["empty", "(,)"]).to_string();
    }
    let (lo, hi) = range_bounds(rng, kind, spec, slot);
    format_range(kind, lo, hi)
}

fn base36(mut n: u64, width: usize) -> String {
    const DIGITS: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ";
    let mut out = Vec::new();
    loop {
        out.push(DIGITS[(n % 36) as usize]);
        n /= 36;
        if n == 0 {
            break;
        }
    }
    while out.len() < width {
        out.push(b'0');
    }
    out.reverse();
    String::from_utf8(out).unwrap_or_default()
}

/// How many distinct values [`unique`] can give the column (`None`: more
/// than any table holds).
pub fn capacity(ty: &Type, b: &Bounds) -> Option<u64> {
    if ty.array {
        return None;
    }
    let cap = |n: i128| u64::try_from(n.max(0)).unwrap_or(u64::MAX);
    if let Some(allowed) = &b.allowed {
        return Some(allowed.len() as u64);
    }
    match &ty.base {
        Base::Int2 | Base::Int4 | Base::Int8 => {
            let (tlo, thi) = int_range(&ty.base);
            let (lo, hi) = int_bounds(b, tlo, thi);
            Some(cap(hi.unwrap_or(thi) - lo.unwrap_or(1) + 1))
        }
        Base::Char(n) => Some(36u64.saturating_pow((*n).min(12))),
        // The row number in decimal, when no other text fits.
        Base::Varchar(Some(n)) => Some(10u64.saturating_pow((*n).min(18))),
        Base::Bit(n) => Some(1u64.checked_shl(*n).unwrap_or(u64::MAX)),
        Base::InternalChar => Some(26),
        // Unique times are whole seconds of one day.
        Base::Time { .. } => Some(86_400),
        Base::Bool => Some(2),
        Base::Enum { labels, .. } => Some(labels.len() as u64),
        Base::Unknown(_) => Some(0),
        Base::Composite { attributes, .. } => attributes
            .iter()
            .map(|(_, t)| capacity(t, &Bounds::default()))
            .try_fold(0u64, |acc, c| c.map(|c| acc.max(c))),
        _ => None,
    }
}

/// The value made distinct from every other row's: the row number written
/// into it (text), or values spaced by row (numbers, times).
#[allow(clippy::too_many_lines)]
fn unique(spec: &Spec, v: V, rng: &mut ChaCha8Rng) -> Option<V> {
    let b = spec.bounds;
    let row = spec.row;
    if spec.ty.array {
        let elem = Type {
            array: false,
            ..spec.ty.clone()
        };
        let inner = Spec { ty: &elem, ..*spec };
        let first = unique(&inner, typed(&inner, rng), rng)?;
        return Some(V::Array(vec![first]));
    }
    if let Some(allowed) = &b.allowed {
        return allowed
            .get((row % allowed.len().max(1) as u64) as usize)
            .cloned();
    }
    match &spec.ty.base {
        Base::Int2 | Base::Int4 | Base::Int8 => {
            let (tlo, thi) = int_range(&spec.ty.base);
            let (lo, _) = int_bounds(b, tlo, thi);
            Some(V::Int(lo.unwrap_or(1) + i128::from(row)))
        }
        Base::Numeric { .. } | Base::Money => {
            let scale = match spec.ty.base {
                Base::Numeric { scale, .. } => scale,
                _ => 2,
            };
            let lo = float_bounds(b).0.unwrap_or(0.0);
            let unit = crate::rng::pow10(-i32::try_from(scale).unwrap_or(0));
            conform(V::Float(lo + (row + 1) as f64 * unit.max(0.01)), spec.ty)
        }
        Base::Float4 | Base::Float8 => {
            let lo = float_bounds(b).0.unwrap_or(0.0);
            Some(V::Float(lo + row as f64 + 0.5))
        }
        Base::Text | Base::Citext | Base::Varchar(_) | Base::Name => {
            let base = match v {
                V::Text(s) => s,
                other => other.text(true).unwrap_or_default(),
            };
            let max = b.max_len.into_iter().chain(spec.ty.max_chars()).min();
            let made = numbered(&base, row, max);
            let made = V::Text(made);
            if b.admits(&made) {
                return Some(made);
            }
            // A pattern the numbered text breaks: a sample with the row
            // number in its digits (`AB12` → `AB0017`), else a sample
            // (distinct in practice, Postgres drops the rare duplicate).
            if let Some(sampler) = b.patterns.iter().find_map(|p| p.sampler.as_ref()) {
                for _ in 0..8 {
                    let s: String = rng.sample(sampler);
                    if let Some(candidate) = with_row_digits(&s, row).map(V::Text)
                        && b.admits(&candidate)
                    {
                        return Some(candidate);
                    }
                }
                for _ in 0..8 {
                    let s: String = rng.sample(sampler);
                    let candidate = V::Text(s);
                    if b.admits(&candidate) {
                        return Some(candidate);
                    }
                }
            }
            Some(made)
        }
        Base::Char(n) => {
            let width = (*n as usize).min(b.max_len.unwrap_or(usize::MAX));
            Some(V::Text(base36(row, width)))
        }
        Base::Bit(n) => Some(V::Raw(format!("{row:0width$b}", width = *n as usize))),
        Base::InternalChar => Some(V::Text(char::from(b'a' + (row % 26) as u8).to_string())),
        Base::Uuid | Base::Bytea | Base::Vector { .. } => Some(v),
        Base::Date => {
            let start = spec.anchor.div_euclid(DAY) - spec.rows as i64;
            Some(V::Date(start + row as i64))
        }
        Base::Timestamp { .. } => {
            let step = (730 * DAY / spec.rows.max(1) as i64).max(1_000_000);
            let jitter = rng.random_range(0..step / 1_000_000) * 1_000_000;
            Some(V::Ts(
                spec.anchor - 730 * DAY + row as i64 * step + jitter.min(step - 1_000_000),
            ))
        }
        Base::Time { .. } => Some(V::Time((row as i64 % (DAY / 1_000_000)) * 1_000_000)),
        Base::Interval => Some(V::Interval {
            days: row as i64,
            micros: 0,
        }),
        Base::Inet | Base::Cidr => Some(V::Raw(if row < (1 << 24) {
            format!(
                "10.{}.{}.{}",
                (row >> 16) & 0xff,
                (row >> 8) & 0xff,
                row & 0xff
            )
        } else {
            format!("2001:db8::{:x}:{:x}", row >> 16, row & 0xffff)
        })),
        Base::Macaddr => Some(V::Raw(format!(
            "02:00:{:02x}:{:02x}:{:02x}:{:02x}",
            (row >> 24) & 0xff,
            (row >> 16) & 0xff,
            (row >> 8) & 0xff,
            row & 0xff
        ))),
        Base::Json | Base::Jsonb => Some(V::Raw(format!(r#"{{"id": {}}}"#, row + 1))),
        Base::Ltree => Some(V::Raw(format!("top.{}.n{row}", pick(rng, words::WORDS)))),
        Base::Hstore => Some(V::Raw(format!("\"id\"=>\"{row}\""))),
        Base::Geometry {
            geography,
            shape,
            srid,
        } => Some(V::Raw(geometry(
            rng,
            *geography,
            shape.as_deref(),
            *srid,
            Some(row),
        ))),
        Base::Point => Some(V::Raw(format!("({},{})", row % 10_000, row / 10_000))),
        Base::Range(kind) => Some(V::Raw(range(rng, *kind, spec, Some(row)))),
        Base::Composite { attributes, .. } => {
            let V::Record(mut fields) = v else {
                return None;
            };
            let i = attributes.iter().position(|(_, t)| t.can_be_unique())?;
            let (name, ty) = &attributes[i];
            let none = Bounds::default();
            let inner = Spec {
                ty,
                bounds: &none,
                heuristic: heuristics::choose(spec.table, name, ty),
                ..*spec
            };
            let base = candidate(&inner, rng);
            fields[i] = unique(&inner, base, rng)?;
            Some(V::Record(fields))
        }
        _ => None,
    }
}

/// `s` with its last run of digits replaced by the row number (padded to
/// the run's width): distinct per row, and still of the same shape.
fn with_row_digits(s: &str, row: u64) -> Option<String> {
    let bytes = s.as_bytes();
    let end = bytes.iter().rposition(u8::is_ascii_digit)? + 1;
    let start = bytes[..end]
        .iter()
        .rposition(|b| !b.is_ascii_digit())
        .map_or(0, |i| i + 1);
    let number = format!("{:0width$}", row + 1, width = end - start);
    Some(format!("{}{number}{}", &s[..start], &s[end..]))
}

/// `base` with the row number written in, so no two rows share it: before
/// the `@` of an email, else at the end. The number survives truncation.
pub fn numbered(base: &str, row: u64, max: Option<usize>) -> String {
    let (head, tail) = match base.find('@') {
        Some(at) => (&base[..at], &base[at..]),
        None => (base, ""),
    };
    let number = (row + 1).to_string();
    // Room for a separator is kept whatever it turns out to be, and it is
    // chosen on the head as it ends up: a cut can leave it ending on a
    // digit (`ab1|c` + 5 must not read as `ab` + 15).
    let fixed = 1 + number.len() + tail.chars().count();
    let keep = match max {
        Some(m) if m < fixed + 1 => {
            // No room for the text: the number alone, as much as fits.
            let digits: Vec<char> = number.chars().collect();
            return digits[digits.len().saturating_sub(m)..].iter().collect();
        }
        Some(m) => m - fixed,
        None => usize::MAX,
    };
    let head: String = head.chars().take(keep).collect();
    let head = head.trim_end();
    if head.is_empty() && tail.is_empty() {
        return number;
    }
    let ends_with_digit = head.ends_with(|c: char| c.is_ascii_digit());
    let sep = if !tail.is_empty() {
        if ends_with_digit { "." } else { "" }
    } else if head.contains(' ') {
        " "
    } else if head.contains('-') || ends_with_digit {
        "-"
    } else if head.contains('_') {
        "_"
    } else if head.chars().any(char::is_lowercase) && !head.starts_with(char::is_uppercase) {
        ""
    } else if head
        .chars()
        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
    {
        "-"
    } else {
        " "
    };
    format!("{head}{sep}{number}{tail}")
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;
    use crate::{checks::Pattern, rng};

    fn spec<'a>(ty: &'a Type, bounds: &'a Bounds, row: u64) -> Spec<'a> {
        Spec {
            table: "t",
            ty,
            bounds,
            heuristic: None,
            profile: Profile::Realistic,
            unique: false,
            disjoint: false,
            cents: false,
            row,
            rows: 1000,
            anchor: 1_790_000_000 * 1_000_000,
        }
    }

    fn draw(s: &Spec) -> V {
        value(s, &mut rng::cell(&rng::key(1, &["c"]), s.row, 0))
    }

    #[test]
    fn numbers_stay_within_their_bounds() {
        let ty = Type::scalar(Base::Int4);
        let b = Bounds {
            lower: Some((V::Int(10), false)),
            upper: Some((V::Int(20), true)),
            ..Bounds::default()
        };
        for row in 0..200 {
            let V::Int(i) = draw(&spec(&ty, &b, row)) else {
                panic!()
            };
            assert!((11..=20).contains(&i), "{i}");
        }
        let ty = Type::scalar(Base::Numeric {
            precision: Some(5),
            scale: 2,
        });
        let b = Bounds::default();
        for row in 0..200 {
            let v = draw(&spec(&ty, &b, row));
            let V::Dec { units, scale: 2 } = v else {
                panic!("{v:?}")
            };
            assert!(units.abs() <= 99_999);
        }
    }

    #[test]
    fn text_respects_lengths_and_patterns() {
        let ty = Type::scalar(Base::Varchar(Some(5)));
        let b = Bounds::default();
        for row in 0..100 {
            let v = draw(&spec(&ty, &b, row));
            assert!(v.as_str().unwrap().chars().count() <= 5);
        }
        let ty = Type::scalar(Base::Text);
        let b = Bounds {
            patterns: vec![Pattern::regex("^[A-Z0-9-]{4,32}$", false)],
            ..Bounds::default()
        };
        for row in 0..100 {
            let v = draw(&spec(&ty, &b, row));
            assert!(b.admits(&v), "{v:?}");
        }
    }

    #[test]
    fn unique_values_never_repeat() {
        let b = Bounds::default();
        for base in [
            Base::Int2,
            Base::Text,
            Base::Varchar(Some(6)),
            Base::Char(3),
            Base::Uuid,
            Base::Date,
            Base::Timestamp {
                tz: true,
                precision: None,
            },
            Base::Inet,
        ] {
            let ty = Type::scalar(base.clone());
            let mut seen = HashSet::new();
            for row in 0..1000 {
                let s = Spec {
                    unique: true,
                    ..spec(&ty, &b, row)
                };
                let v = draw(&s).text(true).unwrap();
                assert!(seen.insert(v.clone()), "{base:?} repeated {v}");
            }
        }
    }

    #[test]
    fn numbered_text_keeps_the_number() {
        assert_eq!(
            numbered("rhoda.wilkinson@example.com", 16, None),
            "rhoda.wilkinson17@example.com"
        );
        assert_eq!(numbered("atlas-labs", 0, None), "atlas-labs-1");
        assert_eq!(numbered("Atlas Labs", 9, None), "Atlas Labs 10");
        assert_eq!(numbered("Engineering", 1, None), "Engineering 2");
        assert_eq!(numbered("rhoda", 1, None), "rhoda2");
        assert_eq!(numbered("CA", 1, None), "CA-2");
        assert_eq!(numbered("abcdefgh", 41, Some(6)), "abc42");
        // Cut to end on a digit: a separator keeps it apart from `ab` + 15.
        assert_eq!(numbered("ab1c", 4, Some(5)), "ab1-5");
        assert_ne!(numbered("ab1c", 4, Some(5)), numbered("ab", 14, Some(5)));
        assert_eq!(numbered("x", 123_456, Some(3)), "457");
        assert_eq!(with_row_digits("AB12", 16).as_deref(), Some("AB17"));
        assert_eq!(
            with_row_digits("SKU-0042-X", 6).as_deref(),
            Some("SKU-0007-X")
        );
        assert_eq!(with_row_digits("abc", 6), None);
    }

    #[test]
    fn vectors_have_their_dimensions_and_unit_length() {
        let ty = Type::scalar(Base::Vector {
            kind: VectorKind::Vector,
            dims: Some(16),
        });
        let b = Bounds::default();
        let v = draw(&spec(&ty, &b, 3));
        let s = v.as_str().unwrap();
        let parts: Vec<f64> = s
            .trim_matches(['[', ']'])
            .split(',')
            .map(|x| x.parse().unwrap())
            .collect();
        assert_eq!(parts.len(), 16);
        let norm: f64 = parts.iter().map(|x| x * x).sum::<f64>().sqrt();
        assert!((norm - 1.0).abs() < 1e-3, "{norm}");
    }

    #[test]
    fn geometry_follows_the_typmod() {
        let mut r = rng::cell(&rng::key(1, &["g"]), 0, 0);
        assert!(
            geometry(&mut r, false, Some("point"), Some(4326), None)
                .starts_with("SRID=4326;POINT(")
        );
        assert!(
            geometry(&mut r, true, Some("polygon"), Some(4326), None)
                .starts_with("SRID=4326;POLYGON((")
        );
        assert!(
            geometry(&mut r, false, Some("linestringz"), None, None).starts_with("LINESTRING Z(")
        );
    }

    /// Every type's text form, for many rows, has the shape its input
    /// function reads.
    #[test]
    #[allow(clippy::too_many_lines)]
    fn every_type_prints_what_postgres_reads() {
        use crate::types::{RangeKind, VectorKind};
        let check = |base: Base, ok: &dyn Fn(&str) -> bool| {
            let ty = Type::scalar(base.clone());
            let b = Bounds::default();
            for row in 0..200 {
                for profile in [Profile::Realistic, Profile::EdgeCases] {
                    let s = Spec {
                        profile,
                        ..spec(&ty, &b, row)
                    };
                    let v = draw(&s);
                    if let Some(text) = v.text(true) {
                        assert!(ok(&text), "{base:?}: {text:?}");
                    }
                }
            }
        };
        let re = |p: &str| regex::Regex::new(p).unwrap();
        let int = re(r"^-?\d+$");
        check(Base::Int2, &|t| int.is_match(t) && t.parse::<i16>().is_ok());
        check(Base::Int4, &|t| t.parse::<i32>().is_ok());
        check(Base::Int8, &|t| t.parse::<i64>().is_ok());
        check(Base::Float8, &|t| t.parse::<f64>().is_ok() || t == "NaN");
        let numeric = re(r"^-?\d{1,8}\.\d{2}$");
        check(
            Base::Numeric {
                precision: Some(10),
                scale: 2,
            },
            &|t| numeric.is_match(t),
        );
        check(Base::Money, &|t| re(r"^-?\d+\.\d{2}$").is_match(t));
        check(Base::Varchar(Some(12)), &|t| t.chars().count() <= 12);
        check(Base::Char(3), &|t| t.chars().count() <= 3);
        check(Base::Bool, &|t| t == "t" || t == "f");
        let uuid = re(r"^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$");
        check(Base::Uuid, &|t| uuid.is_match(t));
        let date = re(r"^(\d{4}-\d{2}-\d{2}|-?infinity)$");
        check(Base::Date, &|t| date.is_match(t));
        let ts = re(r"^(\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2}(\.\d{6})?\+00|-?infinity)$");
        check(
            Base::Timestamp {
                tz: true,
                precision: None,
            },
            &|t| ts.is_match(t),
        );
        check(Base::Time { tz: false }, &|t| {
            re(r"^\d{2}:\d{2}:\d{2}(\+00)?$").is_match(t)
        });
        check(Base::Interval, &|t| {
            re(r"^\d+ days( \d{2}:\d{2}:\d{2})?$").is_match(t)
        });
        check(Base::Bytea, &|t| re(r"^\\x([0-9a-f]{2})*$").is_match(t));
        let ip = re(r"^(192\.0\.2|198\.51\.100|203\.0\.113)\.\d+$|^2001:db8::[0-9a-f]+$");
        check(Base::Inet, &|t| ip.is_match(t));
        check(Base::Cidr, &|t| t.contains('/'));
        check(Base::Macaddr, &|t| {
            re(r"^([0-9a-f]{2}:){5}[0-9a-f]{2}$").is_match(t)
        });
        check(Base::Macaddr8, &|t| {
            re(r"^([0-9a-f]{2}:){7}[0-9a-f]{2}$").is_match(t)
        });
        check(Base::Jsonb, &|t| {
            serde_json::from_str::<serde_json::Value>(t).is_ok()
        });
        check(Base::Xml, &|t| t.starts_with('<') && t.ends_with('>'));
        check(Base::Bit(8), &|t| re(r"^[01]{8}$").is_match(t));
        check(Base::VarBit(Some(5)), &|t| re(r"^[01]{1,5}$").is_match(t));
        check(Base::TsVector, &|t| re(r"^[a-z]+( [a-z]+)*$").is_match(t));
        check(Base::Point, &|t| re(r"^\(-?[\d.]+,-?[\d.]+\)$").is_match(t));
        check(Base::Polygon, &|t| t.starts_with("((") && t.ends_with("))"));
        check(Base::Circle, &|t| t.starts_with("<(") && t.ends_with('>'));
        check(Base::Hstore, &|t| {
            re(r#"^"[a-z]+\d"=>"[a-z]+"(, "[a-z]+\d"=>"[a-z]+")*$"#).is_match(t)
        });
        check(Base::Ltree, &|t| re(r"^top(\.[a-z]+)+$").is_match(t));
        check(
            Base::Geometry {
                geography: false,
                shape: Some("multipolygon".into()),
                srid: Some(3857),
            },
            &|t| t.starts_with("SRID=3857;MULTIPOLYGON(((") && t.ends_with(")))"),
        );
        check(
            Base::Vector {
                kind: VectorKind::HalfVec,
                dims: Some(8),
            },
            &|t| t.starts_with('[') && t.split(',').count() == 8,
        );
        check(
            Base::Vector {
                kind: VectorKind::SparseVec,
                dims: Some(100),
            },
            &|t| re(r"^\{(\d+:[\d.]+,?)+\}/100$").is_match(t),
        );
        check(Base::Range(RangeKind::Int4), &|t| {
            re(r"^(\[\d+,\d+\)|empty|\(,\))$").is_match(t)
        });
        check(Base::Range(RangeKind::TsTz), &|t| {
            t == "empty" || t == "(,)" || (t.starts_with("[\"") && t.ends_with("\")"))
        });
        check(Base::Multirange(RangeKind::Date), &|t| {
            re(r"^\{\[[\d-]+,[\d-]+\),\[[\d-]+,[\d-]+\)\}$").is_match(t)
        });
        check(
            Base::Enum {
                name: "mood".into(),
                labels: vec!["sad".into(), "ok".into()],
            },
            &|t| t == "sad" || t == "ok",
        );
        check(
            Base::Composite {
                name: "pair".into(),
                attributes: vec![
                    ("label".into(), Type::scalar(Base::Text)),
                    ("n".into(), Type::scalar(Base::Int4)),
                ],
            },
            &|t| t.starts_with('(') && t.ends_with(')'),
        );
    }

    #[test]
    fn arrays_hold_values_of_their_element() {
        let ty = Type {
            array: true,
            ..Type::scalar(Base::Int4)
        };
        let b = Bounds::default();
        let shape = regex::Regex::new(r"^\{(-?\d+(,-?\d+)*)?\}$").unwrap();
        for row in 0..100 {
            let v = draw(&spec(&ty, &b, row));
            let text = v.text(true).unwrap();
            assert!(shape.is_match(&text), "{text}");
        }
    }

    #[test]
    fn edge_cases_reach_the_extremes() {
        let ty = Type::scalar(Base::Varchar(Some(40)));
        let b = Bounds::default();
        let mut longest = 0;
        let mut empty = false;
        for row in 0..400 {
            let s = Spec {
                profile: Profile::EdgeCases,
                ..spec(&ty, &b, row)
            };
            let text = draw(&s).text(true).unwrap_or_default();
            longest = longest.max(text.chars().count());
            empty |= text.is_empty();
        }
        assert_eq!(longest, 40);
        assert!(empty);
    }
}
