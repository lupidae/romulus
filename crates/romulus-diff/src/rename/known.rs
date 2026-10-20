//! Known renames ([`super::RenameChoices::known`]): followed object by
//! object through the old model, in the order they ran, then kept only
//! when they make sense between the two models.
//!
//! Each object of the old model is tracked with its current name (and, for
//! what can move, its current schema). A rename names an object by its
//! current name (and its parent's), so chains (`a` → `b`, `b` → `c`) and
//! swaps through a temporary name land on the right object. One that names
//! nothing tracked (an object the text created, a function) is ignored.
//!
//! One that takes a name another object still holds waits: the renames
//! after it may free the name. What still waits at the end of its batch
//! (a version's renames, in a range: [`super::RenameChoices::batches`]) is
//! applied together where the objects only exchange names among themselves
//! (a swap given as one pair each way, as a migration's renames are stored
//! and reversed; each cycle on its own), and ignored otherwise (the text
//! dropped the holder first: the drop would have to come before the
//! rename, which name matching already plans differently). Then what named
//! nothing, or waited, is tried again: a version's renames run by level and
//! name a child by its parent's final name, which a held swap only gives
//! the parent at the end. A batch is done before the next one starts: a
//! later version's rename names objects as that version had them.
//!
//! Then every renamed object must exist under its final name in the new
//! model (under its parent's final name); those that do not go back to
//! their old name, and so do two objects ending with one name, until
//! nothing changes.

use std::collections::{BTreeMap, BTreeSet};

use remus_core::{
    Schema,
    model::{ConstraintKind, EntityKind},
};

use super::{Rename, RenameKind, sql::regclass_name};

#[derive(Debug, Clone)]
struct Tracked {
    kind: RenameKind,
    /// Its schema in the old model (a child's: its parent's).
    schema: String,
    /// Where it is now: a table, view or type can move.
    cur_schema: String,
    /// The table of a column, constraint, policy, index or sequence; the
    /// type of a label or an attribute.
    parent: Option<usize>,
    /// For a table: which kind of entity, so a rename lands on its kind.
    entity: Option<EntityKind>,
    /// A primary key, unique or exclusion constraint: its index has its
    /// name, in the relations' namespace.
    backed: bool,
    orig: String,
    cur: String,
}

/// The namespace an object's name lives in: two objects with the same one
/// cannot share a name.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Space {
    Relations(String),
    Types(String),
    Columns(usize),
    Constraints(usize),
    Policies(usize),
    Labels(usize),
    Attributes(usize),
}

pub(crate) struct Known {
    /// From the old name to the final one, in the order to run them.
    pub renames: Vec<Rename>,
    /// What to run: temporary names through swaps.
    pub steps: Vec<Rename>,
}

/// What [`Tracker::run`] did with a rename.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ran {
    Applied,
    /// Its target is held by another object, for now.
    Blocked,
    /// It names nothing tracked, or nothing changes.
    Ignored,
}

struct Tracker {
    objects: Vec<Tracked>,
}

impl Tracker {
    fn of(schema: &Schema) -> Self {
        let mut objects = Vec::new();
        let push = |objects: &mut Vec<Tracked>, kind, schema: &str, parent, name: &str| {
            objects.push(Tracked {
                kind,
                schema: schema.to_owned(),
                cur_schema: schema.to_owned(),
                parent,
                entity: None,
                backed: false,
                orig: name.to_owned(),
                cur: name.to_owned(),
            });
            objects.len() - 1
        };
        let mut sequences = BTreeSet::new();
        for e in &schema.entities {
            let kind = match e.kind {
                EntityKind::Table | EntityKind::PartitionedTable => RenameKind::Table,
                EntityKind::View => RenameKind::View,
                EntityKind::MaterializedView => RenameKind::MaterializedView,
                // Not migrated: never renamed either.
                EntityKind::ForeignTable => continue,
            };
            let at = push(&mut objects, kind, &e.schema, None, &e.name);
            objects[at].entity = Some(e.kind.clone());
            for c in &e.columns {
                push(
                    &mut objects,
                    RenameKind::Column,
                    &e.schema,
                    Some(at),
                    &c.name,
                );
                if c.generated.is_none()
                    && let Some((s, name)) = c.default.as_deref().and_then(nextval)
                {
                    let s = s.unwrap_or_else(|| e.schema.clone());
                    if sequences.insert((s.clone(), name.clone())) {
                        let i = push(&mut objects, RenameKind::Sequence, &s, Some(at), &name);
                        // Its schema is its own (it moves with the table
                        // only when it is the table's).
                        objects[i].cur_schema = s;
                    }
                }
            }
            for c in &e.constraints {
                let i = push(
                    &mut objects,
                    RenameKind::Constraint,
                    &e.schema,
                    Some(at),
                    &c.name,
                );
                objects[i].backed = matches!(
                    c.kind,
                    ConstraintKind::PrimaryKey | ConstraintKind::Unique | ConstraintKind::Exclusion
                );
            }
            for p in &e.policies {
                push(
                    &mut objects,
                    RenameKind::Policy,
                    &e.schema,
                    Some(at),
                    &p.name,
                );
            }
            for i in &e.indexes {
                push(
                    &mut objects,
                    RenameKind::Index,
                    &e.schema,
                    Some(at),
                    &i.name,
                );
            }
        }
        for p in &schema.partitions {
            push(
                &mut objects,
                RenameKind::Partition,
                &p.child_schema,
                None,
                &p.child,
            );
        }
        for e in &schema.enums {
            let at = push(&mut objects, RenameKind::Enum, &e.schema, None, &e.name);
            for l in &e.labels {
                push(&mut objects, RenameKind::EnumValue, &e.schema, Some(at), l);
            }
        }
        for d in &schema.domains {
            push(&mut objects, RenameKind::Domain, &d.schema, None, &d.name);
        }
        for c in &schema.composites {
            let at = push(
                &mut objects,
                RenameKind::Composite,
                &c.schema,
                None,
                &c.name,
            );
            for a in &c.attributes {
                push(
                    &mut objects,
                    RenameKind::Attribute,
                    &c.schema,
                    Some(at),
                    &a.name,
                );
            }
        }
        Self { objects }
    }

    /// Whether the object moves with its parent (`ALTER TABLE … SET SCHEMA`
    /// takes the table's indexes and keys along); a sequence keeps its own.
    fn follows_parent(&self, i: usize) -> bool {
        self.objects[i].parent.is_some() && self.objects[i].kind != RenameKind::Sequence
    }

    /// The schema it is in now.
    fn schema_now(&self, i: usize) -> &str {
        match self.objects[i].parent {
            Some(p) if self.follows_parent(i) => self.schema_now(p),
            _ => &self.objects[i].cur_schema,
        }
    }

    /// Whether it can move (`ALTER … SET SCHEMA`).
    fn movable(&self, i: usize) -> bool {
        self.objects[i].parent.is_none()
            && matches!(
                self.objects[i].kind,
                RenameKind::Table
                    | RenameKind::View
                    | RenameKind::MaterializedView
                    | RenameKind::Enum
                    | RenameKind::Domain
                    | RenameKind::Composite
            )
    }

    /// The namespaces of object `i` were it in `schema`.
    fn spaces_in(&self, i: usize, schema: &str) -> Vec<Space> {
        let o = &self.objects[i];
        let parent = o.parent.unwrap_or(usize::MAX);
        match o.kind {
            k if k.is_relation() => vec![Space::Relations(schema.to_owned())],
            k if k.is_type() => vec![Space::Types(schema.to_owned())],
            RenameKind::Column => vec![Space::Columns(parent)],
            RenameKind::Constraint if o.backed => vec![
                Space::Constraints(parent),
                Space::Relations(schema.to_owned()),
            ],
            RenameKind::Constraint => vec![Space::Constraints(parent)],
            RenameKind::Policy => vec![Space::Policies(parent)],
            RenameKind::EnumValue => vec![Space::Labels(parent)],
            _ => vec![Space::Attributes(parent)],
        }
    }

    fn spaces(&self, i: usize) -> Vec<Space> {
        self.spaces_in(i, self.schema_now(i))
    }

    fn holder(&self, space: &Space, name: &str) -> Option<usize> {
        (0..self.objects.len())
            .find(|&i| self.objects[i].cur == name && self.spaces(i).contains(space))
    }

    fn parent_is(&self, i: usize, name: Option<&str>) -> bool {
        match (self.objects[i].parent, name) {
            (Some(p), Some(name)) => self.objects[p].cur == name,
            (_, None) => true,
            (None, Some(_)) => false,
        }
    }

    /// The object `rename` names, by current names.
    fn find(&self, rename: &Rename) -> Option<usize> {
        let k = rename.kind;
        let matches = |i: usize, kinds: &dyn Fn(RenameKind, &Tracked) -> bool| {
            let o = &self.objects[i];
            self.schema_now(i) == rename.schema && o.cur == rename.from && kinds(o.kind, o)
        };
        let parent = rename.parent.as_deref();
        let exact = |i: usize| {
            matches(i, &|kind, _| kind == k)
                && (k.is_relation() || k.is_type() || self.parent_is(i, parent))
        };
        if let Some(i) = (0..self.objects.len()).find(|&i| exact(i)) {
            return Some(i);
        }
        if k.is_relation() {
            // ALTER TABLE renames an index or a sequence too, ALTER INDEX a
            // constraint's index (and so the constraint).
            return (0..self.objects.len()).find(|&i| {
                matches(i, &|kind, o| {
                    kind.is_relation() || (kind == RenameKind::Constraint && o.backed)
                })
            });
        }
        if k.is_type() {
            return (0..self.objects.len()).find(|&i| matches(i, &|kind, _| kind.is_type()));
        }
        None
    }

    /// Where `rename` takes object `i`: its schema and name.
    fn target(&self, i: usize, rename: &Rename) -> Option<(String, String)> {
        let schema = match &rename.to_schema {
            Some(to) if self.movable(i) => to.clone(),
            Some(_) => return None,
            None => self.schema_now(i).to_owned(),
        };
        Some((schema, rename.to.clone()))
    }

    fn run(&mut self, rename: &Rename) -> Ran {
        let Some(i) = self.find(rename) else {
            return Ran::Ignored;
        };
        let Some((schema, name)) = self.target(i, rename) else {
            return Ran::Ignored;
        };
        if name.is_empty() || (name == self.objects[i].cur && schema == self.schema_now(i)) {
            return Ran::Ignored;
        }
        let taken = self
            .spaces_in(i, &schema)
            .iter()
            .any(|space| self.holder(space, &name).is_some_and(|h| h != i));
        if taken {
            return Ran::Blocked;
        }
        self.objects[i].cur = name;
        self.objects[i].cur_schema = schema;
        Ran::Applied
    }

    /// Renames that still wait, applied together where their objects only
    /// exchange names among themselves: the largest set whose targets are
    /// free or held by one of them, no two taking one name. One waiting on
    /// an object outside it stays out (and so do the ones waiting on its
    /// name), without holding back the other cycles.
    ///
    /// Returns the renames it did not apply (never run again what it did:
    /// its objects answer to their new names now, and a swap run twice
    /// is undone).
    fn permute(&mut self, waiting: &[Rename]) -> Vec<Rename> {
        let mut moves: Vec<(usize, String, String)> = Vec::new();
        let mut of: Vec<Option<usize>> = Vec::new();
        for rename in waiting {
            let found = self.find(rename);
            of.push(found);
            if let Some(i) = found
                && let Some((schema, name)) = self.target(i, rename)
                && !moves.iter().any(|(j, _, _)| *j == i)
            {
                moves.push((i, schema, name));
            }
        }
        loop {
            let movers: BTreeSet<usize> = moves.iter().map(|(i, _, _)| *i).collect();
            let kept: Vec<(usize, String, String)> = moves
                .iter()
                .filter(|(i, schema, name)| {
                    let spaces = self.spaces_in(*i, schema);
                    let free = spaces.iter().all(|space| {
                        self.holder(space, name)
                            .is_none_or(|h| h == *i || movers.contains(&h))
                    });
                    let alone = !moves.iter().any(|(j, other_schema, other)| {
                        j != i
                            && other == name
                            && self
                                .spaces_in(*j, other_schema)
                                .iter()
                                .any(|s| spaces.contains(s))
                    });
                    free && alone
                })
                .cloned()
                .collect();
            if kept.len() == moves.len() {
                break;
            }
            moves = kept;
        }
        let moved: BTreeSet<usize> = moves.iter().map(|(i, _, _)| *i).collect();
        for (i, schema, name) in moves {
            self.objects[i].cur = name;
            self.objects[i].cur_schema = schema;
        }
        waiting
            .iter()
            .zip(of)
            .filter(|(_, found)| found.is_none_or(|i| !moved.contains(&i)))
            .map(|(rename, _)| rename.clone())
            .collect()
    }

    /// Runs `renames` in order, each blocked one waiting (and tried again
    /// whenever one goes through), each one naming nothing (yet) missed.
    fn run_all(
        &mut self,
        renames: impl IntoIterator<Item = Rename>,
        waiting: &mut Vec<Rename>,
        missed: &mut Vec<Rename>,
    ) {
        for rename in renames {
            match self.run(&rename) {
                Ran::Blocked => waiting.push(rename),
                Ran::Ignored => missed.push(rename),
                Ran::Applied => {}
            }
            // What this one freed may let a waiting one through.
            loop {
                let before = waiting.len();
                waiting.retain(|r| self.run(r) == Ran::Blocked);
                if waiting.len() == before {
                    break;
                }
            }
        }
    }

    fn renamed_at(&self, i: usize) -> bool {
        let o = &self.objects[i];
        o.cur != o.orig || o.cur_schema != o.schema
    }

    /// Whether object `i`, under its final name, is in `new`.
    fn in_new(&self, i: usize, new: &Schema) -> bool {
        let o = &self.objects[i];
        let schema = self.schema_now(i);
        let parent = o.parent.map(|p| self.objects[p].cur.as_str()).unwrap_or("");
        let entity = |name: &str| {
            new.entities
                .iter()
                .find(|e| e.schema == schema && e.name == name)
        };
        match o.kind {
            RenameKind::Table | RenameKind::View | RenameKind::MaterializedView => entity(&o.cur)
                .is_some_and(|e| {
                    let table = |k: &EntityKind| {
                        matches!(k, EntityKind::Table | EntityKind::PartitionedTable)
                    };
                    let was = o.entity.as_ref();
                    was.is_some_and(|was| was == &e.kind || (table(was) && table(&e.kind)))
                }),
            RenameKind::Partition => new
                .partitions
                .iter()
                .any(|p| p.child_schema == schema && p.child == o.cur),
            RenameKind::Column => {
                entity(parent).is_some_and(|e| e.columns.iter().any(|c| c.name == o.cur))
            }
            RenameKind::Constraint => {
                entity(parent).is_some_and(|e| e.constraints.iter().any(|c| c.name == o.cur))
            }
            RenameKind::Policy => {
                entity(parent).is_some_and(|e| e.policies.iter().any(|p| p.name == o.cur))
            }
            RenameKind::Index => {
                entity(parent).is_some_and(|e| e.indexes.iter().any(|x| x.name == o.cur))
            }
            RenameKind::Sequence => new
                .entities
                .iter()
                .flat_map(|e| {
                    e.columns
                        .iter()
                        .filter(|c| c.generated.is_none())
                        .filter_map(move |c| {
                            c.default
                                .as_deref()
                                .and_then(nextval)
                                .map(|(s, n)| (s.unwrap_or_else(|| e.schema.clone()), n))
                        })
                })
                .any(|(s, n)| s == schema && n == o.cur),
            RenameKind::Enum => new
                .enums
                .iter()
                .any(|e| e.schema == schema && e.name == o.cur),
            RenameKind::Domain => new
                .domains
                .iter()
                .any(|d| d.schema == schema && d.name == o.cur),
            RenameKind::Composite => new
                .composites
                .iter()
                .any(|c| c.schema == schema && c.name == o.cur),
            RenameKind::EnumValue => new
                .enums
                .iter()
                .find(|e| e.schema == schema && e.name == parent)
                .is_some_and(|e| e.labels.contains(&o.cur)),
            RenameKind::Attribute => new
                .composites
                .iter()
                .find(|c| c.schema == schema && c.name == parent)
                .is_some_and(|c| c.attributes.iter().any(|a| a.name == o.cur)),
        }
    }

    fn revert(&mut self, i: usize) {
        self.objects[i].cur = self.objects[i].orig.clone();
        self.objects[i].cur_schema = self.objects[i].schema.clone();
    }

    /// Puts back what does not land in `new`, and what ends sharing a name.
    fn validate(&mut self, new: &Schema) {
        loop {
            let mut changed = false;
            for i in 0..self.objects.len() {
                if self.renamed_at(i) && !self.in_new(i, new) {
                    self.revert(i);
                    changed = true;
                }
            }
            let mut by_name: BTreeMap<(Space, String), Vec<usize>> = BTreeMap::new();
            for i in 0..self.objects.len() {
                for space in self.spaces(i) {
                    by_name
                        .entry((space, self.objects[i].cur.clone()))
                        .or_default()
                        .push(i);
                }
            }
            for holders in by_name.values().filter(|h| h.len() > 1) {
                for &i in holders {
                    if self.renamed_at(i) {
                        self.revert(i);
                        changed = true;
                    }
                }
            }
            if !changed {
                break;
            }
        }
    }

    /// One rename of object `i` from `(from_schema, from)` to `(to_schema,
    /// to)`, its parent named by its final name.
    fn rename_of(&self, i: usize, from: (&str, &str), to: (&str, &str)) -> Rename {
        let o = &self.objects[i];
        Rename {
            kind: o.kind,
            schema: from.0.to_owned(),
            parent: o.parent.map(|p| self.objects[p].cur.clone()),
            from: from.1.to_owned(),
            to: to.1.to_owned(),
            to_schema: (from.0 != to.0).then(|| to.0.to_owned()),
        }
    }

    /// The level a kind renames at: types, then relations, then what
    /// belongs to them (named by their parents' final names).
    fn level(&self, i: usize) -> u8 {
        match self.objects[i].kind {
            k if k.is_type() => 0,
            k if k.is_relation() && k != RenameKind::Index && k != RenameKind::Sequence => 1,
            RenameKind::Column | RenameKind::EnumValue | RenameKind::Attribute => 2,
            _ => 3,
        }
    }

    /// The renamed objects, by level.
    fn renamed(&self) -> Vec<usize> {
        let mut out: Vec<usize> = (0..self.objects.len())
            .filter(|&i| self.renamed_at(i))
            .collect();
        out.sort_by_key(|&i| (self.level(i), i));
        out
    }

    /// The schema object `i` is in before its own rename runs: a child's
    /// parent has moved by then (it renames at an earlier level).
    fn schema_before(&self, i: usize) -> String {
        if self.follows_parent(i) {
            self.schema_now(i).to_owned()
        } else {
            self.objects[i].schema.clone()
        }
    }

    /// The renames in an order that frees each name before it is taken,
    /// through a temporary name where objects exchange names.
    fn steps(&self, order: &[usize]) -> Vec<Rename> {
        // Where every object is before the renames run, its children's
        // namespaces following a parent that moves.
        let mut at: BTreeMap<usize, (String, String)> = (0..self.objects.len())
            .map(|i| (i, (self.schema_before(i), self.objects[i].orig.clone())))
            .collect();
        let held =
            |at: &BTreeMap<usize, (String, String)>, space: &Space, name: &str, me: usize| {
                at.iter().any(|(&j, (schema, n))| {
                    j != me && n == name && self.spaces_in(j, schema).contains(space)
                })
            };
        let mut pending: Vec<usize> = order.to_vec();
        let mut steps = Vec::new();
        let mut temp = 0;
        while let Some(level) = pending.iter().map(|&i| self.level(i)).min() {
            // A level at a time: a column's step names its table's final name.
            let ready = pending.iter().position(|&i| {
                let to_schema = &self.objects[i].cur_schema;
                let to_schema = if self.follows_parent(i) {
                    self.schema_now(i)
                } else {
                    to_schema
                };
                self.level(i) == level
                    && self
                        .spaces_in(i, to_schema)
                        .iter()
                        .all(|space| !held(&at, space, &self.objects[i].cur, i))
            });
            let i = match ready {
                Some(p) => pending.remove(p),
                None => {
                    // A cycle: set the level's first one aside.
                    let p = pending
                        .iter()
                        .position(|&i| self.level(i) == level)
                        .unwrap_or(0);
                    let i = pending[p];
                    let (schema, name) = at[&i].clone();
                    let spaces = self.spaces_in(i, &schema);
                    let free = loop {
                        temp += 1;
                        let candidate = format!("{}_romulus_{temp}", self.objects[i].orig);
                        if spaces.iter().all(|space| !held(&at, space, &candidate, i)) {
                            break candidate;
                        }
                    };
                    steps.push(self.rename_of(i, (&schema, &name), (&schema, &free)));
                    at.insert(i, (schema, free));
                    continue;
                }
            };
            let (from_schema, from) = at[&i].clone();
            let to_schema = if self.follows_parent(i) {
                self.schema_now(i).to_owned()
            } else {
                self.objects[i].cur_schema.clone()
            };
            let to = self.objects[i].cur.clone();
            steps.push(self.rename_of(i, (&from_schema, &from), (&to_schema, &to)));
            at.insert(i, (to_schema.clone(), to));
            // Its keys and indexes move with a table.
            if from_schema != to_schema {
                for j in 0..self.objects.len() {
                    if self.objects[j].parent == Some(i) && self.follows_parent(j) {
                        let name = at[&j].1.clone();
                        at.insert(j, (to_schema.clone(), name));
                    }
                }
            }
        }
        steps
    }
}

/// `nextval('public.users_id_seq'::regclass)` → the sequence.
pub(crate) fn nextval(default: &str) -> Option<(Option<String>, String)> {
    let inner = default.strip_prefix("nextval('")?;
    let (name, rest) = inner.rsplit_once('\'')?;
    if rest != "::regclass)" {
        return None;
    }
    regclass_name(&name.replace("''", "'"))
}

/// `known` cut where `batches` start (out-of-range or unordered starts
/// ignored).
fn batches<'a>(known: &'a [Rename], starts: &[usize]) -> Vec<&'a [Rename]> {
    let mut cuts: Vec<usize> = starts
        .iter()
        .copied()
        .filter(|&b| b > 0 && b < known.len())
        .collect();
    cuts.sort_unstable();
    cuts.dedup();
    let mut out = Vec::new();
    let mut at = 0;
    for cut in cuts.into_iter().chain([known.len()]) {
        out.push(&known[at..cut]);
        at = cut;
    }
    out
}

pub(crate) fn resolve(old: &Schema, new: &Schema, known: &[Rename], starts: &[usize]) -> Known {
    if known.is_empty() {
        return Known {
            renames: Vec::new(),
            steps: Vec::new(),
        };
    }
    let mut tracker = Tracker::of(old);
    for batch in batches(known, starts) {
        let mut waiting: Vec<Rename> = Vec::new();
        let mut missed: Vec<Rename> = Vec::new();
        tracker.run_all(batch.iter().cloned(), &mut waiting, &mut missed);
        // A cycle held to the end of the batch (a swap stored as its net
        // pairs) applies; then what names its objects by their new names
        // (a column of a swapped table, under the table's final name), or
        // waited on them, goes through, until nothing changes.
        while !waiting.is_empty() {
            let before = waiting.len();
            waiting = tracker.permute(&waiting);
            if waiting.len() == before {
                break;
            }
            let retried: Vec<Rename> = missed.drain(..).chain(waiting.drain(..)).collect();
            tracker.run_all(retried, &mut waiting, &mut missed);
        }
    }
    tracker.validate(new);
    let order = tracker.renamed();
    let renames = order
        .iter()
        .map(|&i| {
            let o = &tracker.objects[i];
            let to_schema = if tracker.follows_parent(i) {
                tracker.schema_now(i).to_owned()
            } else {
                o.cur_schema.clone()
            };
            tracker.rename_of(
                i,
                (&tracker.schema_before(i), &o.orig),
                (&to_schema, &o.cur),
            )
        })
        .collect();
    let steps = tracker.steps(&order);
    Known { renames, steps }
}
