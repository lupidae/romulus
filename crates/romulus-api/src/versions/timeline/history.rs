//! A database's history as the history of each of its objects: tables (and
//! views…), their columns, constraints, indexes and policies, user types,
//! partitions and extensions.
//!
//! [`step`] reads what romulus-diff reports between two consecutive versions
//! as events, one per object it touched (added, removed, changed, renamed),
//! with the object's value in the newer version and the diff's structured
//! changes. [`fold`] replays the steps of a history, oldest first, into each
//! object's [`ObjectState`]s: what it was from one version to another. An object
//! removed and later added again under its name is the same object, with a
//! gap between its states.
//!
//! Objects are matched by name, as romulus-diff matches them, so a rename is
//! a removal and an addition, unless the diff reports it as a rename
//! ([`reported_renames`], the seam rename detection plugs into): the object
//! then keeps its history under its new name, and the state starting at the
//! rename says so (`{"field": "name", …}`).
//!
//! A step depends on its two models only, which never change: steps are
//! cached by the ids of their two versions (`super::Steps`), so a version
//! added, removed or restored costs the steps it creates, not the history.

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use remus_core::{Schema, model as m};
use romulus_diff::{
    ColumnChange, CompositeChange, ConstraintChange, DomainChange, EntityChange, EntityDiff,
    EnumChange, ExtensionChange, IndexChange, PartitionChange, PartitionDiff, PolicyChange,
    Relation, SchemaDiff, Status,
};
use serde::Serialize;
use serde_json::value::RawValue;
use utoipa::ToSchema;

/// JSON kept as its text: a cached step and every response built from it
/// share it.
pub type Json = Arc<RawValue>;

// ---- What the API answers --------------------------------------------------

/// One state of an object: what it was from the version at `from` until the
/// version at `to`. Indexes are into the timeline's `versions`.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ObjectState {
    /// The first version it is in, in this state.
    pub from: u32,
    /// The first version it is no longer in, in this state (removed, or
    /// changed into the next state); `null` while the last version has it.
    #[schema(required = true)]
    pub to: Option<u32>,
    /// The object as the remus model has it in this state. A table's is its
    /// head: the entity without its columns, constraints, indexes and
    /// policies (their own histories), plus `column_order`, the names of its
    /// columns in table order.
    #[schema(value_type = Object)]
    pub value: Json,
    /// What changed from the state before, when this one directly follows it
    /// (romulus-diff's changes, `{"field", "before", "after"}`, plus `name`,
    /// `schema`, `labels` and `attributes`). Empty for an addition, and for a
    /// table head whose only change is its columns' order (they say why).
    pub changes: Vec<ObjectChange>,
}

/// A change, as romulus-diff structures it, or one it has no field for.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
#[serde(untagged)]
pub enum ObjectChange {
    Entity(EntityChange),
    Column(ColumnChange),
    Constraint(ConstraintChange),
    Index(IndexChange),
    Policy(PolicyChange),
    Partition(PartitionChange),
    Enum(EnumChange),
    Domain(DomainChange),
    Composite(CompositeChange),
    Extension(ExtensionChange),
    Other(OtherChange),
}

/// Changes romulus-diff reports outside its per-object change lists.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(tag = "field", rename_all = "snake_case")]
pub enum OtherChange {
    /// Renamed (reported by the diff as a rename, see [`reported_renames`]).
    Name {
        #[schema(required = true)]
        before: String,
        #[schema(required = true)]
        after: String,
    },
    /// Moved to another schema along with a rename.
    Schema {
        #[schema(required = true)]
        before: String,
        #[schema(required = true)]
        after: String,
    },
    /// An enum's labels, in order.
    Labels {
        #[schema(required = true)]
        before: Vec<String>,
        #[schema(required = true)]
        after: Vec<String>,
    },
    /// A composite type's attributes, `name type` each, in order.
    Attributes {
        #[schema(required = true)]
        before: Vec<String>,
        #[schema(required = true)]
        after: Vec<String>,
    },
}

/// A table, view, materialized view or foreign table, and its parts.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct TableHistory {
    /// Unique in the timeline: `schema.name` of its last state (a suffix
    /// `#2`… in the rare case two objects end under one name).
    pub id: String,
    /// Its schema and name in its last state.
    pub schema: String,
    pub name: String,
    pub states: Vec<ObjectState>,
    pub columns: Vec<PartHistory>,
    pub constraints: Vec<PartHistory>,
    pub indexes: Vec<PartHistory>,
    pub policies: Vec<PartHistory>,
}

/// A column, constraint, index or policy of a table: named within it.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PartHistory {
    /// Unique within its table and kind: its name in its last state.
    pub id: String,
    pub states: Vec<ObjectState>,
}

/// An enum, a domain or a composite type.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct TypeHistory {
    pub kind: UserTypeKind,
    pub schema: String,
    pub name: String,
    pub states: Vec<ObjectState>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum UserTypeKind {
    Enum,
    Domain,
    Composite,
}

/// A partition (by its own schema and name) or an extension (by name; no
/// schema).
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ObjectHistory {
    #[schema(required = true)]
    pub schema: Option<String>,
    pub name: String,
    pub states: Vec<ObjectState>,
}

/// Every object a history ever had, with its states.
#[derive(Debug, Clone, Default, Serialize, ToSchema)]
pub struct History {
    /// By schema, then name.
    pub tables: Vec<TableHistory>,
    pub types: Vec<TypeHistory>,
    pub partitions: Vec<ObjectHistory>,
    pub extensions: Vec<ObjectHistory>,
}

// ---- Renames ----------------------------------------------------------------

/// Renames between two versions: pairs the diff reports as one object
/// renamed rather than one removed and another added.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Renames {
    /// A table (or view…) of the old version, and its name in the new one.
    pub tables: Vec<(Relation, Relation)>,
    pub columns: Vec<ColumnRename>,
}

/// A column renamed in a table (named as in the new version).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnRename {
    pub table: Relation,
    pub from: String,
    pub to: String,
}

/// The renames `diff` reports.
///
/// romulus-diff matches objects by name and reports none yet: a rename is a
/// removal and an addition (docs/MVP.md, "Diff"). This is where rename
/// detection plugs in when the diff learns it: map what it reports here and
/// the time machine shows renames as renames, histories continued under the
/// new name (`step_with`'s tests show what that looks like).
pub fn reported_renames(diff: &SchemaDiff) -> Renames {
    let _ = diff;
    Renames::default()
}

// ---- Steps --------------------------------------------------------------------

/// A schema-qualified name, or a bare one (parts, extensions: `schema` empty).
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Key {
    pub schema: String,
    pub name: String,
}

impl Key {
    fn of(schema: &str, name: &str) -> Self {
        Self {
            schema: schema.to_owned(),
            name: name.to_owned(),
        }
    }

    fn bare(name: &str) -> Self {
        Self::of("", name)
    }

    fn relation(r: &Relation) -> Self {
        Self::of(&r.schema, &r.name)
    }
}

/// What happened to one object between two versions: added (`old` is
/// `None`), removed (`new` is `None`), changed (the same name) or renamed.
#[derive(Debug, Clone)]
pub struct Event {
    pub old: Option<Key>,
    pub new: Option<Key>,
    /// The object in the newer version; `None` when removed, and for a table
    /// head that did not change (only its parts did).
    pub value: Option<Json>,
    pub changes: Vec<ObjectChange>,
}

impl Event {
    fn added(key: Key, value: Json) -> Self {
        Self {
            old: None,
            new: Some(key),
            value: Some(value),
            changes: Vec::new(),
        }
    }

    fn removed(key: Key) -> Self {
        Self {
            old: Some(key),
            new: None,
            value: None,
            changes: Vec::new(),
        }
    }

    fn changed(key: Key, value: Option<Json>, changes: Vec<ObjectChange>) -> Self {
        Self {
            old: Some(key.clone()),
            new: Some(key),
            value,
            changes,
        }
    }

    /// In the order a step is applied: removals free names, renames take
    /// them, then changes, then additions.
    fn rank(&self) -> u8 {
        match (&self.old, &self.new) {
            (Some(_), None) => 0,
            (Some(a), Some(b)) if a != b => 1,
            (Some(_), Some(_)) => 2,
            _ => 3,
        }
    }
}

/// A table's head and its parts.
#[derive(Debug, Clone)]
pub struct TableEvent {
    pub head: Event,
    pub parts: Vec<(Part, Event)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Part {
    Column,
    Constraint,
    Index,
    Policy,
}

const PARTS: [Part; 4] = [Part::Column, Part::Constraint, Part::Index, Part::Policy];

/// Everything that changed from one version to the next.
#[derive(Debug, Clone, Default)]
pub struct Step {
    pub tables: Vec<TableEvent>,
    pub types: Vec<(UserTypeKind, Event)>,
    pub partitions: Vec<Event>,
    pub extensions: Vec<Event>,
}

impl Step {
    /// About what it holds, in bytes: the cache's budget counts it.
    pub fn weight(&self) -> usize {
        let event = |e: &Event| 96 + e.value.as_ref().map_or(0, |v| v.get().len());
        let tables: usize = self
            .tables
            .iter()
            .map(|t| event(&t.head) + t.parts.iter().map(|(_, e)| event(e)).sum::<usize>())
            .sum();
        let types: usize = self.types.iter().map(|(_, e)| event(e)).sum();
        let others: usize = self
            .partitions
            .iter()
            .chain(&self.extensions)
            .map(event)
            .sum();
        64 + tables + types + others
    }
}

/// What changed from `old` to `new`, renames as romulus-diff reports them.
pub fn step(old: &Schema, new: &Schema) -> Step {
    let diff = romulus_diff::diff(old, new);
    step_with(old, new, &diff, &reported_renames(&diff))
}

fn raw<T: Serialize>(value: &T) -> Json {
    // Serialising remus's plain data types cannot fail.
    Arc::from(serde_json::value::to_raw_value(value).unwrap_or_else(|_| {
        serde_json::value::to_raw_value(&serde_json::Value::Null).expect("null serialises")
    }))
}

/// A table's head: the entity without its parts, with its columns' order.
fn head(entity: &m::Entity) -> Json {
    let mut value = serde_json::to_value(entity).unwrap_or_default();
    if let Some(map) = value.as_object_mut() {
        for key in ["columns", "constraints", "indexes", "policies"] {
            map.remove(key);
        }
        map.insert(
            "column_order".into(),
            entity.columns.iter().map(|c| c.name.as_str()).collect(),
        );
    }
    raw(&value)
}

fn same_column_order(a: &m::Entity, b: &m::Entity) -> bool {
    a.columns.len() == b.columns.len()
        && a.columns
            .iter()
            .zip(&b.columns)
            .all(|(x, y)| x.name == y.name)
}

/// [`step`] with the diff already computed and the renames given.
pub fn step_with(old: &Schema, new: &Schema, diff: &SchemaDiff, renames: &Renames) -> Step {
    let index = |schema: &Schema| -> HashMap<Key, usize> {
        schema
            .entities
            .iter()
            .enumerate()
            .map(|(i, e)| (Key::of(&e.schema, &e.name), i))
            .collect()
    };
    let (old_at, new_at) = (index(old), index(new));
    let status_of = |key: &Key| {
        diff.entities
            .iter()
            .find(|e| e.schema == key.schema && e.name == key.name)
            .map(|e| e.status)
    };
    // Renames of the old side's removed tables into the new side's added
    // ones; anything else the seam says is not a rename here.
    let mut table_renames = Vec::new();
    let mut renamed_old = HashSet::new();
    let mut renamed_new = HashSet::new();
    for (from, to) in &renames.tables {
        let (from, to) = (Key::relation(from), Key::relation(to));
        if status_of(&from) == Some(Status::Removed)
            && status_of(&to) == Some(Status::Added)
            && renamed_old.insert(from.clone())
            && renamed_new.insert(to.clone())
        {
            table_renames.push((from, to));
        }
    }
    let column_renames = |table: &Key| -> Vec<(String, String)> {
        renames
            .columns
            .iter()
            .filter(|r| Key::relation(&r.table) == *table)
            .map(|r| (r.from.clone(), r.to.clone()))
            .collect()
    };

    let mut step = Step::default();
    for e in &diff.entities {
        let key = Key::of(&e.schema, &e.name);
        match e.status {
            Status::Added if !renamed_new.contains(&key) => {
                if let Some(&i) = new_at.get(&key) {
                    step.tables.push(added_table(key, &new.entities[i]));
                }
            }
            Status::Removed if !renamed_old.contains(&key) => step.tables.push(TableEvent {
                head: Event::removed(key),
                parts: Vec::new(),
            }),
            Status::Changed => {
                let (Some(&b), Some(&a)) = (old_at.get(&key), new_at.get(&key)) else {
                    continue;
                };
                let (before, after) = (&old.entities[b], &new.entities[a]);
                let own = e
                    .changes
                    .iter()
                    .cloned()
                    .map(ObjectChange::Entity)
                    .collect();
                let value = (!e.changes.is_empty() || !same_column_order(before, after))
                    .then(|| head(after));
                step.tables.push(TableEvent {
                    head: Event::changed(key.clone(), value, own),
                    parts: parts(old, e, before, after, &column_renames(&key)),
                });
            }
            Status::Added | Status::Removed | Status::Unchanged => {}
        }
    }
    for (from, to) in table_renames {
        let (Some(&b), Some(&a)) = (old_at.get(&from), new_at.get(&to)) else {
            continue;
        };
        let (before, after) = (&old.entities[b], &new.entities[a]);
        step.tables
            .push(renamed_table(old, new, before, after, &column_renames(&to)));
    }

    step.partitions = partition_events(diff, new);
    step.types = type_events(old, new, diff);
    for x in &diff.extensions {
        let value = find(&new.extensions, |e| e.name == x.name).map(raw);
        let changes = x
            .changes
            .iter()
            .cloned()
            .map(ObjectChange::Extension)
            .collect();
        step.extensions
            .push(event(x.status, Key::bare(&x.name), value, changes));
    }
    step
}

/// Enums, domains and composite types: their labels and attributes as
/// changes of their own.
fn type_events(old: &Schema, new: &Schema, diff: &SchemaDiff) -> Vec<(UserTypeKind, Event)> {
    let mut out = Vec::new();
    for t in &diff.enums {
        let value = find(&new.enums, |x| x.schema == t.schema && x.name == t.name).map(raw);
        let mut changes: Vec<ObjectChange> =
            t.changes.iter().cloned().map(ObjectChange::Enum).collect();
        if t.status == Status::Changed && !t.labels.is_empty() {
            let labels = |s: &Schema| {
                find(&s.enums, |x| x.schema == t.schema && x.name == t.name)
                    .map(|x| x.labels.clone())
                    .unwrap_or_default()
            };
            changes.push(ObjectChange::Other(OtherChange::Labels {
                before: labels(old),
                after: labels(new),
            }));
        }
        out.push((
            UserTypeKind::Enum,
            event(t.status, Key::of(&t.schema, &t.name), value, changes),
        ));
    }
    for t in &diff.domains {
        let value = find(&new.domains, |x| x.schema == t.schema && x.name == t.name).map(raw);
        let changes = t
            .changes
            .iter()
            .cloned()
            .map(ObjectChange::Domain)
            .collect();
        out.push((
            UserTypeKind::Domain,
            event(t.status, Key::of(&t.schema, &t.name), value, changes),
        ));
    }
    for t in &diff.composites {
        let value = find(&new.composites, |x| {
            x.schema == t.schema && x.name == t.name
        })
        .map(raw);
        let mut changes: Vec<ObjectChange> = t
            .changes
            .iter()
            .cloned()
            .map(ObjectChange::Composite)
            .collect();
        if t.status == Status::Changed && !t.attributes.is_empty() {
            let attributes = |s: &Schema| {
                find(&s.composites, |x| x.schema == t.schema && x.name == t.name)
                    .map(|x| {
                        x.attributes
                            .iter()
                            .map(|a| format!("{} {}", a.name, a.r#type))
                            .collect()
                    })
                    .unwrap_or_default()
            };
            changes.push(ObjectChange::Other(OtherChange::Attributes {
                before: attributes(old),
                after: attributes(new),
            }));
        }
        out.push((
            UserTypeKind::Composite,
            event(t.status, Key::of(&t.schema, &t.name), value, changes),
        ));
    }
    out
}

fn find<T>(list: &[T], pred: impl Fn(&T) -> bool) -> Option<&T> {
    list.iter().find(|x| pred(x))
}

/// The event a status means.
fn event(status: Status, key: Key, value: Option<Json>, changes: Vec<ObjectChange>) -> Event {
    match (status, value) {
        (Status::Added, Some(value)) => Event::added(key, value),
        (Status::Removed, _) => Event::removed(key),
        (_, value) => Event::changed(key, value, changes),
    }
}

fn added_table(key: Key, entity: &m::Entity) -> TableEvent {
    let mut parts = Vec::new();
    for c in &entity.columns {
        parts.push((Part::Column, Event::added(Key::bare(&c.name), raw(c))));
    }
    for c in &entity.constraints {
        parts.push((Part::Constraint, Event::added(Key::bare(&c.name), raw(c))));
    }
    for i in &entity.indexes {
        parts.push((Part::Index, Event::added(Key::bare(&i.name), raw(i))));
    }
    for p in &entity.policies {
        parts.push((Part::Policy, Event::added(Key::bare(&p.name), raw(p))));
    }
    TableEvent {
        head: Event::added(key, head(entity)),
        parts,
    }
}

/// The part events of a table the diff reports changed, column renames
/// included.
fn parts(
    like: &Schema,
    e: &EntityDiff,
    before: &m::Entity,
    after: &m::Entity,
    column_renames: &[(String, String)],
) -> Vec<(Part, Event)> {
    let mut out = Vec::new();
    let status = |name: &str| e.columns.iter().find(|c| c.name == name).map(|c| c.status);
    let mut renamed_from = HashSet::new();
    let mut renamed_to = HashSet::new();
    for (from, to) in column_renames {
        let (Some(old_column), Some(new_column)) = (
            find(&before.columns, |c| c.name == *from),
            find(&after.columns, |c| c.name == *to),
        ) else {
            continue;
        };
        if status(from) != Some(Status::Removed)
            || status(to) != Some(Status::Added)
            || !renamed_from.insert(from.clone())
            || !renamed_to.insert(to.clone())
        {
            continue;
        }
        let mut changes = vec![ObjectChange::Other(OtherChange::Name {
            before: from.clone(),
            after: to.clone(),
        })];
        changes.extend(column_changes(like, before, old_column, new_column));
        out.push((
            Part::Column,
            Event {
                old: Some(Key::bare(from)),
                new: Some(Key::bare(to)),
                value: Some(raw(new_column)),
                changes,
            },
        ));
    }
    for c in &e.columns {
        if renamed_from.contains(&c.name) || renamed_to.contains(&c.name) {
            continue;
        }
        let value = find(&after.columns, |x| x.name == c.name).map(raw);
        let changes = c
            .changes
            .iter()
            .cloned()
            .map(ObjectChange::Column)
            .collect();
        out.push((
            Part::Column,
            event(c.status, Key::bare(&c.name), value, changes),
        ));
    }
    for c in &e.constraints {
        let value = find(&after.constraints, |x| x.name == c.name).map(raw);
        let changes = c
            .changes
            .iter()
            .cloned()
            .map(ObjectChange::Constraint)
            .collect();
        out.push((
            Part::Constraint,
            event(c.status, Key::bare(&c.name), value, changes),
        ));
    }
    for i in &e.indexes {
        let value = find(&after.indexes, |x| x.name == i.name).map(raw);
        let changes = i.changes.iter().cloned().map(ObjectChange::Index).collect();
        out.push((
            Part::Index,
            event(i.status, Key::bare(&i.name), value, changes),
        ));
    }
    for p in &e.policies {
        let value = find(&after.policies, |x| x.name == p.name).map(raw);
        let changes = p
            .changes
            .iter()
            .cloned()
            .map(ObjectChange::Policy)
            .collect();
        out.push((
            Part::Policy,
            event(p.status, Key::bare(&p.name), value, changes),
        ));
    }
    out
}

/// A one-table schema like `like`, for comparing two objects under one name.
fn alone(like: &Schema, entity: m::Entity) -> Schema {
    let mut schema = super::super::empty(like);
    schema.entities.push(entity);
    schema
}

/// How `new` differs from `old`, columns of `table` (in a model like
/// `like`), whatever their names.
fn column_changes(
    like: &Schema,
    table: &m::Entity,
    old: &m::Column,
    new: &m::Column,
) -> Vec<ObjectChange> {
    let shell = |column: m::Column| {
        let mut entity = table.clone();
        entity.columns = vec![column];
        entity.constraints.clear();
        entity.indexes.clear();
        entity.policies.clear();
        entity
    };
    let mut renamed = new.clone();
    renamed.name.clone_from(&old.name);
    let diff = romulus_diff::diff(
        &alone(like, shell(old.clone())),
        &alone(like, shell(renamed)),
    );
    diff.entities
        .iter()
        .flat_map(|e| &e.columns)
        .flat_map(|c| c.changes.iter().cloned().map(ObjectChange::Column))
        .collect()
}

/// A table the diff reports as removed under one name and added under
/// another, which the seam says is a rename.
fn renamed_table(
    old: &Schema,
    new: &Schema,
    before: &m::Entity,
    after: &m::Entity,
    column_renames: &[(String, String)],
) -> TableEvent {
    let from = Key::of(&before.schema, &before.name);
    let to = Key::of(&after.schema, &after.name);
    // The new table under the old name: what the diff then says is what
    // changed besides the name.
    let mut as_before = after.clone();
    as_before.schema.clone_from(&before.schema);
    as_before.name.clone_from(&before.name);
    let diff = romulus_diff::diff(&alone(old, before.clone()), &alone(new, as_before));
    let mut changes = Vec::new();
    if from.schema != to.schema {
        changes.push(ObjectChange::Other(OtherChange::Schema {
            before: from.schema.clone(),
            after: to.schema.clone(),
        }));
    }
    if from.name != to.name {
        changes.push(ObjectChange::Other(OtherChange::Name {
            before: from.name.clone(),
            after: to.name.clone(),
        }));
    }
    let parts = match diff.entities.first() {
        Some(e) => {
            changes.extend(e.changes.iter().cloned().map(ObjectChange::Entity));
            parts(old, e, before, after, column_renames)
        }
        None => Vec::new(),
    };
    TableEvent {
        head: Event {
            old: Some(from),
            new: Some(to),
            value: Some(head(after)),
            changes,
        },
        parts,
    }
}

/// Partitions, listed by the diff under the entity at the top of their tree
/// (or as orphans), as events keyed by their own name. One moved to another
/// tree is listed removed under one and added under the other: that is a
/// change of parent.
fn partition_events(diff: &SchemaDiff, new: &Schema) -> Vec<Event> {
    let listed: Vec<&PartitionDiff> = diff
        .entities
        .iter()
        .flat_map(|e| &e.partitions)
        .chain(&diff.orphan_partitions.partitions)
        .collect();
    let mut out: Vec<Event> = Vec::new();
    let mut at: HashMap<Key, usize> = HashMap::new();
    for p in listed {
        let key = Key::of(&p.schema, &p.name);
        let value = find(&new.partitions, |x| {
            x.child_schema == p.schema && x.child == p.name
        })
        .map(raw);
        let changes = p
            .changes
            .iter()
            .cloned()
            .map(ObjectChange::Partition)
            .collect();
        let e = event(p.status, key.clone(), value, changes);
        if let Some(&i) = at.get(&key) {
            // Removed here and added there (or the reverse): moved.
            let value = out[i].value.clone().or(e.value);
            out[i] = Event::changed(key, value, Vec::new());
            continue;
        }
        at.insert(key, out.len());
        out.push(e);
    }
    out
}

// ---- The fold ---------------------------------------------------------------

/// One object being replayed.
#[derive(Debug, Default)]
struct Track {
    key: Option<Key>,
    states: Vec<ObjectState>,
}

impl Track {
    fn is_open(&self) -> bool {
        self.states.last().is_some_and(|s| s.to.is_none())
    }

    fn close(&mut self, at: u32) {
        if let Some(last) = self.states.last_mut().filter(|s| s.to.is_none()) {
            last.to = Some(at);
        }
    }

    /// A new state from `at`, closing the current one there.
    fn begin(&mut self, at: u32, value: Json, changes: Vec<ObjectChange>) {
        // Changes only describe a state that directly follows another.
        let follows = self.is_open();
        self.close(at);
        self.states.push(ObjectState {
            from: at,
            to: None,
            value,
            changes: if follows { changes } else { Vec::new() },
        });
    }
}

/// Objects of one kind, by their current (or, removed, their last) name.
#[derive(Debug, Default)]
struct Tracks<T> {
    items: Vec<(Track, T)>,
    by_key: HashMap<Key, usize>,
}

impl<T: Default> Tracks<T> {
    /// Applies `event` at version `at`; the object's index, unless it was
    /// unknown and the event cannot start one (a removal).
    fn apply(&mut self, at: u32, event: &Event) -> Option<usize> {
        match (&event.old, &event.new) {
            (Some(old), None) => {
                let i = *self.by_key.get(old)?;
                self.items[i].0.close(at);
                Some(i)
            }
            (old, Some(new)) => {
                let found = old
                    .as_ref()
                    .and_then(|o| self.by_key.get(o).copied())
                    .or_else(|| self.by_key.get(new).copied());
                let i = found.unwrap_or_else(|| {
                    self.items.push((Track::default(), T::default()));
                    self.items.len() - 1
                });
                if let Some(old) = old.as_ref().filter(|o| *o != new)
                    && self.by_key.get(old) == Some(&i)
                {
                    self.by_key.remove(old);
                }
                self.by_key.insert(new.clone(), i);
                let track = &mut self.items[i].0;
                track.key = Some(new.clone());
                match &event.value {
                    Some(value) => track.begin(at, value.clone(), event.changes.clone()),
                    // A table whose head stayed as it was, or a defective
                    // event for an object never seen: nothing to record.
                    None if track.states.is_empty() => return None,
                    None => {}
                }
                Some(i)
            }
            (None, None) => None,
        }
    }

    fn close_all(&mut self, at: u32) {
        for (track, _) in &mut self.items {
            track.close(at);
        }
    }
}

/// A table's parts.
#[derive(Debug, Default)]
struct TableParts {
    columns: Tracks<()>,
    constraints: Tracks<()>,
    indexes: Tracks<()>,
    policies: Tracks<()>,
}

impl TableParts {
    fn of(&mut self, part: Part) -> &mut Tracks<()> {
        match part {
            Part::Column => &mut self.columns,
            Part::Constraint => &mut self.constraints,
            Part::Index => &mut self.indexes,
            Part::Policy => &mut self.policies,
        }
    }
}

fn ordered<'a, T>(events: impl Iterator<Item = (&'a Event, T)>) -> Vec<(&'a Event, T)> {
    let mut events: Vec<_> = events.collect();
    events.sort_by_key(|(e, _)| e.rank());
    events
}

/// Every object's states, from the steps of a history: `steps[i]` leads to
/// the version at index `i` (the first from an empty model).
pub fn fold(steps: &[&Step]) -> History {
    let mut tables: Tracks<TableParts> = Tracks::default();
    let mut types: HashMap<UserTypeKind, Tracks<()>> = HashMap::new();
    let mut partitions: Tracks<()> = Tracks::default();
    let mut extensions: Tracks<()> = Tracks::default();
    for (at, step) in steps.iter().enumerate() {
        let at = u32::try_from(at).unwrap_or(u32::MAX);
        let mut table_events: Vec<&TableEvent> = step.tables.iter().collect();
        table_events.sort_by_key(|t| t.head.rank());
        for table in table_events {
            let Some(i) = tables.apply(at, &table.head) else {
                continue;
            };
            let parts = &mut tables.items[i].1;
            if table.head.new.is_none() {
                for part in PARTS {
                    parts.of(part).close_all(at);
                }
                continue;
            }
            for (event, part) in ordered(table.parts.iter().map(|(p, e)| (e, *p))) {
                parts.of(part).apply(at, event);
            }
        }
        for (event, kind) in ordered(step.types.iter().map(|(k, e)| (e, *k))) {
            types.entry(kind).or_default().apply(at, event);
        }
        for (event, ()) in ordered(step.partitions.iter().map(|e| (e, ()))) {
            partitions.apply(at, event);
        }
        for (event, ()) in ordered(step.extensions.iter().map(|e| (e, ()))) {
            extensions.apply(at, event);
        }
    }
    finish(tables, types, partitions, extensions)
}

/// Unique ids: the last name, suffixed when another object ended under it.
fn unique(base: &str, seen: &mut HashMap<String, usize>) -> String {
    let n = seen.entry(base.to_owned()).or_insert(0);
    *n += 1;
    if *n == 1 {
        base.to_owned()
    } else {
        format!("{base}#{n}")
    }
}

fn part_histories(tracks: Tracks<()>) -> Vec<PartHistory> {
    let mut seen = HashMap::new();
    tracks
        .items
        .into_iter()
        .filter(|(t, ())| !t.states.is_empty())
        .map(|(t, ())| PartHistory {
            id: unique(&t.key.unwrap_or_default().name, &mut seen),
            states: t.states,
        })
        .collect()
}

fn finish(
    tables: Tracks<TableParts>,
    types: HashMap<UserTypeKind, Tracks<()>>,
    partitions: Tracks<()>,
    extensions: Tracks<()>,
) -> History {
    let mut tables: Vec<(Track, TableParts)> = tables
        .items
        .into_iter()
        .filter(|(t, _)| !t.states.is_empty())
        .collect();
    tables.sort_by(|a, b| a.0.key.cmp(&b.0.key));
    let mut seen = HashMap::new();
    let tables = tables
        .into_iter()
        .map(|(track, parts)| {
            let key = track.key.unwrap_or_default();
            TableHistory {
                id: unique(&format!("{}.{}", key.schema, key.name), &mut seen),
                schema: key.schema,
                name: key.name,
                states: track.states,
                columns: part_histories(parts.columns),
                constraints: part_histories(parts.constraints),
                indexes: part_histories(parts.indexes),
                policies: part_histories(parts.policies),
            }
        })
        .collect();
    let objects = |tracks: Tracks<()>, schema: bool| -> Vec<ObjectHistory> {
        let mut list: Vec<ObjectHistory> = tracks
            .items
            .into_iter()
            .filter(|(t, ())| !t.states.is_empty())
            .map(|(t, ())| {
                let key = t.key.unwrap_or_default();
                ObjectHistory {
                    schema: schema.then_some(key.schema),
                    name: key.name,
                    states: t.states,
                }
            })
            .collect();
        list.sort_by(|a, b| (&a.schema, &a.name).cmp(&(&b.schema, &b.name)));
        list
    };
    let mut types: Vec<TypeHistory> = types
        .into_iter()
        .flat_map(|(kind, tracks)| {
            tracks
                .items
                .into_iter()
                .filter(|(t, ())| !t.states.is_empty())
                .map(move |(t, ())| {
                    let key = t.key.unwrap_or_default();
                    TypeHistory {
                        kind,
                        schema: key.schema,
                        name: key.name,
                        states: t.states,
                    }
                })
        })
        .collect();
    types.sort_by(|a, b| (&a.schema, &a.name, a.kind).cmp(&(&b.schema, &b.name, b.kind)));
    History {
        tables,
        types,
        partitions: objects(partitions, true),
        extensions: objects(extensions, false),
    }
}

#[cfg(test)]
mod tests;
