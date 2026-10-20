//! Detected renames: removed and added objects scored in pairs, a level at
//! a time (see the module documentation for the scores).

use std::collections::{BTreeMap, BTreeSet};

use remus_core::{
    Schema,
    model::{Column, ConstraintKind, Entity, EntityKind, TypeKind},
};

use super::{
    Candidate, Confidence, Rename, RenameKind, Resolved,
    apply::apply,
    known::nextval,
    sql::{
        index_shape, move_relation, rename_constraint_column, rename_index_column, rename_relation,
        tokens,
    },
};
use crate::sql::squash;

/// From here a pair is high, when its structure matches exactly.
const HIGH: f64 = 72.0;
/// Below this a pair is not offered.
const MEDIUM: f64 = 45.0;
/// Pairs closer than this for one object are rivals.
const MARGIN: f64 = 5.0;

type Name = (String, String);

/// One removed item and one added item, scored.
#[derive(Debug, Clone)]
struct Pair {
    r: usize,
    a: usize,
    points: f64,
    /// Their structure matches exactly: they may be high.
    exact: bool,
    /// Something besides structure and place says they are one object (a
    /// similar name, a shared comment, keys or columns that follow): high
    /// needs it too, so same-shaped data is never renamed by default.
    evidence: bool,
    reason: String,
    follows: Option<String>,
}

#[derive(Debug)]
struct Assigned {
    pair: Pair,
    /// Indexes of the other assigned pairs it excludes.
    rivals: Vec<usize>,
}

/// The pairs kept: best first, each object in one pair, and pairs too close
/// to tell apart offered together as rivals (none of them chosen).
fn assign(mut pairs: Vec<Pair>) -> Vec<Assigned> {
    pairs.retain(|p| p.points >= MEDIUM);
    pairs.sort_by(|x, y| {
        y.points
            .total_cmp(&x.points)
            .then((x.r, x.a).cmp(&(y.r, y.a)))
    });
    let mut taken_r = BTreeSet::new();
    let mut taken_a = BTreeSet::new();
    let mut out: Vec<Assigned> = Vec::new();
    for (n, p) in pairs.iter().enumerate() {
        if taken_r.contains(&p.r) || taken_a.contains(&p.a) {
            continue;
        }
        let mut group = vec![n];
        group.extend(
            pairs
                .iter()
                .enumerate()
                .filter(|(m, q)| {
                    *m != n
                        && !taken_r.contains(&q.r)
                        && !taken_a.contains(&q.a)
                        && (q.r == p.r || q.a == p.a)
                        && q.points >= p.points - MARGIN
                })
                .map(|(m, _)| m),
        );
        let base = out.len();
        for &m in &group {
            out.push(Assigned {
                pair: pairs[m].clone(),
                rivals: Vec::new(),
            });
        }
        for (x, &m) in group.iter().enumerate() {
            for (y, &k) in group.iter().enumerate() {
                if x != y && (pairs[m].r == pairs[k].r || pairs[m].a == pairs[k].a) {
                    out[base + x].rivals.push(base + y);
                }
            }
        }
        for &m in &group {
            taken_r.insert(pairs[m].r);
            taken_a.insert(pairs[m].a);
        }
    }
    out
}

/// How alike two names are, from 0 to 1: the words they share (`email`
/// and `email_address` share one of three), how few edits turn one into
/// the other, or one abbreviating the other (`desc`, `description`),
/// whichever says more.
pub(crate) fn similarity(a: &str, b: &str) -> f64 {
    let (la, lb) = (a.to_lowercase(), b.to_lowercase());
    if la == lb {
        return 1.0;
    }
    let (wa, wb) = (words(a), words(b));
    let mut rest = wb.clone();
    let mut common = 0;
    for w in &wa {
        if let Some(i) = rest.iter().position(|x| x == w) {
            rest.remove(i);
            common += 1;
        }
    }
    let dice = if wa.is_empty() && wb.is_empty() {
        0.0
    } else {
        f64::from(2 * common) / (wa.len() + wb.len()) as f64
    };
    let longest = la.chars().count().max(lb.chars().count()).max(1);
    let edits = 1.0 - levenshtein(&la, &lb) as f64 / longest as f64;
    let (short, long) = if la.len() <= lb.len() {
        (&la, &lb)
    } else {
        (&lb, &la)
    };
    let abbreviation = if short.chars().count() >= 3 && long.starts_with(short.as_str()) {
        0.6
    } else {
        0.0
    };
    dice.max(edits).max(abbreviation)
}

/// `userEmail`, `user_email`, `user-email` → `user`, `email`.
fn words(name: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut word = String::new();
    let mut previous_lower = false;
    for c in name.chars() {
        if !c.is_alphanumeric() {
            if !word.is_empty() {
                out.push(std::mem::take(&mut word));
            }
            previous_lower = false;
            continue;
        }
        if c.is_uppercase() && previous_lower && !word.is_empty() {
            out.push(std::mem::take(&mut word));
        }
        previous_lower = c.is_lowercase() || c.is_ascii_digit();
        word.extend(c.to_lowercase());
    }
    if !word.is_empty() {
        out.push(word);
    }
    out
}

fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut diagonal = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let above = row[j + 1];
            row[j + 1] = (above + 1)
                .min(row[j] + 1)
                .min(diagonal + usize::from(ca != cb));
            diagonal = above;
        }
    }
    row[b.len()]
}

/// What the names and comments add: up to 25 for the names, 10 for a
/// comment both share. With the reason's words for the name.
fn names_and_comments(
    from: &str,
    to: &str,
    comments: (Option<&String>, Option<&String>),
) -> (f64, Option<&'static str>, Option<&'static str>) {
    let sim = similarity(from, to);
    let comment = matches!(comments, (Some(x), Some(y)) if x == y);
    (
        25.0 * sim + if comment { 10.0 } else { 0.0 },
        (sim >= 0.5).then_some("similar name"),
        comment.then_some("same comment"),
    )
}

/// `Same type; same place; similar name.`
fn sentence(parts: &[&str]) -> String {
    let mut text = parts.join("; ");
    if let Some(first) = text.get(..1) {
        let upper = first.to_uppercase();
        text.replace_range(..1, &upper);
    }
    text.push('.');
    text
}

/// Every name in `schema`'s relation namespace (tables, views, partitions,
/// indexes, the indexes of keys, sequences): a rename cannot take one.
fn relation_names(cur: &Schema, schema: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for e in cur.entities.iter().filter(|e| e.schema == schema) {
        out.insert(e.name.clone());
        out.extend(e.indexes.iter().map(|i| i.name.clone()));
        out.extend(
            e.constraints
                .iter()
                .filter(|c| backed(c.kind.clone()))
                .map(|c| c.name.clone()),
        );
    }
    out.extend(
        cur.partitions
            .iter()
            .filter(|p| p.child_schema == schema)
            .map(|p| p.child.clone()),
    );
    for e in &cur.entities {
        for c in &e.columns {
            if let Some((s, name)) = c.default.as_deref().and_then(nextval)
                && s.as_deref().unwrap_or(&e.schema) == schema
            {
                out.insert(name);
            }
        }
    }
    out
}

/// Every name in `schema`'s type namespace (tables have a row type too).
fn type_names(cur: &Schema, schema: &str) -> BTreeSet<String> {
    let mut out: BTreeSet<String> = cur
        .entities
        .iter()
        .filter(|e| e.schema == schema)
        .map(|e| e.name.clone())
        .collect();
    out.extend(
        cur.enums
            .iter()
            .filter(|e| e.schema == schema)
            .map(|e| e.name.clone()),
    );
    out.extend(
        cur.domains
            .iter()
            .filter(|d| d.schema == schema)
            .map(|d| d.name.clone()),
    );
    out.extend(
        cur.composites
            .iter()
            .filter(|c| c.schema == schema)
            .map(|c| c.name.clone()),
    );
    out
}

fn backed(kind: ConstraintKind) -> bool {
    matches!(
        kind,
        ConstraintKind::PrimaryKey | ConstraintKind::Unique | ConstraintKind::Exclusion
    )
}

fn is_table(e: &Entity) -> bool {
    matches!(e.kind, EntityKind::Table | EntityKind::PartitionedTable)
}

fn find<'a>(schema: &'a Schema, s: &str, name: &str) -> Option<&'a Entity> {
    schema
        .entities
        .iter()
        .find(|e| e.schema == s && e.name == name)
}

/// The keys of the renames applied so far, for [`Pair::follows`]: tables
/// by their final name, columns by table and final name.
#[derive(Default)]
struct Applied {
    tables: BTreeMap<Name, String>,
    columns: BTreeMap<(String, String, String), String>,
}

impl Applied {
    fn of(resolved: &Resolved) -> Self {
        let mut out = Self::default();
        for r in &resolved.renames {
            match r.kind {
                RenameKind::Table => {
                    out.tables.insert((r.schema.clone(), r.to.clone()), r.key());
                }
                RenameKind::Column => {
                    let table = r.parent.clone().unwrap_or_default();
                    out.columns
                        .insert((r.schema.clone(), table, r.to.clone()), r.key());
                }
                _ => {}
            }
        }
        out
    }

    /// What a key, index or sequence of `table` follows: the table's
    /// rename, or a renamed column `mentions` names.
    fn follows(
        &self,
        schema: &str,
        table: &str,
        mentions: impl Fn(&str) -> bool,
    ) -> Option<String> {
        if let Some(key) = self.tables.get(&(schema.to_owned(), table.to_owned())) {
            return Some(key.clone());
        }
        self.columns
            .iter()
            .find(|((s, t, c), _)| s == schema && t == table && mentions(c))
            .map(|(_, key)| key.clone())
    }
}

/// Decides each assigned pair (known decisions, else its default), records
/// it, and applies the ones chosen.
fn settle(
    resolved: &mut Resolved,
    decisions: &BTreeMap<String, bool>,
    proposals: Vec<(Rename, Assigned)>,
) {
    let keys: Vec<String> = proposals.iter().map(|(r, _)| r.key()).collect();
    let mut applied = vec![false; proposals.len()];
    for (i, (rename, assigned)) in proposals.iter().enumerate() {
        let pair = &assigned.pair;
        let confidence =
            if assigned.rivals.is_empty() && pair.exact && pair.evidence && pair.points >= HIGH {
                Confidence::High
            } else {
                Confidence::Medium
            };
        let want = if pair.follows.is_some() {
            true
        } else {
            decisions
                .get(&keys[i])
                .copied()
                .unwrap_or(confidence == Confidence::High)
        };
        applied[i] = want && assigned.rivals.iter().all(|&r| !applied[r]);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let score = pair.points.round().clamp(0.0, 100.0) as u8;
        resolved.candidates.push(Candidate {
            key: keys[i].clone(),
            rename: rename.clone(),
            confidence,
            score,
            applied: applied[i],
            follows: pair.follows.clone(),
            rivals: assigned.rivals.iter().map(|&r| keys[r].clone()).collect(),
            reason: pair.reason.clone(),
        });
    }
    for (i, (rename, _)) in proposals.into_iter().enumerate() {
        if applied[i] {
            apply(&mut resolved.schema, &rename, &mut resolved.rebuild_views);
            resolved.steps.push(rename.clone());
            resolved.renames.push(rename);
        }
    }
}

/// One level of detection: the pairs it proposes, on the old model as the
/// levels before it left it.
type Level = fn(&Resolved, &Schema) -> Vec<(Rename, Assigned)>;

pub(crate) fn detect(resolved: &mut Resolved, new: &Schema, decisions: &BTreeMap<String, bool>) {
    let levels: [Level; 10] = [
        types,
        tables,
        columns,
        labels,
        attributes,
        sequences,
        constraints,
        indexes,
        policies,
        partitions_and_views,
    ];
    for level in levels {
        let proposals = level(resolved, new);
        settle(resolved, decisions, proposals);
    }
}

// ---- Types -------------------------------------------------------------------

/// How many columns kept on both sides go from type `from` to type `to`.
fn switching(cur: &Schema, new: &Schema, kind: &TypeKind, s: &str, from: &str, to: &str) -> usize {
    let mut n = 0;
    for e in &cur.entities {
        let Some(after) = find(new, &e.schema, &e.name) else {
            continue;
        };
        for c in &e.columns {
            if let Some(a) = after.column(&c.name)
                && &c.type_kind == kind
                && &a.type_kind == kind
                && c.type_schema == s
                && a.type_schema == s
                && c.type_name == from
                && a.type_name == to
            {
                n += 1;
            }
        }
    }
    n
}

fn types(resolved: &Resolved, new: &Schema) -> Vec<(Rename, Assigned)> {
    let cur = &resolved.schema;
    let mut out = Vec::new();

    // Enums.
    let removed: Vec<_> = cur
        .enums
        .iter()
        .filter(|e| {
            !new.enums
                .iter()
                .any(|x| x.schema == e.schema && x.name == e.name)
        })
        .collect();
    let added: Vec<_> = new
        .enums
        .iter()
        .filter(|e| {
            !cur.enums
                .iter()
                .any(|x| x.schema == e.schema && x.name == e.name)
        })
        .collect();
    let mut pairs = Vec::new();
    for (i, r) in removed.iter().enumerate() {
        let taken = type_names(cur, &r.schema);
        for (j, a) in added.iter().enumerate() {
            if a.schema != r.schema || taken.contains(&a.name) || r.labels.is_empty() {
                continue;
            }
            let common = r.labels.iter().filter(|l| a.labels.contains(l)).count();
            let overlap = common as f64 / r.labels.len().max(a.labels.len()) as f64;
            let grown = common == r.labels.len();
            // The same labels in place but one, which a label rename
            // explains (the next level offers it).
            let one_renamed = r.labels.len() == a.labels.len()
                && r.labels.len() >= 2
                && r.labels
                    .iter()
                    .zip(&a.labels)
                    .filter(|(x, y)| x != y)
                    .count()
                    == 1;
            let uses = switching(cur, new, &TypeKind::Enum, &r.schema, &r.name, &a.name);
            let (base, structure) = if r.labels == a.labels {
                (75.0, "same labels")
            } else if grown && overlap >= 0.5 {
                (55.0, "same labels, and more")
            } else if one_renamed {
                (55.0, "same labels but one, in place")
            } else if overlap >= 0.5 {
                (45.0, "mostly the same labels")
            } else {
                continue;
            };
            let (extra, name, comment) =
                names_and_comments(&r.name, &a.name, (r.comment.as_ref(), a.comment.as_ref()));
            let used = (uses > 0).then_some("its columns switch to it");
            let parts: Vec<&str> = [Some(structure), used, name, comment]
                .into_iter()
                .flatten()
                .collect();
            pairs.push(Pair {
                r: i,
                a: j,
                points: base + if uses > 0 { 20.0 } else { 0.0 } + extra,
                exact: r.labels == a.labels || ((grown || one_renamed) && uses > 0),
                evidence: name.is_some() || comment.is_some() || uses > 0,
                reason: sentence(&parts),
                follows: None,
            });
        }
    }
    for assigned in assign(pairs) {
        let (r, a) = (removed[assigned.pair.r], added[assigned.pair.a]);
        out.push((
            type_rename(RenameKind::Enum, &r.schema, &r.name, &a.name),
            assigned,
        ));
    }

    // Domains.
    let removed: Vec<_> = cur
        .domains
        .iter()
        .filter(|d| {
            !new.domains
                .iter()
                .any(|x| x.schema == d.schema && x.name == d.name)
        })
        .collect();
    let added: Vec<_> = new
        .domains
        .iter()
        .filter(|d| {
            !cur.domains
                .iter()
                .any(|x| x.schema == d.schema && x.name == d.name)
        })
        .collect();
    let checks = |d: &remus_core::model::DomainType| {
        let mut c: Vec<String> = d.constraints.iter().map(|x| squash(x)).collect();
        c.sort();
        c
    };
    let mut pairs = Vec::new();
    for (i, r) in removed.iter().enumerate() {
        let taken = type_names(cur, &r.schema);
        for (j, a) in added.iter().enumerate() {
            if a.schema != r.schema || taken.contains(&a.name) {
                continue;
            }
            let base_same = squash(&r.base_type) == squash(&a.base_type);
            let exact = base_same
                && r.not_null == a.not_null
                && r.default.as_deref().map(squash) == a.default.as_deref().map(squash)
                && checks(r) == checks(a);
            let uses = switching(cur, new, &TypeKind::Domain, &r.schema, &r.name, &a.name);
            let (base, structure) = if exact {
                (75.0, "same base type, default and checks")
            } else if base_same {
                (45.0, "same base type")
            } else {
                continue;
            };
            let (extra, name, comment) =
                names_and_comments(&r.name, &a.name, (r.comment.as_ref(), a.comment.as_ref()));
            let used = (uses > 0).then_some("its columns switch to it");
            let parts: Vec<&str> = [Some(structure), used, name, comment]
                .into_iter()
                .flatten()
                .collect();
            pairs.push(Pair {
                r: i,
                a: j,
                points: base + if uses > 0 { 20.0 } else { 0.0 } + extra,
                exact,
                evidence: name.is_some() || comment.is_some() || uses > 0,
                reason: sentence(&parts),
                follows: None,
            });
        }
    }
    for assigned in assign(pairs) {
        let (r, a) = (removed[assigned.pair.r], added[assigned.pair.a]);
        out.push((
            type_rename(RenameKind::Domain, &r.schema, &r.name, &a.name),
            assigned,
        ));
    }

    // Composites.
    let removed: Vec<_> = cur
        .composites
        .iter()
        .filter(|c| {
            !new.composites
                .iter()
                .any(|x| x.schema == c.schema && x.name == c.name)
        })
        .collect();
    let added: Vec<_> = new
        .composites
        .iter()
        .filter(|c| {
            !cur.composites
                .iter()
                .any(|x| x.schema == c.schema && x.name == c.name)
        })
        .collect();
    let mut pairs = Vec::new();
    for (i, r) in removed.iter().enumerate() {
        let taken = type_names(cur, &r.schema);
        for (j, a) in added.iter().enumerate() {
            if a.schema != r.schema || taken.contains(&a.name) || r.attributes.is_empty() {
                continue;
            }
            let attrs = |c: &remus_core::model::CompositeType| {
                c.attributes
                    .iter()
                    .map(|x| (x.name.clone(), x.r#type.clone()))
                    .collect::<Vec<_>>()
            };
            let (ra, aa) = (attrs(r), attrs(a));
            let common = ra.iter().filter(|x| aa.contains(x)).count();
            let overlap = common as f64 / ra.len().max(aa.len()) as f64;
            let uses = switching(cur, new, &TypeKind::Composite, &r.schema, &r.name, &a.name);
            let (base, structure) = if ra == aa {
                (75.0, "same attributes")
            } else if overlap >= 0.75 {
                (45.0, "mostly the same attributes")
            } else {
                continue;
            };
            let (extra, name, comment) =
                names_and_comments(&r.name, &a.name, (r.comment.as_ref(), a.comment.as_ref()));
            let used = (uses > 0).then_some("its columns switch to it");
            let parts: Vec<&str> = [Some(structure), used, name, comment]
                .into_iter()
                .flatten()
                .collect();
            pairs.push(Pair {
                r: i,
                a: j,
                points: base + if uses > 0 { 20.0 } else { 0.0 } + extra,
                exact: ra == aa,
                evidence: name.is_some() || comment.is_some() || uses > 0,
                reason: sentence(&parts),
                follows: None,
            });
        }
    }
    for assigned in assign(pairs) {
        let (r, a) = (removed[assigned.pair.r], added[assigned.pair.a]);
        out.push((
            type_rename(RenameKind::Composite, &r.schema, &r.name, &a.name),
            assigned,
        ));
    }
    out
}

fn type_rename(kind: RenameKind, schema: &str, from: &str, to: &str) -> Rename {
    Rename {
        kind,
        schema: schema.to_owned(),
        parent: None,
        from: from.to_owned(),
        to: to.to_owned(),
        to_schema: None,
    }
}

// ---- Tables ------------------------------------------------------------------

/// A column's default with the sequence it draws from blanked: a serial
/// column's sequence is named after its table and column.
fn masked_default(c: &Column) -> Option<String> {
    let default = c.default.as_deref()?;
    Some(if c.generated.is_none() && nextval(default).is_some() {
        "nextval(<sequence>)".to_owned()
    } else {
        squash(default)
    })
}

/// A table's structure with its own name blanked (self references, its
/// indexes' `ON` clause) and every key's and index's name left out: its
/// columns' names and types, its keys, its indexes. Nullability and
/// defaults may change along a rename (they are altered after it).
fn table_shape(e: &Entity) -> (Vec<String>, Vec<String>, Vec<String>) {
    let blank = "\u{1}";
    let mut columns: Vec<String> = e
        .columns
        .iter()
        .map(|c| format!("{} {}", c.name, c.r#type))
        .collect();
    columns.sort();
    // Its own name and schema blanked: a table moved compares too.
    let own = |definition: &str| {
        let moved = move_relation(definition, &e.schema, &e.name, blank);
        rename_relation(&moved, blank, &e.name, blank)
    };
    let mut keys: Vec<String> = e
        .constraints
        .iter()
        .map(|c| format!("{:?} {}", c.kind, squash(&own(&c.definition))))
        .collect();
    keys.sort();
    let mut indexes: Vec<String> = e
        .indexes
        .iter()
        .map(|i| index_shape(&own(&i.definition), &i.name))
        .collect();
    indexes.sort();
    (columns, keys, indexes)
}

/// Same kind of table: plain, or partitioned by the same key.
fn same_class(r: &Entity, a: &Entity) -> bool {
    r.kind == a.kind
        && r.unlogged == a.unlogged
        && r.partition_key.as_deref().map(squash) == a.partition_key.as_deref().map(squash)
}

fn tables(resolved: &Resolved, new: &Schema) -> Vec<(Rename, Assigned)> {
    let cur = &resolved.schema;
    let removed: Vec<&Entity> = cur
        .entities
        .iter()
        .filter(|e| is_table(e) && find(new, &e.schema, &e.name).is_none())
        .collect();
    let added: Vec<&Entity> = new
        .entities
        .iter()
        .filter(|e| is_table(e) && find(cur, &e.schema, &e.name).is_none())
        .collect();
    let mut pairs = Vec::new();
    let mut taken_in: BTreeMap<&str, BTreeSet<String>> = BTreeMap::new();
    for (i, r) in removed.iter().enumerate() {
        if r.columns.is_empty() || added.is_empty() {
            continue;
        }
        let shape = table_shape(r);
        for (j, a) in added.iter().enumerate() {
            // Within its schema, or moved to another under its own name
            // (`ALTER TABLE … SET SCHEMA`).
            let moved = a.schema != r.schema;
            if (moved && a.name != r.name) || !same_class(r, a) {
                continue;
            }
            let taken = taken_in
                .entry(a.schema.as_str())
                .or_insert_with(|| relation_names(cur, &a.schema));
            if taken.contains(&a.name) {
                continue;
            }
            let typed = |e: &Entity| {
                e.columns
                    .iter()
                    .map(|c| (c.name.clone(), c.r#type.clone()))
                    .collect::<Vec<_>>()
            };
            let (rc, ac) = (typed(r), typed(a));
            let common = rc.iter().filter(|x| ac.contains(x)).count();
            let overlap = common as f64 / rc.len().max(ac.len()) as f64;
            let exact = rc.len() == ac.len() && common == rc.len() && shape == table_shape(a);
            let (base, structure) = if exact {
                (
                    if rc.len() >= 2 { 75.0 } else { 60.0 },
                    "same columns, keys and indexes".to_owned(),
                )
            } else if overlap >= 0.75 {
                (
                    30.0 + 30.0 * overlap,
                    format!("{common} of {} columns alike", rc.len().max(ac.len())),
                )
            } else if rc.len() == ac.len()
                && rc.len() >= 2
                && rc.iter().zip(&ac).all(|(x, y)| x.1 == y.1)
            {
                // Renamed with some of its columns: their types, in order.
                (
                    45.0,
                    "columns of the same types, in the same order".to_owned(),
                )
            } else {
                continue;
            };
            // A foreign key kept on both sides that now references it.
            let followed = cur.entities.iter().any(|e| {
                find(new, &e.schema, &e.name).is_some_and(|after| {
                    e.foreign_keys().any(|fk| {
                        fk.ref_schema.as_deref() == Some(&r.schema)
                            && fk.ref_table.as_deref() == Some(&r.name)
                            && after.constraints.iter().any(|x| {
                                x.name == fk.name
                                    && x.ref_schema.as_deref() == Some(&a.schema)
                                    && x.ref_table.as_deref() == Some(&a.name)
                            })
                    })
                })
            });
            let (extra, name, comment) =
                names_and_comments(&r.name, &a.name, (r.comment.as_ref(), a.comment.as_ref()));
            let name = if moved {
                Some("the same name, in another schema")
            } else {
                name
            };
            let follows = followed.then_some("its foreign keys follow it");
            let parts: Vec<&str> = [Some(structure.as_str()), follows, name, comment]
                .into_iter()
                .flatten()
                .collect();
            pairs.push(Pair {
                r: i,
                a: j,
                points: base + if followed { 10.0 } else { 0.0 } + extra,
                exact,
                evidence: name.is_some() || comment.is_some() || followed || moved,
                reason: sentence(&parts),
                follows: None,
            });
        }
    }
    assign(pairs)
        .into_iter()
        .map(|assigned| {
            let (r, a) = (removed[assigned.pair.r], added[assigned.pair.a]);
            let rename = Rename {
                kind: RenameKind::Table,
                schema: r.schema.clone(),
                parent: None,
                from: r.name.clone(),
                to: a.name.clone(),
                to_schema: (a.schema != r.schema).then(|| a.schema.clone()),
            };
            (rename, assigned)
        })
        .collect()
}

// ---- Columns -----------------------------------------------------------------

/// Every key and index of `table` naming `column`, its name blanked (and
/// the keys' and indexes' own names left out).
fn column_keys(table: &Entity, column: &str) -> Vec<String> {
    let blank = "\u{1}";
    let mut out = Vec::new();
    for c in &table.constraints {
        let self_reference = c.kind == ConstraintKind::ForeignKey
            && c.ref_schema.as_deref() == Some(&table.schema)
            && c.ref_table.as_deref() == Some(&table.name);
        let blanked = rename_constraint_column(&c.definition, column, blank, self_reference);
        if blanked != c.definition {
            out.push(format!("{:?} {}", c.kind, squash(&blanked)));
        }
    }
    for i in &table.indexes {
        let shape = index_shape(&i.definition, &i.name);
        let blanked = rename_index_column(&shape, column, blank);
        if blanked != shape {
            out.push(blanked);
        }
    }
    out.sort();
    out
}

/// The type families a conversion keeps meaning within.
fn type_family(ty: &str) -> Option<&'static str> {
    let array = ty.ends_with("[]");
    let base = ty.trim_end_matches("[]");
    let base = base.split('(').next().unwrap_or(base).trim();
    let family = match base {
        "smallint" | "integer" | "bigint" | "numeric" | "real" | "double precision" => "number",
        "text" | "character varying" | "character" | "citext" | "name" => "text",
        "timestamp without time zone" | "timestamp with time zone" | "date" => "time",
        "time without time zone" | "time with time zone" => "clock",
        "json" | "jsonb" => "json",
        "boolean" => "boolean",
        "uuid" => "uuid",
        "bytea" => "bytes",
        "inet" | "cidr" => "address",
        _ => return None,
    };
    Some(if array {
        match family {
            "number" => "number[]",
            "text" => "text[]",
            _ => family,
        }
    } else {
        family
    })
}

/// For each column not kept on both sides: the kept columns around it and
/// its place between them.
fn slots<'a>(
    columns: &'a [Column],
    kept: &BTreeSet<&str>,
) -> Vec<(Option<&'a str>, Option<&'a str>, usize)> {
    let mut out = Vec::with_capacity(columns.len());
    let mut previous = None;
    let mut offset = 0;
    for (i, c) in columns.iter().enumerate() {
        if kept.contains(c.name.as_str()) {
            previous = Some(c.name.as_str());
            offset = 0;
            out.push((previous, previous, 0));
            continue;
        }
        let following = columns[i + 1..]
            .iter()
            .find(|x| kept.contains(x.name.as_str()))
            .map(|x| x.name.as_str());
        out.push((previous, following, offset));
        offset += 1;
    }
    out
}

fn columns(resolved: &Resolved, new: &Schema) -> Vec<(Rename, Assigned)> {
    let cur = &resolved.schema;
    let mut out = Vec::new();
    for b in cur.entities.iter().filter(|e| is_table(e)) {
        let Some(a) = find(new, &b.schema, &b.name).filter(|a| is_table(a)) else {
            continue;
        };
        let kept: BTreeSet<&str> = b
            .columns
            .iter()
            .filter(|c| a.column(&c.name).is_some())
            .map(|c| c.name.as_str())
            .collect();
        let removed: Vec<usize> = (0..b.columns.len())
            .filter(|&i| !kept.contains(b.columns[i].name.as_str()))
            .collect();
        let added: Vec<usize> = (0..a.columns.len())
            .filter(|&j| b.column(&a.columns[j].name).is_none())
            .collect();
        if removed.is_empty() || added.is_empty() {
            continue;
        }
        let (old_slots, new_slots) = (slots(&b.columns, &kept), slots(&a.columns, &kept));
        let mut pairs = Vec::new();
        for (ri, &i) in removed.iter().enumerate() {
            let r = &b.columns[i];
            let r_keys = column_keys(b, &r.name);
            for (aj, &j) in added.iter().enumerate() {
                let x = &a.columns[j];
                let same_type = r.r#type == x.r#type;
                let exact = same_type
                    && r.not_null == x.not_null
                    && masked_default(r) == masked_default(x)
                    && r.identity == x.identity
                    && r.generated == x.generated
                    && r_keys == column_keys(a, &x.name);
                let placed = i == j || old_slots[i] == new_slots[j];
                let compatible = same_type
                    || type_family(&r.r#type).is_some_and(|f| type_family(&x.r#type) == Some(f));
                let (base, structure) = if exact {
                    (60.0, "same type, nullability, default and keys")
                } else if compatible && placed {
                    (
                        35.0,
                        if same_type {
                            "same type"
                        } else {
                            "compatible type"
                        },
                    )
                } else {
                    continue;
                };
                let (extra, name, comment) =
                    names_and_comments(&r.name, &x.name, (r.comment.as_ref(), x.comment.as_ref()));
                let place = placed.then_some("same place");
                let parts: Vec<&str> = [Some(structure), place, name, comment]
                    .into_iter()
                    .flatten()
                    .collect();
                pairs.push(Pair {
                    r: ri,
                    a: aj,
                    points: base + if placed { 15.0 } else { 0.0 } + extra,
                    exact,
                    evidence: name.is_some() || comment.is_some(),
                    reason: sentence(&parts),
                    follows: None,
                });
            }
        }
        for assigned in assign(pairs) {
            let (r, x) = (
                &b.columns[removed[assigned.pair.r]],
                &a.columns[added[assigned.pair.a]],
            );
            let rename = Rename {
                kind: RenameKind::Column,
                schema: b.schema.clone(),
                parent: Some(b.name.clone()),
                from: r.name.clone(),
                to: x.name.clone(),
                to_schema: None,
            };
            out.push((rename, assigned));
        }
    }
    out
}

// ---- Labels and attributes ---------------------------------------------------

/// The longest common subsequence of two lists, as pairs of positions.
fn common_order(old: &[String], new: &[String]) -> Vec<(usize, usize)> {
    let (n, m) = (old.len(), new.len());
    let mut table = vec![vec![0_usize; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            table[i][j] = if old[i] == new[j] {
                table[i + 1][j + 1] + 1
            } else {
                table[i + 1][j].max(table[i][j + 1])
            };
        }
    }
    let (mut i, mut j) = (0, 0);
    let mut out = Vec::new();
    while i < n && j < m {
        if old[i] == new[j] {
            out.push((i, j));
            i += 1;
            j += 1;
        } else if table[i + 1][j] >= table[i][j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    out
}

fn labels(resolved: &Resolved, new: &Schema) -> Vec<(Rename, Assigned)> {
    let cur = &resolved.schema;
    let mut out = Vec::new();
    for b in &cur.enums {
        let Some(a) = new
            .enums
            .iter()
            .find(|x| x.schema == b.schema && x.name == b.name)
        else {
            continue;
        };
        let kept = common_order(&b.labels, &a.labels);
        // The gaps between kept labels, on both sides.
        let mut bounds: Vec<(usize, usize)> = vec![(0, 0)];
        bounds.extend(kept.iter().map(|(i, j)| (i + 1, j + 1)));
        let mut ends: Vec<(usize, usize)> = kept.clone();
        ends.push((b.labels.len(), a.labels.len()));
        let mut pairs = Vec::new();
        let mut removed: Vec<usize> = Vec::new();
        let mut added: Vec<usize> = Vec::new();
        for ((i0, j0), (i1, j1)) in bounds.into_iter().zip(ends) {
            let (old_gap, new_gap) = (i0..i1, j0..j1);
            if old_gap.is_empty() || old_gap.len() != new_gap.len() {
                continue;
            }
            let single = old_gap.len() == 1;
            for (i, j) in old_gap.zip(new_gap) {
                if a.labels.contains(&b.labels[i]) || b.labels.contains(&a.labels[j]) {
                    continue;
                }
                removed.push(i);
                added.push(j);
                let (extra, name, _) = names_and_comments(&b.labels[i], &a.labels[j], (None, None));
                let place = if single {
                    "same place among the labels"
                } else {
                    "between the same labels, in order"
                };
                let parts: Vec<&str> = [Some(place), name].into_iter().flatten().collect();
                pairs.push(Pair {
                    r: removed.len() - 1,
                    a: added.len() - 1,
                    points: if single { 75.0 } else { 50.0 } + extra,
                    exact: single,
                    evidence: name.is_some(),
                    reason: sentence(&parts),
                    follows: None,
                });
            }
        }
        for assigned in assign(pairs) {
            let rename = Rename {
                kind: RenameKind::EnumValue,
                schema: b.schema.clone(),
                parent: Some(b.name.clone()),
                from: b.labels[removed[assigned.pair.r]].clone(),
                to: a.labels[added[assigned.pair.a]].clone(),
                to_schema: None,
            };
            out.push((rename, assigned));
        }
    }
    out
}

fn attributes(resolved: &Resolved, new: &Schema) -> Vec<(Rename, Assigned)> {
    let cur = &resolved.schema;
    let mut out = Vec::new();
    for b in &cur.composites {
        let Some(a) = new
            .composites
            .iter()
            .find(|x| x.schema == b.schema && x.name == b.name)
        else {
            continue;
        };
        let removed: Vec<usize> = (0..b.attributes.len())
            .filter(|&i| !a.attributes.iter().any(|x| x.name == b.attributes[i].name))
            .collect();
        let added: Vec<usize> = (0..a.attributes.len())
            .filter(|&j| !b.attributes.iter().any(|x| x.name == a.attributes[j].name))
            .collect();
        let mut pairs = Vec::new();
        for (ri, &i) in removed.iter().enumerate() {
            for (aj, &j) in added.iter().enumerate() {
                let (r, x) = (&b.attributes[i], &a.attributes[j]);
                if r.r#type != x.r#type {
                    continue;
                }
                let placed = i == j;
                let (extra, name, _) = names_and_comments(&r.name, &x.name, (None, None));
                let parts: Vec<&str> = [Some("same type"), placed.then_some("same place"), name]
                    .into_iter()
                    .flatten()
                    .collect();
                pairs.push(Pair {
                    r: ri,
                    a: aj,
                    points: 60.0 + if placed { 15.0 } else { 0.0 } + extra,
                    exact: true,
                    evidence: name.is_some(),
                    reason: sentence(&parts),
                    follows: None,
                });
            }
        }
        for assigned in assign(pairs) {
            let rename = Rename {
                kind: RenameKind::Attribute,
                schema: b.schema.clone(),
                parent: Some(b.name.clone()),
                from: b.attributes[removed[assigned.pair.r]].name.clone(),
                to: a.attributes[added[assigned.pair.a]].name.clone(),
                to_schema: None,
            };
            out.push((rename, assigned));
        }
    }
    out
}

// ---- Sequences, keys, indexes, policies ------------------------------------

/// The sequences each side's serial columns draw from.
fn drawn(schema: &Schema) -> BTreeSet<Name> {
    let mut out = BTreeSet::new();
    for e in &schema.entities {
        for c in e.columns.iter().filter(|c| c.generated.is_none()) {
            if let Some((s, name)) = c.default.as_deref().and_then(nextval) {
                out.insert((s.unwrap_or_else(|| e.schema.clone()), name));
            }
        }
    }
    out
}

fn sequences(resolved: &Resolved, new: &Schema) -> Vec<(Rename, Assigned)> {
    let cur = &resolved.schema;
    let applied = Applied::of(resolved);
    let (old_drawn, new_drawn) = (drawn(cur), drawn(new));
    let mut found: BTreeMap<Name, (String, Option<String>, String)> = BTreeMap::new();
    let mut conflicting = BTreeSet::new();
    for b in &cur.entities {
        let Some(a) = find(new, &b.schema, &b.name) else {
            continue;
        };
        for c in b.columns.iter().filter(|c| c.generated.is_none()) {
            let Some(x) = a.column(&c.name).filter(|x| x.generated.is_none()) else {
                continue;
            };
            let side = |col: &Column| {
                col.default
                    .as_deref()
                    .and_then(nextval)
                    .map(|(s, n)| (s.unwrap_or_else(|| b.schema.clone()), n))
            };
            let (Some(from), Some(to)) = (side(c), side(x)) else {
                continue;
            };
            if from == to || from.0 != to.0 || new_drawn.contains(&from) || old_drawn.contains(&to)
            {
                continue;
            }
            if relation_names(cur, &from.0).contains(&to.1) {
                continue;
            }
            let follows = applied.follows(&b.schema, &b.name, |renamed| renamed == c.name);
            match found.get(&from) {
                Some((other, _, _)) if *other != to.1 => {
                    conflicting.insert(from.clone());
                }
                Some(_) => {}
                None => {
                    found.insert(from, (to.1, follows, b.name.clone()));
                }
            }
        }
    }
    found
        .into_iter()
        .filter(|(from, _)| !conflicting.contains(from))
        .map(|((schema, from), (to, follows, table))| {
            let rename = Rename {
                kind: RenameKind::Sequence,
                schema,
                parent: Some(table),
                from,
                to,
                to_schema: None,
            };
            let assigned = Assigned {
                pair: Pair {
                    r: 0,
                    a: 0,
                    points: 80.0,
                    exact: true,
                    evidence: true,
                    reason: "Drawn from by the same column.".to_owned(),
                    follows,
                },
                rivals: Vec::new(),
            };
            (rename, assigned)
        })
        .collect()
}

/// Whether the text names `column` (outside literals).
fn names(text: &str, column: &str) -> bool {
    tokens(text).iter().any(|t| t.is(column))
}

fn constraints(resolved: &Resolved, new: &Schema) -> Vec<(Rename, Assigned)> {
    let cur = &resolved.schema;
    let applied = Applied::of(resolved);
    let mut out = Vec::new();
    for b in &cur.entities {
        let Some(a) = find(new, &b.schema, &b.name) else {
            continue;
        };
        let removed: Vec<_> = b
            .constraints
            .iter()
            .filter(|c| !a.constraints.iter().any(|x| x.name == c.name))
            .collect();
        let added: Vec<_> = a
            .constraints
            .iter()
            .filter(|c| !b.constraints.iter().any(|x| x.name == c.name))
            .collect();
        if removed.is_empty() || added.is_empty() {
            continue;
        }
        let relations = relation_names(cur, &b.schema);
        let mut pairs = Vec::new();
        for (i, r) in removed.iter().enumerate() {
            for (j, x) in added.iter().enumerate() {
                if r.kind != x.kind
                    || squash(&r.definition) != squash(&x.definition)
                    || (backed(x.kind.clone()) && relations.contains(&x.name))
                {
                    continue;
                }
                let (extra, name, comment) =
                    names_and_comments(&r.name, &x.name, (r.comment.as_ref(), x.comment.as_ref()));
                let parts: Vec<&str> = [Some("same definition"), name, comment]
                    .into_iter()
                    .flatten()
                    .collect();
                pairs.push(Pair {
                    r: i,
                    a: j,
                    points: 75.0 + extra,
                    exact: true,
                    evidence: true,
                    reason: sentence(&parts),
                    follows: applied.follows(&b.schema, &b.name, |c| names(&r.definition, c)),
                });
            }
        }
        for assigned in assign(pairs) {
            let rename = Rename {
                kind: RenameKind::Constraint,
                schema: b.schema.clone(),
                parent: Some(b.name.clone()),
                from: removed[assigned.pair.r].name.clone(),
                to: added[assigned.pair.a].name.clone(),
                to_schema: None,
            };
            out.push((rename, assigned));
        }
    }
    out
}

fn indexes(resolved: &Resolved, new: &Schema) -> Vec<(Rename, Assigned)> {
    let cur = &resolved.schema;
    let applied = Applied::of(resolved);
    let mut out = Vec::new();
    for b in &cur.entities {
        let Some(a) = find(new, &b.schema, &b.name) else {
            continue;
        };
        let removed: Vec<_> = b
            .indexes
            .iter()
            .filter(|i| !a.indexes.iter().any(|x| x.name == i.name))
            .collect();
        let added: Vec<_> = a
            .indexes
            .iter()
            .filter(|i| !b.indexes.iter().any(|x| x.name == i.name))
            .collect();
        if removed.is_empty() || added.is_empty() {
            continue;
        }
        let relations = relation_names(cur, &b.schema);
        let mut pairs = Vec::new();
        for (i, r) in removed.iter().enumerate() {
            let shape = index_shape(&r.definition, &r.name);
            for (j, x) in added.iter().enumerate() {
                if r.unique != x.unique
                    || r.partial != x.partial
                    || relations.contains(&x.name)
                    || shape != index_shape(&x.definition, &x.name)
                {
                    continue;
                }
                let (extra, name, _) = names_and_comments(&r.name, &x.name, (None, None));
                let parts: Vec<&str> = [Some("same definition"), name]
                    .into_iter()
                    .flatten()
                    .collect();
                pairs.push(Pair {
                    r: i,
                    a: j,
                    points: 75.0 + extra,
                    exact: true,
                    evidence: true,
                    reason: sentence(&parts),
                    follows: applied.follows(&b.schema, &b.name, |c| names(&shape, c)),
                });
            }
        }
        for assigned in assign(pairs) {
            let rename = Rename {
                kind: RenameKind::Index,
                schema: b.schema.clone(),
                parent: Some(b.name.clone()),
                from: removed[assigned.pair.r].name.clone(),
                to: added[assigned.pair.a].name.clone(),
                to_schema: None,
            };
            out.push((rename, assigned));
        }
    }
    out
}

fn policies(resolved: &Resolved, new: &Schema) -> Vec<(Rename, Assigned)> {
    let cur = &resolved.schema;
    let mut out = Vec::new();
    for b in &cur.entities {
        let Some(a) = find(new, &b.schema, &b.name) else {
            continue;
        };
        let removed: Vec<_> = b
            .policies
            .iter()
            .filter(|p| !a.policies.iter().any(|x| x.name == p.name))
            .collect();
        let added: Vec<_> = a
            .policies
            .iter()
            .filter(|p| !b.policies.iter().any(|x| x.name == p.name))
            .collect();
        let shape = |p: &remus_core::model::Policy| {
            let mut roles = p.roles.clone();
            roles.sort();
            (
                format!("{:?}", p.command),
                p.permissive,
                roles,
                p.using.as_deref().map(squash),
                p.check.as_deref().map(squash),
            )
        };
        let mut pairs = Vec::new();
        for (i, r) in removed.iter().enumerate() {
            for (j, x) in added.iter().enumerate() {
                if shape(r) != shape(x) {
                    continue;
                }
                let (extra, name, _) = names_and_comments(&r.name, &x.name, (None, None));
                let parts: Vec<&str> = [Some("same command, roles and expressions"), name]
                    .into_iter()
                    .flatten()
                    .collect();
                pairs.push(Pair {
                    r: i,
                    a: j,
                    points: 75.0 + extra,
                    exact: true,
                    evidence: true,
                    reason: sentence(&parts),
                    follows: None,
                });
            }
        }
        for assigned in assign(pairs) {
            let rename = Rename {
                kind: RenameKind::Policy,
                schema: b.schema.clone(),
                parent: Some(b.name.clone()),
                from: removed[assigned.pair.r].name.clone(),
                to: added[assigned.pair.a].name.clone(),
                to_schema: None,
            };
            out.push((rename, assigned));
        }
    }
    out
}

// ---- Partitions and views ----------------------------------------------------

fn partitions_and_views(resolved: &Resolved, new: &Schema) -> Vec<(Rename, Assigned)> {
    let cur = &resolved.schema;
    let mut out = Vec::new();

    let removed: Vec<_> = cur
        .partitions
        .iter()
        .filter(|p| {
            !new.partitions
                .iter()
                .any(|x| x.child_schema == p.child_schema && x.child == p.child)
        })
        .collect();
    let added: Vec<_> = new
        .partitions
        .iter()
        .filter(|p| {
            !cur.partitions
                .iter()
                .any(|x| x.child_schema == p.child_schema && x.child == p.child)
        })
        .collect();
    let mut pairs = Vec::new();
    for (i, r) in removed.iter().enumerate() {
        let taken = relation_names(cur, &r.child_schema);
        for (j, a) in added.iter().enumerate() {
            let same = a.child_schema == r.child_schema
                && a.parent_schema == r.parent_schema
                && a.parent == r.parent
                && r.bound.as_deref().map(squash) == a.bound.as_deref().map(squash)
                && r.partition_key.as_deref().map(squash) == a.partition_key.as_deref().map(squash);
            if !same || taken.contains(&a.child) {
                continue;
            }
            let (extra, name, _) = names_and_comments(&r.child, &a.child, (None, None));
            let parts: Vec<&str> = [Some("same parent and bound"), name]
                .into_iter()
                .flatten()
                .collect();
            pairs.push(Pair {
                r: i,
                a: j,
                points: 75.0 + extra,
                exact: true,
                evidence: name.is_some(),
                reason: sentence(&parts),
                follows: None,
            });
        }
    }
    for assigned in assign(pairs) {
        let (r, a) = (removed[assigned.pair.r], added[assigned.pair.a]);
        let rename = Rename {
            kind: RenameKind::Partition,
            schema: r.child_schema.clone(),
            parent: None,
            from: r.child.clone(),
            to: a.child.clone(),
            to_schema: None,
        };
        out.push((rename, assigned));
    }

    let removed: Vec<&Entity> = cur
        .entities
        .iter()
        .filter(|e| e.kind.is_view() && find(new, &e.schema, &e.name).is_none())
        .collect();
    let added: Vec<&Entity> = new
        .entities
        .iter()
        .filter(|e| e.kind.is_view() && find(cur, &e.schema, &e.name).is_none())
        .collect();
    let mut pairs = Vec::new();
    for (i, r) in removed.iter().enumerate() {
        if resolved
            .rebuild_views
            .contains(&(r.schema.clone(), r.name.clone()))
        {
            continue;
        }
        let taken = relation_names(cur, &r.schema);
        for (j, a) in added.iter().enumerate() {
            let names = |e: &Entity| e.columns.iter().map(|c| c.name.clone()).collect::<Vec<_>>();
            let same = a.schema == r.schema
                && a.kind == r.kind
                && r.view_definition.as_deref().map(squash)
                    == a.view_definition.as_deref().map(squash)
                && names(r) == names(a);
            if !same || taken.contains(&a.name) {
                continue;
            }
            let (extra, name, comment) =
                names_and_comments(&r.name, &a.name, (r.comment.as_ref(), a.comment.as_ref()));
            let parts: Vec<&str> = [Some("same query"), name, comment]
                .into_iter()
                .flatten()
                .collect();
            pairs.push(Pair {
                r: i,
                a: j,
                points: 75.0 + extra,
                exact: true,
                evidence: true,
                reason: sentence(&parts),
                follows: None,
            });
        }
    }
    for assigned in assign(pairs) {
        let (r, a) = (removed[assigned.pair.r], added[assigned.pair.a]);
        let kind = if r.kind == EntityKind::MaterializedView {
            RenameKind::MaterializedView
        } else {
            RenameKind::View
        };
        let rename = Rename {
            kind,
            schema: r.schema.clone(),
            parent: None,
            from: r.name.clone(),
            to: a.name.clone(),
            to_schema: None,
        };
        out.push((rename, assigned));
    }
    out
}
