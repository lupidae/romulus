//! Rows, one at a time. A row's columns are computed on demand, so a foreign
//! key reads its parent's key at any row without generating the rows before
//! it, and a column ordered after another (`updated_at` after `created_at`)
//! reads the other first.

use std::{cmp::Ordering, sync::Arc};

use rand::RngExt;
use rand_chacha::ChaCha8Rng;

use crate::{
    checks::{Eval, Tri},
    generate::{self, Spec, conform},
    model::{ComboPart, Fill, FkMode, Model, Table},
    partition::KeyCons,
    rng::{cell, key, skewed},
    types::{Base, Type},
    value::{DAY, V},
};

/// Draws of a row broken by a CHECK before it is left to Postgres.
const ATTEMPTS: u32 = 8;
/// Foreign keys reading keys reading keys…: deeper is a modelling loop.
const MAX_DEPTH: u32 = 12;

/// A row being generated. The caches hold `None` until computed;
/// `Some(None)` is a NULL foreign key, or no partition.
#[allow(clippy::option_option)]
pub struct Row<'m> {
    m: &'m Model,
    t: &'m Table,
    ti: usize,
    index: u64,
    attempt: u32,
    depth: u32,
    values: Vec<Option<V>>,
    busy: Vec<bool>,
    fk_rows: Vec<Option<Option<u64>>>,
    fk_values: Vec<Option<Option<Vec<V>>>>,
    leaf: Option<Option<usize>>,
    groups: Vec<Option<Vec<bool>>>,
    combos: Vec<Option<Vec<u64>>>,
}

impl<'m> Row<'m> {
    pub fn new(m: &'m Model, ti: usize, row: u64, attempt: u32, depth: u32) -> Self {
        let t = &m.tables[ti];
        Self {
            m,
            t,
            ti,
            index: row,
            attempt,
            depth,
            values: vec![None; t.columns.len()],
            busy: vec![false; t.columns.len()],
            fk_rows: vec![None; t.fks.len()],
            fk_values: vec![None; t.fks.len()],
            leaf: None,
            groups: vec![None; t.non_nulls.len()],
            combos: vec![None; t.combos.len()],
        }
    }

    fn attempt_for(&self, stable: bool) -> u32 {
        if stable { 0 } else { self.attempt }
    }

    /// A column's value; `None` when Postgres fills it (not generated).
    pub fn get(&mut self, c: usize) -> Option<V> {
        if self.ensure(c) {
            self.values[c].clone()
        } else {
            None
        }
    }

    /// Computes a column's value in place; false when Postgres fills it.
    fn ensure(&mut self, c: usize) -> bool {
        if self.values[c].is_some() {
            return true;
        }
        if !self.t.columns[c].written() {
            return false;
        }
        let v = if self.busy[c] || self.depth > MAX_DEPTH {
            V::Null
        } else {
            self.busy[c] = true;
            let v = self.compute(c);
            self.busy[c] = false;
            v
        };
        self.values[c] = Some(v);
        true
    }

    fn compute(&mut self, c: usize) -> V {
        let col = &self.t.columns[c];
        match col.fill {
            Fill::Database(_) | Fill::Null => V::Null,
            Fill::Sequence { start } => V::Int(start + i128::from(self.index)),
            Fill::Foreign { fk, position } => self
                .fk_values(fk)
                .and_then(|vals| vals.get(position).cloned())
                .unwrap_or(V::Null),
            Fill::Partition => self.partition_value(c),
            Fill::Value => self.value(c),
        }
    }

    fn value(&mut self, c: usize) -> V {
        let t = self.t;
        let col = &t.columns[c];
        let mut rng = cell(&col.key, self.index, self.attempt_for(col.stable));
        // NULL, decided first: by a num_nonnulls group, or the column's share.
        let grouped = self.group_null(c);
        let null = match grouped {
            Some(n) => n,
            None => col.null_rate > 0.0 && rng.random_bool(col.null_rate.min(1.0)),
        };
        if null {
            return V::Null;
        }
        // A combination's digit.
        for (k, combo) in t.combos.iter().enumerate() {
            if let Some(p) = combo
                .parts
                .iter()
                .position(|p| matches!(p, ComboPart::Column { column, .. } if *column == c))
            {
                let digits = self.combo_digits(k);
                if let ComboPart::Column { values, .. } = &combo.parts[p] {
                    return values.get(digits[p] as usize).cloned().unwrap_or(V::Null);
                }
            }
        }
        // A constant default, for some rows (when no heuristic speaks).
        if let Some(d) = &col.default
            && col.heuristic.is_none()
            && !col.unique
            && col.bounds.admits(d)
        {
            let share = match self.m.options.profile {
                crate::Profile::Realistic => 0.5,
                crate::Profile::EdgeCases => 0.2,
            };
            if rng.random_bool(share) {
                return d.clone();
            }
        }
        // After another column: that value plus a step.
        if !col.unique && !col.stable {
            for &(earlier, strict) in &col.after {
                let Some(base) = self.get(earlier) else {
                    continue;
                };
                if base.is_null() {
                    continue;
                }
                if let Some(v) =
                    later(&base, strict, &col.ty, &mut rng).and_then(|v| conform(v, &col.ty))
                    && col.bounds.admits(&v)
                {
                    return v;
                }
            }
        }
        let spec = self.spec(c);
        generate::value(&spec, &mut rng)
    }

    fn spec(&self, c: usize) -> Spec<'m> {
        let t = self.t;
        let col = &t.columns[c];
        Spec {
            table: &t.name,
            ty: &col.ty,
            bounds: &col.bounds,
            heuristic: col.heuristic,
            profile: self.m.options.profile,
            unique: col.unique,
            disjoint: col.disjoint,
            cents: col.cents,
            row: self.index,
            rows: t.rows,
            anchor: self.m.anchor,
        }
    }

    /// Whether a `num_nonnulls` group makes this column NULL.
    fn group_null(&mut self, c: usize) -> Option<bool> {
        let g = self
            .t
            .non_nulls
            .iter()
            .position(|g| g.columns.contains(&c))?;
        let chosen = self.group(g);
        let i = self.t.non_nulls[g].columns.iter().position(|&x| x == c)?;
        Some(!chosen[i])
    }

    fn group(&mut self, g: usize) -> Vec<bool> {
        if let Some(chosen) = &self.groups[g] {
            return chosen.clone();
        }
        let group = &self.t.non_nulls[g];
        let mut rng = cell(&self.t.key, self.index, self.attempt);
        // A stream of its own for each group.
        for _ in 0..=g {
            let _: u64 = rng.random();
        }
        let n = group.columns.len();
        // Contradictory CHECKs (at least 2, at most 1) cannot all hold:
        // the lower count, and Postgres judges.
        let (lo, hi) = (group.min.min(n), group.max.min(n));
        let k = if lo <= hi {
            rng.random_range(lo..=hi)
        } else {
            hi
        };
        let mut idx: Vec<usize> = (0..n).collect();
        for i in 0..n {
            let j = rng.random_range(i..n);
            idx.swap(i, j);
        }
        let mut chosen = vec![false; n];
        for &i in idx.iter().take(k) {
            chosen[i] = true;
        }
        self.groups[g] = Some(chosen.clone());
        chosen
    }

    fn combo_digits(&mut self, k: usize) -> Vec<u64> {
        if let Some(d) = &self.combos[k] {
            return d.clone();
        }
        let combo = &self.t.combos[k];
        let mut code = combo.perm.apply(self.index);
        let mut digits = Vec::with_capacity(combo.parts.len());
        for part in &combo.parts {
            let radix = part.radix(&self.m.tables, &self.t.fks).max(1);
            digits.push(code % radix);
            code /= radix;
        }
        self.combos[k] = Some(digits.clone());
        digits
    }

    /// The parent row a foreign key points at, `None` for NULL.
    fn fk_row(&mut self, f: usize) -> Option<u64> {
        if let Some(r) = self.fk_rows[f] {
            return r;
        }
        let r = self.choose_fk_row(f);
        self.fk_rows[f] = Some(r);
        r
    }

    fn choose_fk_row(&mut self, f: usize) -> Option<u64> {
        let t = self.t;
        let fk = &t.fks[f];
        let parent = &self.m.tables[fk.parent?];
        let n = if parent.skip.is_some() {
            0
        } else {
            parent.rows
        };
        if n == 0 {
            return None;
        }
        // An exclusive arc (`num_nonnulls(a_id, b_id) = 1`) decides NULL.
        let decided = fk.columns.iter().find_map(|&c| self.group_null(c));
        if decided == Some(true) {
            return None;
        }
        let mut rng = cell(&fk.key, self.index, self.attempt_for(fk.stable));
        let null = fk.null_rate > 0.0 && rng.random_bool(fk.null_rate.min(1.0));
        if null && decided.is_none() {
            return None;
        }
        match fk.mode {
            FkMode::Skewed => Some(parent.perm.apply(skewed(&mut rng, n))),
            FkMode::OneToOne => Some(crate::rng::Permutation::new(n, &fk.key).apply(self.index)),
            FkMode::Tree => {
                if self.index == 0 {
                    if fk.nullable { None } else { Some(0) }
                } else {
                    Some(skewed(&mut rng, self.index.min(n)))
                }
            }
            FkMode::Chain => {
                if self.index == 0 {
                    if fk.nullable { None } else { Some(n - 1) }
                } else {
                    Some(self.index - 1)
                }
            }
            FkMode::Combo { combo, part } => {
                let digits = self.combo_digits(combo);
                digits.get(part).copied()
            }
        }
    }

    /// The parent's key values a foreign key copies.
    fn fk_values(&mut self, f: usize) -> Option<Vec<V>> {
        if let Some(v) = &self.fk_values[f] {
            return v.clone();
        }
        let out = self.fk_row(f).and_then(|j| {
            let fk = &self.t.fks[f];
            let p = fk.parent?;
            let mut parent = Row::new(self.m, p, j, 0, self.depth + 1);
            let vals: Vec<V> = fk
                .parent_columns
                .iter()
                .map(|&c| parent.get(c).unwrap_or(V::Null))
                .collect();
            if vals.iter().any(V::is_null) {
                return None;
            }
            // Columns of a self-reference's own row: its key values.
            let _ = self.ti;
            Some(vals)
        });
        self.fk_values[f] = Some(out.clone());
        out
    }

    fn leaf(&mut self) -> Option<usize> {
        if let Some(l) = self.leaf {
            return l;
        }
        let t = self.t;
        let l = if t.leaves.is_empty() || t.leaf_weights == 0 {
            None
        } else {
            let mut rng = cell(
                &key(self.m.options.seed, &[&t.schema, &t.name, "#partition"]),
                self.index,
                0,
            );
            let mut x = rng.random_range(0..t.leaf_weights);
            t.leaves.iter().position(|l| {
                if x < l.weight {
                    true
                } else {
                    x -= l.weight;
                    false
                }
            })
        };
        self.leaf = Some(l);
        l
    }

    fn partition_value(&mut self, c: usize) -> V {
        let t = self.t;
        let col = &t.columns[c];
        let mut rng = cell(&col.key, self.index, 0);
        let cons = self
            .leaf()
            .and_then(|l| {
                t.leaves[l]
                    .keys
                    .iter()
                    .rev()
                    .find(|(name, _)| *name == col.name)
            })
            .map(|(_, cons)| cons.clone());
        let Some(cons) = cons else {
            let spec = self.spec(c);
            return generate::value(&spec, &mut rng);
        };
        let v = match cons {
            KeyCons::OneOf(values) => {
                let usable: Vec<&V> = values
                    .iter()
                    .filter(|v| !(col.not_null && v.is_null()))
                    .collect();
                if usable.is_empty() {
                    V::Null
                } else {
                    usable[rng.random_range(0..usable.len())].clone()
                }
            }
            KeyCons::Range { lo, hi } => {
                between(lo.as_ref(), hi.as_ref(), &col.ty, &mut rng, self.m.anchor)
            }
            KeyCons::NoneOf(values) => {
                let spec = self.spec(c);
                let mut v = generate::value(&spec, &mut rng);
                let mut tries = 0;
                while values.iter().any(|x| same(x, &v)) && tries < 16 {
                    v = generate::value(&spec, &mut rng);
                    tries += 1;
                }
                if values.iter().any(|x| same(x, &v)) {
                    v = match v {
                        V::Text(s) => V::Text(format!("{s}-other")),
                        V::Int(i) => V::Int(
                            values
                                .iter()
                                .filter_map(|x| if let V::Int(j) = x { Some(*j) } else { None })
                                .max()
                                .unwrap_or(i)
                                + 1,
                        ),
                        other => other,
                    };
                }
                v
            }
            KeyCons::Outside(ranges) => {
                let lowest = ranges
                    .iter()
                    .filter_map(|(lo, _)| lo.clone())
                    .min_by(|a, b| a.compare(b).unwrap_or(Ordering::Equal));
                let highest = ranges
                    .iter()
                    .filter_map(|(_, hi)| hi.clone())
                    .max_by(|a, b| a.compare(b).unwrap_or(Ordering::Equal));
                let below = ranges.iter().all(|(lo, _)| lo.is_some());
                let above = ranges.iter().all(|(_, hi)| hi.is_some());
                match (below && lowest.is_some(), above && highest.is_some()) {
                    (true, true) if rng.random_bool(0.5) => {
                        before(lowest.as_ref(), &col.ty, &mut rng, self.m.anchor)
                    }
                    (true, _) => before(lowest.as_ref(), &col.ty, &mut rng, self.m.anchor),
                    (false, true) => {
                        between(highest.as_ref(), None, &col.ty, &mut rng, self.m.anchor)
                    }
                    (false, false) => V::Null,
                }
            }
        };
        conform(v, &col.ty).unwrap_or(V::Null)
    }

    /// Whether the row keeps every CHECK this crate can judge.
    fn passes(&mut self) -> bool {
        let t = self.t;
        let n = t.columns.len();
        for c in 0..n {
            self.ensure(c);
        }
        // Generated columns, computed as Postgres will: CHECKs read them.
        let mut generated: Vec<(usize, V)> = Vec::new();
        for (c, col) in t.columns.iter().enumerate() {
            if let Some(e) = &col.expression_expr {
                let written = &self.values;
                let lookup = |name: &str| t.column(name).and_then(|i| written[i].clone());
                let eval = Eval {
                    column: &lookup,
                    value: None,
                    patterns: &t.patterns,
                };
                if let Some(v) = eval.value(e) {
                    if !fits(&v, &col.ty) {
                        return false;
                    }
                    generated.push((c, v));
                }
            }
        }
        let values = &self.values;
        let value = |c: usize| -> Option<&V> {
            values[c]
                .as_ref()
                .or_else(|| generated.iter().find(|(g, _)| *g == c).map(|(_, v)| v))
        };
        for (c, col) in t.columns.iter().enumerate() {
            let Some(v) = value(c) else { continue };
            if matches!(col.fill, Fill::Value) && !v.is_null() && !col.bounds.admits(v) {
                return false;
            }
            if col.not_null && v.is_null() && col.written() {
                return false;
            }
            for check in &col.domain_checks {
                let lookup = |_: &str| None;
                let eval = Eval {
                    column: &lookup,
                    value: Some(v.clone()),
                    patterns: &t.patterns,
                };
                if !v.is_null() && eval.check(check) == Tri::False {
                    return false;
                }
            }
        }
        if t.checks.iter().all(|c| c.expr.is_none()) {
            return true;
        }
        let lookup = |name: &str| t.column(name).and_then(value).cloned();
        let eval = Eval {
            column: &lookup,
            value: None,
            patterns: &t.patterns,
        };
        t.checks
            .iter()
            .filter_map(|c| c.expr.as_ref())
            .all(|e| eval.check(e) != Tri::False)
    }
}

/// Whether a computed value fits a numeric column's type.
fn fits(v: &V, ty: &Type) -> bool {
    if ty.array {
        return true;
    }
    let Some(f) = v.as_f64() else { return true };
    if f.is_nan() {
        return !ty.is_integer();
    }
    match ty.base {
        Base::Int2 => f.abs() <= 32_767.0,
        Base::Int4 => f.abs() <= 2_147_483_647.0,
        Base::Int8 => f.abs() < 9.2e18,
        Base::Numeric {
            precision: Some(p),
            scale,
        } => {
            f.abs() < crate::rng::pow10(i32::try_from(p.saturating_sub(scale)).unwrap_or(i32::MAX))
        }
        _ => true,
    }
}

fn same(a: &V, b: &V) -> bool {
    a == b
        || a.compare(b) == Some(Ordering::Equal)
        || matches!((a.text(true), b.text(true)), (Some(x), Some(y)) if x == y)
}

/// A value after `base`: a step forward in its kind.
fn later(base: &V, strict: bool, ty: &Type, rng: &mut ChaCha8Rng) -> Option<V> {
    if !(ty.is_temporal() || ty.is_numeric()) {
        return None;
    }
    let min = i64::from(strict);
    Some(match base {
        V::Ts(t) => V::Ts(t.saturating_add(rng.random_range(min.max(60)..30 * 86_400) * 1_000_000)),
        V::Date(d) => V::Date(d.saturating_add(rng.random_range(min.max(1)..60))),
        V::Int(i) => V::Int(i.saturating_add(i128::from(rng.random_range(min.max(1)..100)))),
        V::Float(f) => V::Float(f + rng.random_range(0.01..100.0f64)),
        V::Dec { units, scale } => V::Dec {
            units: units + i128::from(rng.random_range(1..10_000)) * 10i128.pow(*scale) / 100,
            scale: *scale,
        },
        _ => return None,
    })
}

/// A value of `ty` in `[lo, hi)` (a year or a thousand from an open end).
fn between(lo: Option<&V>, hi: Option<&V>, ty: &Type, rng: &mut ChaCha8Rng, anchor: i64) -> V {
    let year = 365 * DAY;
    match (lo, hi) {
        (Some(V::Ts(a)), Some(V::Ts(b))) if b > a => {
            let t = rng.random_range(*a..*b);
            let t = t - t.rem_euclid(1_000_000);
            V::Ts(t.max(*a))
        }
        (Some(V::Ts(a)), None) => V::Ts(a + rng.random_range(0..year) / 1_000_000 * 1_000_000),
        (None, Some(V::Ts(b))) => {
            V::Ts(b - 1_000_000 - rng.random_range(0..year) / 1_000_000 * 1_000_000)
        }
        (Some(V::Date(a)), Some(V::Date(b))) if b > a => V::Date(rng.random_range(*a..*b)),
        (Some(V::Date(a)), None) => V::Date(a + rng.random_range(0..365)),
        (None, Some(V::Date(b))) => V::Date(b - 1 - rng.random_range(0..365)),
        (Some(V::Int(a)), Some(V::Int(b))) if b > a => V::Int(rng.random_range(*a..*b)),
        (Some(V::Int(a)), None) => V::Int(a + rng.random_range(0..1000)),
        (None, Some(V::Int(b))) => V::Int(b - 1 - rng.random_range(0..1000)),
        (Some(a), Some(b)) if a.as_f64().is_some() && b.as_f64().is_some() => {
            let (x, y) = (a.as_f64().unwrap_or(0.0), b.as_f64().unwrap_or(0.0));
            if y > x {
                V::Float(x + (y - x) * rng.random_range(0.0..0.999))
            } else {
                a.clone()
            }
        }
        (Some(V::Text(a)), _) => V::Text(a.clone()),
        (Some(a), _) => a.clone(),
        (None, None) => match ty.base {
            Base::Timestamp { .. } => V::Ts(anchor - rng.random_range(0..year)),
            Base::Date => V::Date(anchor / DAY - rng.random_range(0..365)),
            _ => V::Int(rng.random_range(0..1000)),
        },
        (None, Some(b)) => b.clone(),
    }
}

/// A value of `ty` below `hi`.
fn before(hi: Option<&V>, ty: &Type, rng: &mut ChaCha8Rng, anchor: i64) -> V {
    between(None, hi, ty, rng, anchor)
}

/// The rows of one table, streamed: the staging table's columns
/// (`romulus_ord`, then every written column) in Postgres text form.
pub struct Rows {
    model: Arc<Model>,
    table: usize,
    next: u64,
    end: u64,
}

impl Rows {
    pub fn new(model: Arc<Model>, table: usize) -> Self {
        let end = model.tables[table].rows;
        Self {
            model,
            table,
            next: 0,
            end,
        }
    }
}

/// One row as the staging table takes it.
pub fn render(model: &Model, ti: usize, row: u64) -> Vec<Option<String>> {
    let t = &model.tables[ti];
    let mut r = Row::new(model, ti, row, 0, 0);
    for attempt in 1..=ATTEMPTS {
        if r.passes() || attempt == ATTEMPTS {
            break;
        }
        r = Row::new(model, ti, row, attempt, 0);
    }
    let mut out = Vec::with_capacity(t.columns.len() + 1);
    out.push(Some(row.to_string()));
    for (c, col) in t.columns.iter().enumerate() {
        if !col.written() {
            continue;
        }
        let tz = col.ty.array
            || matches!(
                col.ty.base,
                Base::Timestamp { tz: true, .. } | Base::Time { tz: true }
            );
        r.ensure(c);
        out.push(r.values[c].as_ref().and_then(|v| v.text(tz)));
    }
    out
}

impl Iterator for Rows {
    type Item = Vec<Option<String>>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.next >= self.end {
            return None;
        }
        let row = self.next;
        self.next += 1;
        Some(render(&self.model, self.table, row))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let n = usize::try_from(self.end - self.next).unwrap_or(usize::MAX);
        (n, Some(n))
    }
}

impl ExactSizeIterator for Rows {}
