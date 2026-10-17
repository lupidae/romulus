//! Partitioned tables: where rows can land. Each leaf of the partition tree
//! becomes constraints on the key columns (a range, a list, what a
//! `DEFAULT` partition takes), parsed from the bounds the model prints. A
//! row first picks a leaf, then its key columns take values inside it
//! (docs/playground.md, "Partitions").

use remus_core::Schema;

use crate::{
    checks::literal,
    expr::{self, Expr, Tok},
    types::Type,
    value::V,
};

/// What one key column takes in one leaf.
#[derive(Debug, Clone, PartialEq)]
pub enum KeyCons {
    /// `[lo, hi)`, `None` for `MINVALUE` / `MAXVALUE`.
    Range {
        lo: Option<V>,
        hi: Option<V>,
    },
    OneOf(Vec<V>),
    /// A `DEFAULT` partition of a list: anything else.
    NoneOf(Vec<V>),
    /// A `DEFAULT` partition of a range: below or above every sibling.
    Outside(Vec<(Option<V>, Option<V>)>),
}

#[derive(Debug, Clone)]
pub struct Leaf {
    /// The partition rows land in (schema-qualified).
    pub name: String,
    /// Relative share of the rows: a `DEFAULT` partition gets fewer.
    pub weight: u32,
    pub keys: Vec<(String, KeyCons)>,
}

#[derive(Debug, Clone, Default)]
pub struct Tree {
    pub leaves: Vec<Leaf>,
    /// Partitions whose bounds were not understood: they get no rows.
    pub skipped: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
enum Strategy {
    Range,
    List,
    Hash,
}

/// `RANGE (a, b)` → (Range, [a, b]); `None` when a key is an expression.
fn key_spec(definition: &str) -> Option<(Strategy, Vec<String>)> {
    let toks = expr::lex(definition)?;
    let mut it = toks.into_iter();
    let strategy = match it.next()? {
        Tok::Ident { name, .. } => match name.as_str() {
            "range" => Strategy::Range,
            "list" => Strategy::List,
            "hash" => Strategy::Hash,
            _ => return None,
        },
        _ => return None,
    };
    if it.next()? != Tok::LParen {
        return None;
    }
    let mut columns = Vec::new();
    loop {
        match it.next()? {
            Tok::Ident { name, .. } => columns.push(name),
            _ => return None,
        }
        // Opclass or collation after the column: not part of the key.
        loop {
            match it.next()? {
                Tok::Comma => break,
                Tok::RParen => return Some((strategy, columns)),
                Tok::Ident { .. } | Tok::Dot => {}
                _ => return None,
            }
        }
    }
}

/// A bound's values: `FROM (…) TO (…)`, `IN (…)`, `WITH (modulus, remainder)`.
#[derive(Debug, Clone, PartialEq)]
enum Bound {
    Range(Vec<Option<Expr>>, Vec<Option<Expr>>),
    In(Vec<Expr>),
    Hash(u64, u64),
    Default,
}

fn parse_bound(text: &str) -> Option<Bound> {
    let toks = expr::lex(text)?;
    let mut p = expr::Parser::new(toks);
    if p.eat_word("default") {
        return Some(Bound::Default);
    }
    if !(p.eat_word("for") && p.eat_word("values")) {
        return None;
    }
    let values = |p: &mut expr::Parser| -> Option<Vec<Option<Expr>>> {
        p.expect(&Tok::LParen)?;
        let mut out = Vec::new();
        loop {
            if p.eat_word("minvalue") || p.eat_word("maxvalue") {
                out.push(None);
            } else {
                out.push(Some(p.expr(0)?));
            }
            match p.next()? {
                Tok::Comma => {}
                Tok::RParen => return Some(out),
                _ => return None,
            }
        }
    };
    if p.eat_word("from") {
        let from = values(&mut p)?;
        if !p.eat_word("to") {
            return None;
        }
        let to = values(&mut p)?;
        return Some(Bound::Range(from, to));
    }
    if p.eat_word("in") {
        let list = values(&mut p)?;
        return list.into_iter().collect::<Option<Vec<_>>>().map(Bound::In);
    }
    if p.eat_word("with") {
        p.expect(&Tok::LParen)?;
        let mut modulus = None;
        let mut remainder = None;
        loop {
            let Some(Tok::Ident { name, .. }) = p.next() else {
                return None;
            };
            let Some(Tok::Num(n)) = p.next() else {
                return None;
            };
            let n: u64 = n.parse().ok()?;
            match name.as_str() {
                "modulus" => modulus = Some(n),
                "remainder" => remainder = Some(n),
                _ => return None,
            }
            match p.next()? {
                Tok::Comma => {}
                Tok::RParen => break,
                _ => return None,
            }
        }
        return Some(Bound::Hash(modulus?, remainder?));
    }
    None
}

/// The leaves of the partition tree under `schema.table`, with their key
/// constraints. `types` gives a column's type (literals read as it).
pub fn tree(
    model: &Schema,
    schema: &str,
    table: &str,
    key: &str,
    types: &dyn Fn(&str) -> Option<Type>,
) -> Tree {
    let mut out = Tree::default();
    walk(model, schema, table, key, types, &[], &mut out, 0);
    out
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn walk(
    model: &Schema,
    schema: &str,
    table: &str,
    key: &str,
    types: &dyn Fn(&str) -> Option<Type>,
    inherited: &[(String, KeyCons)],
    out: &mut Tree,
    depth: usize,
) {
    let children: Vec<_> = model
        .partitions
        .iter()
        .filter(|p| p.parent_schema == schema && p.parent == table)
        .collect();
    let name_of = |p: &remus_core::model::Partition| format!("{}.{}", p.child_schema, p.child);
    let skip_all = |out: &mut Tree| {
        for c in &children {
            out.skipped.push(name_of(c));
        }
    };
    if depth > 8 {
        return skip_all(out);
    }
    let Some((strategy, columns)) = key_spec(key) else {
        return skip_all(out);
    };
    let bounds: Vec<Option<Bound>> = children
        .iter()
        .map(|c| c.bound.as_deref().and_then(parse_bound))
        .collect();
    let read = |e: &Option<Expr>, column: &str| -> Option<Option<V>> {
        match e {
            None => Some(None),
            Some(e) => literal(e, types(column).as_ref()).map(Some),
        }
    };
    if strategy == Strategy::Hash {
        // Every value lands in some child when the remainders cover every
        // class; the children then act as one leaf, routed by Postgres.
        let hashes: Option<Vec<(u64, u64)>> = bounds
            .iter()
            .map(|b| match b {
                Some(Bound::Hash(m, r)) => Some((*m, *r)),
                _ => None,
            })
            .collect();
        let covered = hashes.is_some_and(|h| {
            let max = h.iter().map(|(m, _)| *m).max().unwrap_or(0);
            max > 0 && max <= 4096 && (0..max).all(|class| h.iter().any(|(m, r)| class % m == *r))
        });
        if covered && children.iter().all(|c| c.partition_key.is_none()) {
            out.leaves.push(Leaf {
                name: format!("{schema}.{table}"),
                weight: 4,
                keys: inherited.to_vec(),
            });
        } else {
            skip_all(out);
        }
        return;
    }
    let mut siblings: Vec<KeyCons> = Vec::new();
    let mut leaves: Vec<(usize, Option<KeyCons>)> = Vec::new();
    for (i, bound) in bounds.iter().enumerate() {
        let cons = match (bound, &strategy) {
            (Some(Bound::Range(from, to)), Strategy::Range) if !columns.is_empty() => {
                // The first column decides when the others are free: keep
                // single-column ranges, and multi-column ones whose first
                // column alone has room.
                let (Some(lo), Some(hi)) = (read(&from[0], &columns[0]), read(&to[0], &columns[0]))
                else {
                    leaves.push((i, None));
                    continue;
                };
                if columns.len() > 1 && lo.is_some() && lo == hi {
                    leaves.push((i, None));
                    continue;
                }
                Some(KeyCons::Range { lo, hi })
            }
            (Some(Bound::In(values)), Strategy::List) if columns.len() == 1 => values
                .iter()
                .map(|v| match v {
                    Expr::Lit(expr::Lit::Null) => Some(V::Null),
                    other => literal(other, types(&columns[0]).as_ref()),
                })
                .collect::<Option<Vec<_>>>()
                .map(KeyCons::OneOf),
            (Some(Bound::Default), _) => {
                leaves.push((i, Some(KeyCons::NoneOf(Vec::new()))));
                continue;
            }
            _ => None,
        };
        if let Some(c) = &cons {
            siblings.push(c.clone());
        }
        leaves.push((i, cons));
    }
    for (i, cons) in leaves {
        let child = children[i];
        let Some(cons) = cons else {
            out.skipped.push(name_of(child));
            continue;
        };
        let (cons, weight) = match cons {
            KeyCons::NoneOf(_) => {
                let rest = match strategy {
                    Strategy::List => KeyCons::NoneOf(
                        siblings
                            .iter()
                            .flat_map(|s| match s {
                                KeyCons::OneOf(v) => v.clone(),
                                _ => Vec::new(),
                            })
                            .collect(),
                    ),
                    _ if columns.len() == 1 => KeyCons::Outside(
                        siblings
                            .iter()
                            .filter_map(|s| match s {
                                KeyCons::Range { lo, hi } => Some((lo.clone(), hi.clone())),
                                _ => None,
                            })
                            .collect(),
                    ),
                    _ => {
                        out.skipped.push(name_of(child));
                        continue;
                    }
                };
                (rest, 1)
            }
            other => (other, 4),
        };
        let mut keys = inherited.to_vec();
        keys.push((columns[0].clone(), cons));
        match &child.partition_key {
            Some(sub) => walk(
                model,
                &child.child_schema,
                &child.child,
                sub,
                types,
                &keys,
                out,
                depth + 1,
            ),
            None => out.leaves.push(Leaf {
                name: name_of(child),
                weight,
                keys,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Base;

    fn model() -> Schema {
        Schema::from_json(
            r#"{"remus_version": "0.2", "database": "d", "server_version": "17", "partitions": [
            {"parent_schema": "a", "parent": "events", "child_schema": "a", "child": "events_2026_09",
             "bound": "FOR VALUES FROM ('2026-09-01 00:00:00+00') TO ('2026-10-01 00:00:00+00')"},
            {"parent_schema": "a", "parent": "events", "child_schema": "a", "child": "events_default", "bound": "DEFAULT"},
            {"parent_schema": "a", "parent": "orders", "child_schema": "a", "child": "orders_eu",
             "bound": "FOR VALUES IN ('eu')", "partition_key": "RANGE (placed_on)"},
            {"parent_schema": "a", "parent": "orders_eu", "child_schema": "a", "child": "orders_eu_2026",
             "bound": "FOR VALUES FROM ('2026-01-01') TO ('2027-01-01')"},
            {"parent_schema": "a", "parent": "orders_eu", "child_schema": "a", "child": "orders_eu_old", "bound": "DEFAULT"},
            {"parent_schema": "a", "parent": "sessions", "child_schema": "a", "child": "s0", "bound": "FOR VALUES WITH (modulus 2, remainder 0)"},
            {"parent_schema": "a", "parent": "sessions", "child_schema": "a", "child": "s1", "bound": "FOR VALUES WITH (modulus 2, remainder 1)"},
            {"parent_schema": "a", "parent": "half", "child_schema": "a", "child": "h0", "bound": "FOR VALUES WITH (modulus 2, remainder 0)"}
            ]}"#,
        )
        .unwrap()
    }

    #[allow(clippy::unnecessary_wraps)]
    fn types(column: &str) -> Option<Type> {
        Some(Type::scalar(match column {
            "occurred_at" => Base::Timestamp {
                tz: true,
                precision: None,
            },
            "placed_on" => Base::Date,
            _ => Base::Text,
        }))
    }

    #[test]
    fn range_partitions_and_their_default() {
        let t = tree(&model(), "a", "events", "RANGE (occurred_at)", &types);
        assert_eq!(t.leaves.len(), 2);
        assert!(matches!(
            &t.leaves[0].keys[0].1,
            KeyCons::Range {
                lo: Some(V::Ts(_)),
                hi: Some(V::Ts(_))
            }
        ));
        let KeyCons::Outside(ranges) = &t.leaves[1].keys[0].1 else {
            panic!("{:?}", t.leaves[1])
        };
        assert_eq!(ranges.len(), 1);
        assert_eq!(t.leaves[1].weight, 1);
    }

    #[test]
    fn sub_partitions_combine_their_keys() {
        let t = tree(&model(), "a", "orders", "LIST (region)", &types);
        assert_eq!(t.leaves.len(), 2);
        let leaf = &t.leaves[0];
        assert_eq!(leaf.name, "a.orders_eu_2026");
        assert_eq!(
            leaf.keys[0],
            ("region".into(), KeyCons::OneOf(vec![V::Text("eu".into())]))
        );
        assert!(matches!(
            &leaf.keys[1].1,
            KeyCons::Range {
                lo: Some(V::Date(_)),
                ..
            }
        ));
    }

    #[test]
    fn hash_partitions_route_themselves_when_complete() {
        let t = tree(&model(), "a", "sessions", "HASH (id)", &types);
        assert_eq!(t.leaves.len(), 1);
        assert_eq!(t.skipped.len(), 0);
        let t = tree(&model(), "a", "half", "HASH (id)", &types);
        assert_eq!(t.leaves.len(), 0);
        assert_eq!(t.skipped, ["a.h0"]);
    }
}
