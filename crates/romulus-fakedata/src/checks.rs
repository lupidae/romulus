//! CHECK constraints: the shapes generated to pass (bounds, `IN`, lengths,
//! regexes, `a < b`, `num_nonnulls`), and an evaluator for the rest of what
//! it can read, so a row breaking a CHECK is drawn again before Postgres
//! ever sees it. What neither covers is left to the load's
//! `WHERE (check) IS NOT FALSE` (docs/playground.md, "CHECKs").

use std::{cmp::Ordering, collections::HashMap};

use regex_syntax::hir::{
    Class, ClassBytes, ClassBytesRange, ClassUnicode, ClassUnicodeRange, Hir, HirKind,
};

use crate::{
    expr::{Expr, Lit},
    types::{Base, Type},
    value::{V, parse_date, parse_decimal, parse_timestamp},
};

/// A text pattern a value must (or must not) match.
#[derive(Debug, Clone)]
pub struct Pattern {
    pub source: String,
    pub case_insensitive: bool,
    /// Postgres's regex, read by the `regex` crate; `None` when it uses
    /// what that crate does not have (backreferences, lookaround).
    pub matcher: Option<regex::Regex>,
    /// A generator of matching strings, for anchored patterns.
    pub sampler: Option<rand_regex::Regex>,
}

impl Pattern {
    pub fn regex(source: &str, case_insensitive: bool) -> Self {
        let translated = translate_regex(source);
        let flagged = if case_insensitive {
            format!("(?i){translated}")
        } else {
            translated.clone()
        };
        let matcher = regex::Regex::new(&flagged).ok();
        // Sampling needs the whole value to be the pattern.
        let sampler = (translated.starts_with('^') && translated.ends_with('$'))
            .then(|| {
                let body = &translated[1..translated.len() - 1];
                let body = if case_insensitive {
                    format!("(?i){body}")
                } else {
                    body.to_owned()
                };
                sampler(&body)
            })
            .flatten();
        Self {
            source: source.to_owned(),
            case_insensitive,
            matcher,
            sampler,
        }
    }

    /// `LIKE` patterns become anchored regexes.
    pub fn like(source: &str, case_insensitive: bool) -> Self {
        let mut re = String::from("^");
        let mut chars = source.chars();
        while let Some(c) = chars.next() {
            match c {
                '%' => re.push_str(".*"),
                '_' => re.push('.'),
                '\\' => {
                    if let Some(next) = chars.next() {
                        re.push_str(&regex::escape(&next.to_string()));
                    }
                }
                other => re.push_str(&regex::escape(&other.to_string())),
            }
        }
        re.push('$');
        let mut p = Self::regex(&re, case_insensitive);
        source.clone_into(&mut p.source);
        p
    }

    pub fn matches(&self, s: &str) -> Option<bool> {
        self.matcher.as_ref().map(|m| m.is_match(s))
    }
}

/// A sampler of strings matching `body`, its classes kept to printable
/// characters: `.` and `[^@]` would otherwise draw from all of Unicode,
/// NUL and control characters included, and Postgres refuses NUL in text
/// (a `COPY` aborts on it).
fn sampler(body: &str) -> Option<rand_regex::Regex> {
    let hir = regex_syntax::ParserBuilder::new()
        .build()
        .parse(body)
        .ok()?;
    rand_regex::Regex::with_hir(printable(hir), 12).ok()
}

fn printable(hir: Hir) -> Hir {
    match hir.into_kind() {
        HirKind::Class(Class::Unicode(class)) => {
            let mut ascii = ClassUnicode::new([ClassUnicodeRange::new(' ', '~')]);
            ascii.intersect(&class);
            if !ascii.ranges().is_empty() {
                return Hir::class(Class::Unicode(ascii));
            }
            let mut rest = class;
            rest.difference(&ClassUnicode::new([
                ClassUnicodeRange::new('\0', '\u{1f}'),
                ClassUnicodeRange::new('\u{7f}', '\u{9f}'),
            ]));
            Hir::class(Class::Unicode(rest))
        }
        HirKind::Class(Class::Bytes(class)) => {
            let mut ascii = ClassBytes::new([ClassBytesRange::new(b' ', b'~')]);
            ascii.intersect(&class);
            Hir::class(Class::Bytes(ascii))
        }
        HirKind::Repetition(mut r) => {
            r.sub = Box::new(printable(*r.sub));
            Hir::repetition(r)
        }
        HirKind::Capture(mut c) => {
            c.sub = Box::new(printable(*c.sub));
            Hir::capture(c)
        }
        HirKind::Concat(items) => Hir::concat(items.into_iter().map(printable).collect()),
        HirKind::Alternation(items) => Hir::alternation(items.into_iter().map(printable).collect()),
        HirKind::Literal(lit) => Hir::literal(lit.0),
        HirKind::Look(look) => Hir::look(look),
        HirKind::Empty => Hir::empty(),
    }
}

/// Postgres ARE spellings the `regex` crate writes otherwise.
fn translate_regex(source: &str) -> String {
    source
        .replace("[[:<:]]", r"\b")
        .replace("[[:>:]]", r"\b")
        .replace(r"\m", r"\b")
        .replace(r"\M", r"\b")
        .replace(r"\y", r"\b")
        .replace(r"\Y", r"\B")
        .replace(r"\Z", r"\z")
}

/// What one column must satisfy, gathered from every CHECK that reads it
/// alone.
#[derive(Debug, Clone, Default)]
pub struct Bounds {
    /// (value, inclusive).
    pub lower: Option<(V, bool)>,
    pub upper: Option<(V, bool)>,
    pub allowed: Option<Vec<V>>,
    pub excluded: Vec<V>,
    pub min_len: Option<usize>,
    pub max_len: Option<usize>,
    pub patterns: Vec<Pattern>,
    pub anti_patterns: Vec<Pattern>,
    pub not_null: bool,
    /// An `OR` is easiest kept by this column not being NULL
    /// (`status = 'cart' OR placed_at IS NOT NULL`).
    pub prefer_not_null: bool,
    /// The names of the CHECKs that shaped these bounds (the report).
    pub shaped_by: Vec<String>,
}

impl Bounds {
    fn tighten_lower(&mut self, v: V, inclusive: bool) {
        let replace = match &self.lower {
            None => true,
            Some((cur, cur_inc)) => match v.compare(cur) {
                Some(Ordering::Greater) => true,
                Some(Ordering::Equal) => *cur_inc && !inclusive,
                _ => false,
            },
        };
        if replace {
            self.lower = Some((v, inclusive));
        }
    }

    fn tighten_upper(&mut self, v: V, inclusive: bool) {
        let replace = match &self.upper {
            None => true,
            Some((cur, cur_inc)) => match v.compare(cur) {
                Some(Ordering::Less) => true,
                Some(Ordering::Equal) => *cur_inc && !inclusive,
                _ => false,
            },
        };
        if replace {
            self.upper = Some((v, inclusive));
        }
    }

    fn allow(&mut self, values: Vec<V>) {
        self.allowed = Some(match self.allowed.take() {
            None => values,
            Some(current) => current.into_iter().filter(|v| values.contains(v)).collect(),
        });
    }

    /// Whether `v` keeps the value-level bounds (patterns aside).
    pub fn admits(&self, v: &V) -> bool {
        if v.is_null() {
            return !self.not_null;
        }
        // An array's bounds are its elements' (a domain's CHECK, per value).
        if let V::Array(items) = v {
            return items.iter().all(|x| x.is_null() || self.admits(x));
        }
        if let Some(allowed) = &self.allowed
            && !allowed
                .iter()
                .any(|a| a.compare(v) == Some(Ordering::Equal) || a == v)
        {
            return false;
        }
        if self
            .excluded
            .iter()
            .any(|x| x.compare(v) == Some(Ordering::Equal) || x == v)
        {
            return false;
        }
        if let Some((lo, inc)) = &self.lower {
            match v.compare(lo) {
                Some(Ordering::Less) => return false,
                Some(Ordering::Equal) if !inc => return false,
                _ => {}
            }
        }
        if let Some((hi, inc)) = &self.upper {
            match v.compare(hi) {
                Some(Ordering::Greater) => return false,
                Some(Ordering::Equal) if !inc => return false,
                _ => {}
            }
        }
        if let Some(s) = v.as_str() {
            let n = s.chars().count();
            if self.min_len.is_some_and(|m| n < m) || self.max_len.is_some_and(|m| n > m) {
                return false;
            }
            if self.patterns.iter().any(|p| p.matches(s) == Some(false))
                || self
                    .anti_patterns
                    .iter()
                    .any(|p| p.matches(s) == Some(true))
            {
                return false;
            }
        }
        true
    }
}

/// The bounds a domain's CHECKs put on its values.
pub fn domain_bounds(ty: &Type) -> Bounds {
    let mut out = Bounds::default();
    let Some(domain) = &ty.domain else {
        return out;
    };
    let scalar = Type {
        array: false,
        ..ty.clone()
    };
    let name = domain.names.last().cloned().unwrap_or_default();
    for (_, parsed) in &domain.checks {
        if let Some(e) = parsed {
            let s = shapes(e, &name, Some("value"), &|_| Some(scalar.clone()));
            for (_, b) in s.bounds {
                merge(&mut out, b);
            }
        }
    }
    out
}

/// A comparison between two columns: `left op right`.
#[derive(Debug, Clone, PartialEq)]
pub struct Order {
    pub left: String,
    pub op: String,
    pub right: String,
}

/// `num_nonnulls(cols) op n` (or `num_nulls`, turned around).
#[derive(Debug, Clone, PartialEq)]
pub struct NonNulls {
    pub columns: Vec<String>,
    /// How many may be non-null: inclusive range.
    pub min: usize,
    pub max: usize,
}

/// What reading one CHECK gave.
#[derive(Debug, Clone, Default)]
pub struct Shapes {
    /// Bounds per column.
    pub bounds: HashMap<String, Bounds>,
    pub orders: Vec<Order>,
    pub non_nulls: Vec<NonNulls>,
    /// Whether every conjunct was one of the shapes above (then the
    /// generator satisfies the CHECK by construction).
    pub complete: bool,
}

/// Reads the shapes of a CHECK's expression. `value_column` is the column a
/// domain's `VALUE` stands for. `types` gives each column's type, to read
/// literals as the column's values.
pub fn shapes(
    expr: &Expr,
    name: &str,
    value_column: Option<&str>,
    types: &dyn Fn(&str) -> Option<Type>,
) -> Shapes {
    let mut out = Shapes {
        complete: true,
        ..Shapes::default()
    };
    for conjunct in expr.conjuncts() {
        if !conjunct_shape(conjunct, name, value_column, types, &mut out) {
            out.complete = false;
        }
    }
    out
}

fn column_of(e: &Expr, value_column: Option<&str>) -> Option<String> {
    match e.uncast() {
        Expr::Col(c) => Some(c.clone()),
        Expr::Value => value_column.map(str::to_owned),
        _ => None,
    }
}

/// A `length(col)`-like call: the column.
fn length_of(e: &Expr, value_column: Option<&str>) -> Option<String> {
    match e.uncast() {
        Expr::Func(name, args) if args.len() == 1 => {
            let bare = name.rsplit('.').next().unwrap_or(name);
            matches!(
                bare,
                "length" | "char_length" | "character_length" | "octet_length"
            )
            .then(|| column_of(&args[0], value_column))
            .flatten()
        }
        _ => None,
    }
}

fn flip(op: &str) -> &str {
    match op {
        "<" => ">",
        "<=" => ">=",
        ">" => "<",
        ">=" => "<=",
        other => other,
    }
}

#[allow(clippy::too_many_lines)]
fn conjunct_shape(
    e: &Expr,
    name: &str,
    value_column: Option<&str>,
    types: &dyn Fn(&str) -> Option<Type>,
    out: &mut Shapes,
) -> bool {
    match e {
        Expr::IsNull(inner, true) => {
            if let Some(col) = column_of(inner, value_column) {
                bound(out, &col, name).not_null = true;
                return true;
            }
            false
        }
        Expr::Bin(op, left, right)
            if matches!(op.as_str(), "<" | "<=" | ">" | ">=" | "=" | "<>" | "!=") =>
        {
            // length(col) op n
            if let (Some(col), Some(n)) = (length_of(left, value_column), literal_int(right)) {
                return length_shape(bound(out, &col, name), op, n);
            }
            if let (Some(n), Some(col)) = (literal_int(left), length_of(right, value_column)) {
                return length_shape(bound(out, &col, name), flip(op), n);
            }
            // num_nonnulls(…) op n
            if let Some(shape) = non_nulls(left, op, right, value_column) {
                out.non_nulls.push(shape);
                return true;
            }
            match (
                column_of(left, value_column),
                column_of(right, value_column),
            ) {
                (Some(a), Some(b)) => {
                    if matches!(op.as_str(), "<" | "<=" | ">" | ">=") {
                        out.orders.push(Order {
                            left: a,
                            op: op.clone(),
                            right: b,
                        });
                        return true;
                    }
                    false
                }
                (Some(col), None) => {
                    let ty = types(&col);
                    match literal(right, ty.as_ref()) {
                        Some(v) => value_shape(bound(out, &col, name), op, v),
                        None => false,
                    }
                }
                (None, Some(col)) => {
                    let ty = types(&col);
                    match literal(left, ty.as_ref()) {
                        Some(v) => value_shape(bound(out, &col, name), flip(op), v),
                        None => false,
                    }
                }
                (None, None) => false,
            }
        }
        Expr::Between(inner, lo, hi, false) => {
            let Some(col) = column_of(inner, value_column) else {
                return false;
            };
            let ty = types(&col);
            match (literal(lo, ty.as_ref()), literal(hi, ty.as_ref())) {
                (Some(lo), Some(hi)) => {
                    let b = bound(out, &col, name);
                    b.tighten_lower(lo, true);
                    b.tighten_upper(hi, true);
                    true
                }
                _ => false,
            }
        }
        Expr::Quantified {
            op,
            all,
            left,
            right,
        } => {
            let Some(col) = column_of(left, value_column) else {
                return false;
            };
            let ty = types(&col);
            let Some(values) = literal_list(right, ty.as_ref()) else {
                return false;
            };
            match (op.as_str(), all) {
                ("=", false) => {
                    bound(out, &col, name).allow(values);
                    true
                }
                ("<>" | "!=", true) => {
                    bound(out, &col, name).excluded.extend(values);
                    true
                }
                _ => false,
            }
        }
        Expr::In(inner, list, negated) => {
            let Some(col) = column_of(inner, value_column) else {
                return false;
            };
            let ty = types(&col);
            let Some(values) = list
                .iter()
                .map(|x| literal(x, ty.as_ref()))
                .collect::<Option<Vec<_>>>()
            else {
                return false;
            };
            if *negated {
                bound(out, &col, name).excluded.extend(values);
            } else {
                bound(out, &col, name).allow(values);
            }
            true
        }
        Expr::Not(inner) => match &**inner {
            Expr::Quantified {
                op,
                all: false,
                left,
                right,
            } if op == "=" => {
                let Some(col) = column_of(left, value_column) else {
                    return false;
                };
                let ty = types(&col);
                match literal_list(right, ty.as_ref()) {
                    Some(values) => {
                        bound(out, &col, name).excluded.extend(values);
                        true
                    }
                    None => false,
                }
            }
            _ => false,
        },
        Expr::Bin(op, left, right)
            if matches!(
                op.as_str(),
                "~" | "~*" | "!~" | "!~*" | "~~" | "~~*" | "!~~" | "!~~*"
            ) =>
        {
            let Some(col) = column_of(left, value_column) else {
                return false;
            };
            let Some(V::Text(source)) = literal(right, None) else {
                return false;
            };
            let ci = op.contains('*');
            let negated = op.starts_with('!');
            let pattern = if op.contains("~~") {
                Pattern::like(&source, ci)
            } else {
                Pattern::regex(&source, ci)
            };
            let usable = pattern.matcher.is_some();
            let b = bound(out, &col, name);
            if negated {
                b.anti_patterns.push(pattern);
            } else {
                b.patterns.push(pattern);
            }
            usable
        }
        Expr::Or(branches) => {
            // `col IS NULL OR <shape of col>`: the shape holds when not NULL.
            let nullable: Vec<String> = branches
                .iter()
                .filter_map(|b| match b {
                    Expr::IsNull(inner, false) => column_of(inner, value_column),
                    _ => None,
                })
                .collect();
            let rest: Vec<&Expr> = branches
                .iter()
                .filter(|b| !matches!(b, Expr::IsNull(_, false)))
                .collect();
            if nullable.len() == 1 && rest.len() == 1 {
                let mut inner = Shapes {
                    complete: true,
                    ..Shapes::default()
                };
                let ok = conjunct_shape(rest[0], name, value_column, types, &mut inner);
                let only_that_column = inner.bounds.keys().all(|k| *k == nullable[0])
                    && inner.orders.is_empty()
                    && inner.non_nulls.is_empty();
                if ok && only_that_column {
                    for (col, b) in inner.bounds {
                        merge(bound(out, &col, name), b);
                    }
                    return true;
                }
            }
            // Easiest kept by a column not being NULL.
            for b in branches {
                if let Expr::IsNull(inner, true) = b
                    && let Some(col) = column_of(inner, value_column)
                {
                    bound(out, &col, name).prefer_not_null = true;
                }
            }
            false
        }
        Expr::Lit(Lit::Bool(true)) => true,
        _ => false,
    }
}

fn bound<'a>(out: &'a mut Shapes, col: &str, name: &str) -> &'a mut Bounds {
    let b = out.bounds.entry(col.to_owned()).or_default();
    if !b.shaped_by.iter().any(|n| n == name) {
        b.shaped_by.push(name.to_owned());
    }
    b
}

/// Folds `from` into `into`: the tighter of each bound.
pub fn merge(into: &mut Bounds, from: Bounds) {
    if let Some((v, inc)) = from.lower {
        into.tighten_lower(v, inc);
    }
    if let Some((v, inc)) = from.upper {
        into.tighten_upper(v, inc);
    }
    if let Some(a) = from.allowed {
        into.allow(a);
    }
    into.excluded.extend(from.excluded);
    into.min_len = match (into.min_len, from.min_len) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (a, b) => a.or(b),
    };
    into.max_len = match (into.max_len, from.max_len) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    };
    into.patterns.extend(from.patterns);
    into.anti_patterns.extend(from.anti_patterns);
    into.not_null |= from.not_null;
    into.prefer_not_null |= from.prefer_not_null;
    for name in from.shaped_by {
        if !into.shaped_by.contains(&name) {
            into.shaped_by.push(name);
        }
    }
}

fn length_shape(b: &mut Bounds, op: &str, n: i128) -> bool {
    let Ok(n) = usize::try_from(n.max(0)) else {
        return false;
    };
    let max = |b: &mut Bounds, m: usize| b.max_len = Some(b.max_len.map_or(m, |c| c.min(m)));
    let min = |b: &mut Bounds, m: usize| b.min_len = Some(b.min_len.map_or(m, |c| c.max(m)));
    match op {
        "<" => max(b, n.saturating_sub(1)),
        "<=" => max(b, n),
        ">" => min(b, n + 1),
        ">=" => min(b, n),
        "=" => {
            min(b, n);
            max(b, n);
        }
        "<>" | "!=" if n == 0 => min(b, 1),
        _ => return false,
    }
    true
}

fn value_shape(b: &mut Bounds, op: &str, v: V) -> bool {
    match op {
        ">" => b.tighten_lower(v, false),
        ">=" => b.tighten_lower(v, true),
        "<" => b.tighten_upper(v, false),
        "<=" => b.tighten_upper(v, true),
        "=" => b.allow(vec![v]),
        "<>" | "!=" => {
            if v == V::Text(String::new()) {
                b.min_len = Some(b.min_len.unwrap_or(0).max(1));
            }
            b.excluded.push(v);
        }
        _ => return false,
    }
    true
}

fn non_nulls(left: &Expr, op: &str, right: &Expr, value_column: Option<&str>) -> Option<NonNulls> {
    let (call, op, n) = match (left.uncast(), literal_int(right)) {
        (Expr::Func(..), Some(n)) => (left.uncast(), op, n),
        _ => (right.uncast(), flip(op), literal_int(left)?),
    };
    let Expr::Func(name, args) = call else {
        return None;
    };
    let bare = name.rsplit('.').next().unwrap_or(name);
    let counts_nulls = match bare {
        "num_nonnulls" => false,
        "num_nulls" => true,
        _ => return None,
    };
    let columns = args
        .iter()
        .map(|a| column_of(a, value_column))
        .collect::<Option<Vec<_>>>()?;
    let total = columns.len();
    let n = usize::try_from(n).ok()?;
    let (mut min, mut max) = match op {
        "=" => (n, n),
        ">=" => (n, total),
        ">" => (n + 1, total),
        "<=" => (0, n),
        "<" => (0, n.checked_sub(1)?),
        _ => return None,
    };
    if counts_nulls {
        (min, max) = (total.saturating_sub(max), total.saturating_sub(min));
    }
    (min <= max && min <= total).then_some(NonNulls {
        columns,
        min,
        max: max.min(total),
    })
}

fn literal_int(e: &Expr) -> Option<i128> {
    match literal(e, None)? {
        V::Int(i) => Some(i),
        V::Dec { units, scale: 0 } => Some(units),
        _ => None,
    }
}

fn literal_list(e: &Expr, ty: Option<&Type>) -> Option<Vec<V>> {
    match e.uncast() {
        Expr::Array(items) => items.iter().map(|x| literal(x, ty)).collect(),
        // `'{a,b}'::text[]`
        Expr::Lit(Lit::Str(s)) => {
            let inner = s.strip_prefix('{')?.strip_suffix('}')?;
            inner
                .split(',')
                .map(|item| {
                    let item = item.trim().trim_matches('"');
                    typed_literal(item, ty.map(|t| &t.base), None)
                })
                .collect()
        }
        _ => None,
    }
}

/// A constant, as a value of `ty` (the cast's type when it has one).
pub fn literal(e: &Expr, ty: Option<&Type>) -> Option<V> {
    match e {
        Expr::Cast(inner, cast) => {
            let target = cast_base(cast);
            match &**inner {
                Expr::Lit(Lit::Str(s)) => {
                    typed_literal(s, target.as_ref().or(ty.map(|t| &t.base)), Some(cast))
                }
                Expr::Lit(Lit::Num(n)) => number(n, target.as_ref().or(ty.map(|t| &t.base))),
                Expr::Cast(..) | Expr::Neg(_) => {
                    let v = literal(inner, ty)?;
                    Some(match (target, v) {
                        (Some(Base::Float8), V::Int(i)) =>
                        {
                            #[allow(clippy::cast_precision_loss)]
                            V::Float(i as f64)
                        }
                        (Some(Base::Float8), V::Dec { .. }) => {
                            V::Float(literal(inner, ty)?.as_f64()?)
                        }
                        (_, v) => v,
                    })
                }
                _ => None,
            }
        }
        Expr::Lit(Lit::Str(s)) => typed_literal(s, ty.map(|t| &t.base), None),
        Expr::Lit(Lit::Num(n)) => number(n, ty.map(|t| &t.base)),
        Expr::Lit(Lit::Bool(b)) => Some(V::Bool(*b)),
        Expr::Neg(inner) => match literal(inner, ty)? {
            V::Int(i) => Some(V::Int(-i)),
            V::Float(f) => Some(V::Float(-f)),
            V::Dec { units, scale } => Some(V::Dec {
                units: -units,
                scale,
            }),
            _ => None,
        },
        _ => None,
    }
}

/// The base of a cast's type, for the common built-in names.
fn cast_base(cast: &str) -> Option<Base> {
    let bare = cast.trim_end_matches("[]");
    Some(match bare {
        "integer" | "bigint" | "smallint" | "int" | "int4" | "int8" | "int2" => Base::Int8,
        "numeric" | "decimal" => Base::Numeric {
            precision: None,
            scale: 6,
        },
        "double precision" | "real" | "float8" | "float4" => Base::Float8,
        "date" => Base::Date,
        "timestamp with time zone" | "timestamptz" => Base::Timestamp {
            tz: true,
            precision: None,
        },
        "timestamp without time zone" | "timestamp" => Base::Timestamp {
            tz: false,
            precision: None,
        },
        "boolean" | "bool" => Base::Bool,
        "text" | "character varying" | "varchar" | "bpchar" | "character" | "name" => Base::Text,
        _ if bare.starts_with("character varying(")
            || bare.starts_with("character(")
            || bare.starts_with("numeric(") =>
        {
            if bare.starts_with("numeric") {
                Base::Numeric {
                    precision: None,
                    scale: 6,
                }
            } else {
                Base::Text
            }
        }
        _ => return None,
    })
}

fn number(n: &str, base: Option<&Base>) -> Option<V> {
    if !n.contains(['.', 'e', 'E'])
        && let Ok(i) = n.parse::<i128>()
    {
        return Some(match base {
            Some(Base::Float4 | Base::Float8) => V::Float(i as f64),
            _ => V::Int(i),
        });
    }
    if n.contains(['e', 'E']) {
        return n.parse::<f64>().ok().map(V::Float);
    }
    let scale = u32::try_from(n.split_once('.').map_or(0, |(_, f)| f.len())).ok()?;
    match base {
        Some(Base::Float4 | Base::Float8) => n.parse::<f64>().ok().map(V::Float),
        _ => parse_decimal(n, scale).map(|units| V::Dec { units, scale }),
    }
}

fn typed_literal(s: &str, base: Option<&Base>, cast: Option<&str>) -> Option<V> {
    match base {
        Some(
            Base::Int2
            | Base::Int4
            | Base::Int8
            | Base::Numeric { .. }
            | Base::Float4
            | Base::Float8
            | Base::Money,
        ) => number(s.trim(), base),
        Some(Base::Date) => parse_date(s).map(V::Date),
        Some(Base::Timestamp { .. }) => parse_timestamp(s).map(V::Ts),
        Some(Base::Bool) => match s.trim().to_lowercase().as_str() {
            "t" | "true" | "yes" | "on" | "1" => Some(V::Bool(true)),
            "f" | "false" | "no" | "off" | "0" => Some(V::Bool(false)),
            _ => None,
        },
        // Text, enums and types this crate prints as text: the string.
        Some(
            Base::Text
            | Base::Varchar(_)
            | Base::Char(_)
            | Base::Citext
            | Base::Name
            | Base::Enum { .. }
            | Base::InternalChar,
        )
        | None => Some(V::Text(s.to_owned())),
        Some(_) => cast.is_none().then(|| V::Text(s.to_owned())),
    }
}

/// SQL's three values; `Unknown` also stands for "cannot tell", which a
/// CHECK treats like NULL: it passes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tri {
    True,
    False,
    Unknown,
}

impl Tri {
    fn from(b: bool) -> Self {
        if b { Tri::True } else { Tri::False }
    }

    fn not(self) -> Self {
        match self {
            Tri::True => Tri::False,
            Tri::False => Tri::True,
            Tri::Unknown => Tri::Unknown,
        }
    }
}

/// Evaluates CHECK expressions over a generated row.
pub struct Eval<'a> {
    /// A column's value (`None`: not generated here, e.g. left to Postgres).
    pub column: &'a dyn Fn(&str) -> Option<V>,
    /// `VALUE` in a domain's CHECK.
    pub value: Option<V>,
    /// Patterns compiled at plan time, by (source, case-insensitive, like).
    pub patterns: &'a HashMap<(String, bool, bool), Pattern>,
}

impl Eval<'_> {
    pub fn check(&self, e: &Expr) -> Tri {
        match e {
            Expr::And(list) => {
                let mut out = Tri::True;
                for x in list {
                    match self.check(x) {
                        Tri::False => return Tri::False,
                        Tri::Unknown => out = Tri::Unknown,
                        Tri::True => {}
                    }
                }
                out
            }
            Expr::Or(list) => {
                let mut out = Tri::False;
                for x in list {
                    match self.check(x) {
                        Tri::True => return Tri::True,
                        Tri::Unknown => out = Tri::Unknown,
                        Tri::False => {}
                    }
                }
                out
            }
            Expr::Not(inner) => self.check(inner).not(),
            Expr::IsNull(inner, negated) => match self.value(inner) {
                Some(v) => Tri::from(v.is_null() != *negated),
                None => Tri::Unknown,
            },
            Expr::IsBool(inner, value, negated) => {
                let t = self.check(inner);
                let is = match t {
                    Tri::Unknown => false,
                    Tri::True => *value,
                    Tri::False => !*value,
                };
                // IS [NOT] TRUE never yields NULL, but "cannot tell" stays so.
                if t == Tri::Unknown && self.value(inner).is_none() {
                    Tri::Unknown
                } else {
                    Tri::from(is != *negated)
                }
            }
            Expr::Bin(op, left, right) => self.binary(op, left, right),
            Expr::Between(inner, lo, hi, negated) => {
                let (Some(v), Some(lo), Some(hi)) =
                    (self.value(inner), self.value(lo), self.value(hi))
                else {
                    return Tri::Unknown;
                };
                let ge = cmp_tri(&v, &lo, |o| o != Ordering::Less);
                let le = cmp_tri(&v, &hi, |o| o != Ordering::Greater);
                let both = match (ge, le) {
                    (Tri::False, _) | (_, Tri::False) => Tri::False,
                    (Tri::True, Tri::True) => Tri::True,
                    _ => Tri::Unknown,
                };
                if *negated { both.not() } else { both }
            }
            Expr::In(inner, list, negated) => {
                let Some(v) = self.value(inner) else {
                    return Tri::Unknown;
                };
                let values: Option<Vec<V>> = list.iter().map(|x| self.value(x)).collect();
                let Some(values) = values else {
                    return Tri::Unknown;
                };
                let t = any_equal(&v, &values);
                if *negated { t.not() } else { t }
            }
            Expr::Quantified {
                op,
                all,
                left,
                right,
            } => {
                let Some(v) = self.value(left) else {
                    return Tri::Unknown;
                };
                let Some(V::Array(values)) = self.value(right) else {
                    return Tri::Unknown;
                };
                match (op.as_str(), all) {
                    ("=", false) => any_equal(&v, &values),
                    ("<>" | "!=", true) => any_equal(&v, &values).not(),
                    _ => Tri::Unknown,
                }
            }
            Expr::Lit(Lit::Bool(b)) => Tri::from(*b),
            Expr::Lit(Lit::Null) => Tri::Unknown,
            other => match self.value(other) {
                Some(V::Bool(b)) => Tri::from(b),
                _ => Tri::Unknown,
            },
        }
    }

    fn binary(&self, op: &str, left: &Expr, right: &Expr) -> Tri {
        match op {
            "=" | "<>" | "!=" | "<" | "<=" | ">" | ">=" => {
                // Enum order is not text order: only equality is judged.
                if matches!(op, "<" | "<=" | ">" | ">=")
                    && (custom_cast(left) || custom_cast(right))
                {
                    return Tri::Unknown;
                }
                let (Some(a), Some(b)) = (self.value(left), self.value(right)) else {
                    return Tri::Unknown;
                };
                if a.is_null() || b.is_null() {
                    return Tri::Unknown;
                }
                let Some(o) = a
                    .compare(&b)
                    .or_else(|| (a == b).then_some(Ordering::Equal))
                else {
                    return Tri::Unknown;
                };
                Tri::from(match op {
                    "=" => o == Ordering::Equal,
                    "<>" | "!=" => o != Ordering::Equal,
                    "<" => o == Ordering::Less,
                    "<=" => o != Ordering::Greater,
                    ">" => o == Ordering::Greater,
                    _ => o != Ordering::Less,
                })
            }
            "~" | "~*" | "!~" | "!~*" | "~~" | "~~*" | "!~~" | "!~~*" => {
                let Some(v) = self.value(left) else {
                    return Tri::Unknown;
                };
                let Some(s) = v.text(true) else {
                    return Tri::Unknown;
                };
                let Some(V::Text(source)) = literal(right, None) else {
                    return Tri::Unknown;
                };
                let key = (source, op.contains('*'), op.contains("~~"));
                let Some(pattern) = self.patterns.get(&key) else {
                    return Tri::Unknown;
                };
                match pattern.matches(&s) {
                    Some(m) => Tri::from(m != op.starts_with('!')),
                    None => Tri::Unknown,
                }
            }
            "is distinct from" | "is not distinct from" => {
                let (Some(a), Some(b)) = (self.value(left), self.value(right)) else {
                    return Tri::Unknown;
                };
                let same = match (a.is_null(), b.is_null()) {
                    (true, true) => true,
                    (true, false) | (false, true) => false,
                    _ => a.compare(&b) == Some(Ordering::Equal) || a == b,
                };
                Tri::from(same == (op == "is not distinct from"))
            }
            _ => Tri::Unknown,
        }
    }

    /// A value; `None` when it cannot tell.
    pub fn value(&self, e: &Expr) -> Option<V> {
        match e {
            Expr::Col(name) => (self.column)(name),
            Expr::Value => self.value.clone(),
            Expr::Lit(Lit::Null) => Some(V::Null),
            Expr::Lit(_) | Expr::Neg(_) => literal(e, None),
            Expr::Cast(inner, cast) => {
                if let Some(v) = literal(e, None) {
                    return Some(v);
                }
                let v = self.value(inner)?;
                match cast_base(cast) {
                    Some(Base::Text) => Some(match v {
                        V::Null => V::Null,
                        other => V::Text(other.text(true)?),
                    }),
                    Some(Base::Int8 | Base::Numeric { .. } | Base::Float8)
                        if v.as_f64().is_some() =>
                    {
                        Some(v)
                    }
                    Some(Base::Date | Base::Timestamp { .. })
                        if matches!(v, V::Date(_) | V::Ts(_)) =>
                    {
                        Some(v)
                    }
                    // A cast to the column's own type (enum, domain…).
                    _ if matches!(v, V::Text(_) | V::Null) && !cast.ends_with("[]") => Some(v),
                    _ => None,
                }
            }
            Expr::Array(items) => items
                .iter()
                .map(|x| self.value(x))
                .collect::<Option<Vec<_>>>()
                .map(V::Array),
            Expr::Func(name, args) => self.call(name, args),
            Expr::Bin(op, a, b) if matches!(op.as_str(), "+" | "-" | "*" | "/" | "%" | "||") => {
                let (a, b) = (self.value(a)?, self.value(b)?);
                if a.is_null() || b.is_null() {
                    return Some(V::Null);
                }
                arithmetic(op, &a, &b)
            }
            _ => None,
        }
    }

    fn call(&self, name: &str, args: &[Expr]) -> Option<V> {
        let bare = name.rsplit('.').next().unwrap_or(name);
        let values = || {
            args.iter()
                .map(|a| self.value(a))
                .collect::<Option<Vec<_>>>()
        };
        match bare {
            "length" | "char_length" | "character_length" | "octet_length" if args.len() == 1 => {
                let v = self.value(&args[0])?;
                if v.is_null() {
                    return Some(V::Null);
                }
                let s = v.text(true)?;
                Some(V::Int(if bare == "octet_length" {
                    s.len() as i128
                } else {
                    s.chars().count() as i128
                }))
            }
            "lower" | "upper" | "btrim" | "trim" | "ltrim" | "rtrim" if args.len() == 1 => {
                let v = self.value(&args[0])?;
                let Some(s) = v.text(true) else {
                    return Some(V::Null);
                };
                Some(V::Text(match bare {
                    "lower" => s.to_lowercase(),
                    "upper" => s.to_uppercase(),
                    "ltrim" => s.trim_start().to_owned(),
                    "rtrim" => s.trim_end().to_owned(),
                    _ => s.trim().to_owned(),
                }))
            }
            "num_nonnulls" | "num_nulls" => {
                let vs = values()?;
                let n = vs.iter().filter(|v| !v.is_null()).count();
                Some(V::Int(
                    if bare == "num_nulls" { vs.len() - n } else { n } as i128
                ))
            }
            "coalesce" => {
                let vs = values()?;
                Some(vs.into_iter().find(|v| !v.is_null()).unwrap_or(V::Null))
            }
            "round" | "trunc" | "floor" | "ceil" | "ceiling" if !args.is_empty() => {
                let v = self.value(&args[0])?;
                if v.is_null() {
                    return Some(V::Null);
                }
                let digits = match args.get(1) {
                    Some(d) => match self.value(d)? {
                        V::Int(n) => i32::try_from(n).ok()?,
                        _ => return None,
                    },
                    None => 0,
                };
                let f = v.as_f64()?;
                let scale = crate::rng::pow10(digits);
                let r = match bare {
                    "round" => (f * scale).round() / scale,
                    "trunc" => (f * scale).trunc() / scale,
                    "floor" => f.floor(),
                    _ => f.ceil(),
                };
                Some(V::Float(r))
            }
            "abs" if args.len() == 1 => match self.value(&args[0])? {
                V::Int(i) => Some(V::Int(i.abs())),
                V::Float(f) => Some(V::Float(f.abs())),
                V::Dec { units, scale } => Some(V::Dec {
                    units: units.abs(),
                    scale,
                }),
                V::Null => Some(V::Null),
                _ => None,
            },
            "cardinality" | "array_length" if !args.is_empty() => match self.value(&args[0])? {
                V::Array(items) => Some(V::Int(items.len() as i128)),
                V::Null => Some(V::Null),
                _ => None,
            },
            _ => None,
        }
    }
}

/// Functions the evaluator computes.
const KNOWN_FUNCTIONS: &[&str] = &[
    "length",
    "char_length",
    "character_length",
    "octet_length",
    "lower",
    "upper",
    "btrim",
    "trim",
    "ltrim",
    "rtrim",
    "num_nonnulls",
    "num_nulls",
    "coalesce",
    "abs",
    "cardinality",
    "array_length",
    "round",
    "trunc",
    "floor",
    "ceil",
    "ceiling",
];

/// Whether the evaluator can judge `e` (no call it does not compute).
pub fn evaluable(e: &Expr) -> bool {
    let mut ok = !e.has_unknown();
    e.walk(&mut |x| {
        if let Expr::Func(name, _) = x {
            let bare = name.rsplit('.').next().unwrap_or(name);
            ok &= KNOWN_FUNCTIONS.contains(&bare);
        }
    });
    ok
}

fn custom_cast(e: &Expr) -> bool {
    match e {
        Expr::Cast(_, cast) => cast_base(cast).is_none(),
        _ => false,
    }
}

fn cmp_tri(a: &V, b: &V, ok: impl Fn(Ordering) -> bool) -> Tri {
    if a.is_null() || b.is_null() {
        return Tri::Unknown;
    }
    match a.compare(b) {
        Some(o) => Tri::from(ok(o)),
        None => Tri::Unknown,
    }
}

fn any_equal(v: &V, values: &[V]) -> Tri {
    if v.is_null() {
        return Tri::Unknown;
    }
    let mut out = Tri::False;
    for x in values {
        if x.is_null() {
            out = Tri::Unknown;
        } else if x.compare(v) == Some(Ordering::Equal) || x == v {
            return Tri::True;
        }
    }
    out
}

fn arithmetic(op: &str, a: &V, b: &V) -> Option<V> {
    if op == "||" {
        return Some(V::Text(format!("{}{}", a.text(true)?, b.text(true)?)));
    }
    match (a, b) {
        (V::Int(x), V::Int(y)) => Some(V::Int(match op {
            "+" => x.checked_add(*y)?,
            "-" => x.checked_sub(*y)?,
            "*" => x.checked_mul(*y)?,
            "%" => x.checked_rem(*y)?,
            _ => x.checked_div(*y)?,
        })),
        _ if op == "%" => None,
        (V::Date(d), V::Int(n)) if op == "+" || op == "-" => {
            let n = i64::try_from(*n).ok()?;
            Some(V::Date(if op == "+" { d + n } else { d - n }))
        }
        (V::Date(x), V::Date(y)) if op == "-" => Some(V::Int(i128::from(x - y))),
        _ => {
            let (left, right) = (a.as_f64()?, b.as_f64()?);
            let r = match op {
                "+" => left + right,
                "-" => left - right,
                "*" => left * right,
                _ if right == 0.0 => return None,
                _ => left / right,
            };
            Some(V::Float(r))
        }
    }
}

/// Every pattern literal in `e`, compiled once.
pub fn collect_patterns(e: &Expr, into: &mut HashMap<(String, bool, bool), Pattern>) {
    e.walk(&mut |x| {
        if let Expr::Bin(op, _, right) = x
            && op.contains('~')
            && let Some(V::Text(source)) = literal(right, None)
        {
            let ci = op.contains('*');
            let like = op.contains("~~");
            into.entry((source.clone(), ci, like)).or_insert_with(|| {
                if like {
                    Pattern::like(&source, ci)
                } else {
                    Pattern::regex(&source, ci)
                }
            });
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::expr::parse_check;

    #[allow(clippy::unnecessary_wraps)]
    fn ty(base: Base) -> Option<Type> {
        Some(Type::scalar(base))
    }

    #[allow(clippy::unnecessary_wraps)]
    fn types(col: &str) -> Option<Type> {
        match col {
            "price" | "total" => ty(Base::Numeric {
                precision: Some(12),
                scale: 2,
            }),
            "reading" => ty(Base::Float8),
            "slug" | "code" | "username" | "email" => ty(Base::Text),
            "starts_on" => ty(Base::Date),
            _ => ty(Base::Int4),
        }
    }

    fn read(check: &str) -> Shapes {
        let e = parse_check(check, false).unwrap();
        shapes(&e, "c", None, &types)
    }

    #[test]
    fn bounds_and_between_become_a_range() {
        let s = read(
            "CHECK (((reading >= ('-1000'::integer)::double precision) AND (reading <= (1000)::double precision)))",
        );
        assert!(s.complete);
        let b = &s.bounds["reading"];
        assert_eq!(b.lower, Some((V::Float(-1000.0), true)));
        assert_eq!(b.upper, Some((V::Float(1000.0), true)));
        let s = read(r#"CHECK (("total" >= (0)::numeric))"#);
        assert_eq!(s.bounds["total"].lower, Some((V::Int(0), true)));
    }

    #[test]
    fn lists_lengths_and_patterns_are_read() {
        let s = read("CHECK ((code = ANY (ARRAY['a'::text, 'b'::text])))");
        assert_eq!(
            s.bounds["code"].allowed,
            Some(vec![V::Text("a".into()), V::Text("b".into())])
        );
        let s = read("CHECK ((char_length(username) >= 3))");
        assert_eq!(s.bounds["username"].min_len, Some(3));
        let s = read("CHECK ((slug ~ '^[a-z0-9-]{3,40}$'::text))");
        let p = &s.bounds["slug"].patterns[0];
        assert_eq!(p.matches("ok-slug"), Some(true));
        assert_eq!(p.matches("No"), Some(false));
        assert!(p.sampler.is_some());
        let s = read("CHECK (((email IS NULL) OR (email ~~ '%@%'::text)))");
        assert!(s.complete);
        assert_eq!(s.bounds["email"].patterns[0].matches("a@b"), Some(true));
    }

    #[test]
    fn column_orders_and_non_nulls_are_read() {
        let s = read("CHECK ((ends > starts))");
        assert_eq!(
            s.orders,
            [Order {
                left: "ends".into(),
                op: ">".into(),
                right: "starts".into()
            }]
        );
        let s = read("CHECK ((num_nonnulls(a, b, c) = 1))");
        assert_eq!(
            s.non_nulls,
            [NonNulls {
                columns: vec!["a".into(), "b".into(), "c".into()],
                min: 1,
                max: 1
            }]
        );
    }

    #[test]
    fn an_or_kept_by_a_not_null_says_so() {
        let s = read("CHECK (((status = 'cart'::public.order_status) OR (placed_at IS NOT NULL)))");
        assert!(!s.complete);
        assert!(s.bounds["placed_at"].prefer_not_null);
    }

    #[test]
    fn the_evaluator_judges_what_it_reads() {
        let e = parse_check(
            "CHECK (((state <> 'escalated'::public.ticket_state) OR (priority = 'high'::public.priority)))",
            false,
        )
        .unwrap();
        let patterns = HashMap::new();
        let row = |state: &'static str, priority: &'static str| {
            move |c: &str| match c {
                "state" => Some(V::Text(state.into())),
                "priority" => Some(V::Text(priority.into())),
                _ => None,
            }
        };
        let f = row("escalated", "low");
        let ev = Eval {
            column: &f,
            value: None,
            patterns: &patterns,
        };
        assert_eq!(ev.check(&e), Tri::False);
        let f = row("escalated", "high");
        let ev = Eval {
            column: &f,
            value: None,
            patterns: &patterns,
        };
        assert_eq!(ev.check(&e), Tri::True);
        let unknown = parse_check("CHECK (util.is_valid_slug(slug))", false).unwrap();
        assert_eq!(ev.check(&unknown), Tri::Unknown);
    }

    #[test]
    fn domain_checks_read_value_as_the_column() {
        let e = parse_check("CHECK ((VALUE >= 0))", true).unwrap();
        let s = shapes(&e, "cents", Some("price"), &types);
        assert_eq!(s.bounds["price"].lower, Some((V::Int(0), true)));
    }

    #[test]
    fn samplers_draw_printable_characters_only() {
        use rand::RngExt;
        for (pattern, ci) in [
            ("^A.*$", false),
            ("^[^@]+@[^@]+$", false),
            ("^x.{3}y$", true),
        ] {
            let p = Pattern::regex(pattern, ci);
            let sampler = p.sampler.as_ref().expect("a sampler");
            let mut rng = crate::rng::cell(&crate::rng::key(1, &[pattern]), 0, 0);
            for _ in 0..2000 {
                let s: String = rng.sample(sampler);
                assert!(s.chars().all(|c| !c.is_control()), "{s:?}");
                assert_eq!(p.matches(&s), Some(true), "{s:?}");
            }
        }
        let like = Pattern::like("A%", false);
        let mut rng = crate::rng::cell(&crate::rng::key(1, &["like"]), 0, 0);
        for _ in 0..2000 {
            let s: String = rng.sample(like.sampler.as_ref().unwrap());
            assert!(!s.contains('\0'));
        }
    }

    #[test]
    fn modulo_is_judged_on_integers() {
        let e = parse_check("CHECK (((id % 7) <> 0))", false).unwrap();
        let patterns = HashMap::new();
        for (id, expected) in [(14, Tri::False), (15, Tri::True)] {
            let f = move |c: &str| (c == "id").then_some(V::Int(id));
            let ev = Eval {
                column: &f,
                value: None,
                patterns: &patterns,
            };
            assert_eq!(ev.check(&e), expected);
        }
    }
}
