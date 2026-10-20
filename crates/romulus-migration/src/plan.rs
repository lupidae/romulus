//! Planning the statements.
//!
//! Each statement gets a [`Phase`]; the migration is the statements sorted
//! by stage, then phase, then the order they were planned in. The phases are
//! the dependency order:
//!
//! 1. schemas and extensions the rest needs, then the renames (planned
//!    by `romulus_diff::resolve`, which hands over the old model as it is
//!    after them: every other statement names objects by their new names);
//! 2. what stands in the way, dropped: policies, views (dependents first),
//!    foreign keys, indexes and constraints that are removed, changed, or
//!    must be rebuilt around a change; tables whose kind changes;
//! 3. types: enums (new labels, or recreated), domains, composites;
//! 4. tables created, columns added and altered, partitions;
//! 5. constraints, indexes, then foreign keys (every table exists by then);
//! 6. views (dependencies first), row level security, policies, comments;
//! 7. last, in their own section: columns, tables and types dropped, then
//!    extensions.
//!
//! Dropping late keeps the destructive statements together at the end,
//! where nothing depends on them any more. Some drops cannot wait (a table
//! whose name a view takes, a partition whose bound a new one overlaps);
//! those are flagged where they stand.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write as _,
};

use remus_core::{
    Schema,
    emit::ident::{qualified, quote_ident, quote_literal},
    model::{
        Column, CompositeType, Constraint, ConstraintKind, DomainType, Entity, EntityKind,
        EnumType, Index, Partition, Policy, TypeKind,
    },
};
use romulus_diff::{Candidate, Confidence, Rename, RenameKind, Resolved};

use crate::{
    Hazard, Kind, Migration, Options, Section, Stage, Statement, Warning, WarningCode,
    cast::conversion,
    partitions::Moves,
    sql::{
        column_definition, create_policy, create_view, identity_when, mentions, owned_sequence,
        parenthesized, policy_roles, relation_keyword, same_sql, storage, table_name,
    },
    versions::{self, Feature, PARTITIONED_EXCLUSION},
};

/// A schema and a name: the identity of every schema-qualified object.
type Name = (String, String);

/// What the function bodies warning names before "and N more".
const BODIES_SHOWN: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Phase {
    Schemas,
    Extensions,
    Renames,
    /// Partitions that become tables of their own: detached before they
    /// (or their parent) change, their inherited objects first of all.
    EarlyDetach,
    DropPolicies,
    DropViews,
    DropForeignKeys,
    DropIndexes,
    DropConstraints,
    EarlyDrops,
    Types,
    Domains,
    Composites,
    Sequences,
    Tables,
    /// Identities and generation expressions removed, before the
    /// conversions they would block and the sequences that take their name.
    UnsetColumns,
    /// Columns dropped to be added again (below), before the conversions
    /// they would block.
    ReaddDrops,
    Columns,
    ReaddAdds,
    DetachPartitions,
    DropPartitions,
    CreatePartitions,
    AttachPartitions,
    Constraints,
    Indexes,
    ForeignKeys,
    Views,
    Security,
    Comments,
    DropColumns,
    DropTables,
    /// Composites, then domains, then enums: each may use the next.
    DropComposites,
    DropDomains,
    DropEnums,
    DropExtensions,
}

impl Phase {
    fn section(self) -> Section {
        use Phase as P;
        match self {
            P::Schemas | P::Extensions => Section::Schemas,
            P::Renames => Section::Renames,
            P::DropPolicies
            | P::DropViews
            | P::DropForeignKeys
            | P::DropIndexes
            | P::DropConstraints
            | P::EarlyDetach
            | P::EarlyDrops => Section::Dependents,
            P::Types | P::Domains | P::Composites => Section::Types,
            P::Sequences
            | P::Tables
            | P::UnsetColumns
            | P::ReaddDrops
            | P::Columns
            | P::ReaddAdds
            | P::DetachPartitions
            | P::DropPartitions
            | P::CreatePartitions
            | P::AttachPartitions => Section::Tables,
            P::Constraints | P::Indexes | P::ForeignKeys => Section::Keys,
            P::Views => Section::Views,
            P::Security => Section::Security,
            P::Comments => Section::Comments,
            P::DropColumns
            | P::DropTables
            | P::DropComposites
            | P::DropDomains
            | P::DropEnums
            | P::DropExtensions => Section::Removals,
        }
    }
}

struct Planned {
    phase: Phase,
    statement: Statement,
    /// What it needs of Postgres that its text does not show
    /// ([`versions::features`] reads the rest).
    needs: Vec<Feature>,
}

impl Statement {
    /// Deletes or changes data.
    fn destroys(&mut self, note: impl Into<String>) -> &mut Self {
        self.flag(Hazard::DeletesData, note)
    }

    /// Fails when existing rows break it.
    fn can_fail(&mut self, note: impl Into<String>) -> &mut Self {
        self.flag(Hazard::CanFail, note)
    }

    /// Lets more rows through to more roles.
    fn widens(&mut self, note: impl Into<String>) -> &mut Self {
        self.flag(Hazard::WidensAccess, note)
    }

    fn flag(&mut self, hazard: Hazard, note: impl Into<String>) -> &mut Self {
        self.hazard = Some(hazard);
        self.note = Some(note.into());
        self
    }

    fn noted(&mut self, note: impl Into<String>) -> &mut Self {
        self.note = Some(note.into());
        self
    }
}

/// The note on dropping a detached partition's guessed name.
const GUESSED_DROP: &str = "Named as Postgres names a partition's copy of its parent's key or index: if yours has another name (`\\d` the table), drop that one instead.";

fn name_of(schema: &str, name: &str) -> Name {
    (schema.to_owned(), name.to_owned())
}

fn dotted((schema, name): &Name) -> String {
    format!("{schema}.{name}")
}

/// Items of both sides paired by key, sorted by key.
fn pair<'a, T, K: Ord>(
    old: &'a [T],
    new: &'a [T],
    key: impl Fn(&'a T) -> K,
) -> Vec<(K, Option<&'a T>, Option<&'a T>)> {
    let mut map: BTreeMap<K, (Option<&'a T>, Option<&'a T>)> = BTreeMap::new();
    for item in old {
        map.entry(key(item)).or_default().0.get_or_insert(item);
    }
    for item in new {
        map.entry(key(item)).or_default().1.get_or_insert(item);
    }
    map.into_iter().map(|(k, (b, a))| (k, b, a)).collect()
}

/// What an entity is, as far as changing it in place goes: a table cannot
/// become partitioned, a view cannot become materialized.
fn class(entity: &Entity) -> (u8, Option<String>) {
    match entity.kind {
        EntityKind::Table => (0, None),
        EntityKind::PartitionedTable => {
            (1, entity.partition_key.as_deref().map(crate::sql::squash))
        }
        EntityKind::View => (2, None),
        EntityKind::MaterializedView => (3, None),
        EntityKind::ForeignTable => (4, None),
    }
}

/// Names in an order where each comes after the ones it depends on (among
/// `names`), and the ones caught in a cycle, which no order satisfies.
fn dependency_order(
    names: &BTreeSet<Name>,
    deps: impl Fn(&Name) -> Vec<Name>,
) -> (Vec<Name>, Vec<Name>) {
    let mut pending: BTreeMap<Name, BTreeSet<Name>> = names
        .iter()
        .map(|n| {
            let d = deps(n)
                .into_iter()
                .filter(|d| d != n && names.contains(d))
                .collect();
            (n.clone(), d)
        })
        .collect();
    let mut ordered = Vec::with_capacity(names.len());
    loop {
        let ready: Vec<Name> = pending
            .iter()
            .filter(|(_, d)| d.is_empty())
            .map(|(n, _)| n.clone())
            .collect();
        if ready.is_empty() {
            break;
        }
        for n in &ready {
            pending.remove(n);
        }
        for d in pending.values_mut() {
            for n in &ready {
                d.remove(n);
            }
        }
        ordered.extend(ready);
    }
    (ordered, pending.into_keys().collect())
}

pub(crate) struct Planner<'a> {
    old: &'a Schema,
    new: &'a Schema,
    options: &'a Options,
    old_entities: BTreeMap<Name, &'a Entity>,
    new_entities: BTreeMap<Name, &'a Entity>,
    /// In both, but their kind or partition key changed: dropped and
    /// created again.
    recreated: BTreeSet<Name>,
    /// Enums and domains recreated, and the name the old one is set aside
    /// under until the columns are converted.
    aside: BTreeMap<Name, String>,
    /// Per kept table, the columns whose type changes (or whose type is
    /// recreated).
    converted: BTreeMap<Name, BTreeSet<String>>,
    /// Per kept table, the columns dropped and added again, and why: a
    /// column that becomes generated, or a generated column reading a
    /// column whose type changes.
    readded: BTreeMap<Name, BTreeMap<String, String>>,
    /// Tables whose primary key, unique constraints or unique indexes are
    /// dropped or rebuilt: foreign keys to them go first.
    rekeyed: BTreeSet<Name>,
    /// Old views dropped (removed, or dropped to be created again).
    views_dropped: BTreeSet<Name>,
    /// Sequences already created by this migration.
    sequences: BTreeSet<String>,
    /// A removed table holds a name (its key's index, an index, a
    /// sequence) something new takes: removed tables go before anything is
    /// created, not last.
    early_removals: bool,
    /// Tables on either side of a foreign key added (quoted names).
    keys_checked: Vec<String>,
    /// Partitions detached as tables and tables attached as partitions.
    moves: Moves,
    /// Planning a detached partition: the names predicted for its keys and
    /// indexes (`Moves::guessed`), dropped `IF EXISTS`, kept `IF NOT EXISTS`.
    guessed: BTreeSet<String>,
    /// Planning a table about to be attached: its constraints (the parent's
    /// CHECKs among them) go in before the attach.
    attaching: bool,
    /// What `romulus_diff::resolve` decided: the renames to run, every
    /// candidate (the ones left off annotate their drop and add), and the
    /// views to build again around a rename the rewrite cannot follow.
    steps: Vec<Rename>,
    candidates: Vec<Candidate>,
    applied: Vec<Rename>,
    rebuild_views: BTreeSet<Name>,
    /// The schemas the old model has before its renames: a move into one
    /// it lacks creates it first.
    old_schemas: BTreeSet<String>,
    out: Vec<Planned>,
    warnings: Vec<Warning>,
}

impl<'a> Planner<'a> {
    pub(crate) fn new(
        old: &'a Schema,
        new: &'a Schema,
        options: &'a Options,
        moves: Moves,
        resolved: &Resolved,
        original: &Schema,
    ) -> Self {
        let index = |s: &'a Schema| {
            s.entities
                .iter()
                .map(|e| (name_of(&e.schema, &e.name), e))
                .collect::<BTreeMap<_, _>>()
        };
        Self {
            old,
            new,
            options,
            old_entities: index(old),
            new_entities: index(new),
            recreated: BTreeSet::new(),
            aside: BTreeMap::new(),
            converted: BTreeMap::new(),
            readded: BTreeMap::new(),
            rekeyed: BTreeSet::new(),
            views_dropped: BTreeSet::new(),
            sequences: BTreeSet::new(),
            early_removals: false,
            keys_checked: Vec::new(),
            moves,
            guessed: BTreeSet::new(),
            attaching: false,
            steps: resolved.steps.clone(),
            candidates: resolved.candidates.clone(),
            applied: resolved.renames.clone(),
            rebuild_views: resolved.rebuild_views.clone(),
            old_schemas: Self::schemas_of(original)
                .into_iter()
                .map(str::to_owned)
                .collect(),
            out: Vec::new(),
            warnings: Vec::new(),
        }
    }

    pub(crate) fn plan(mut self) -> Migration {
        self.classify();
        self.schemas();
        self.extensions();
        self.renames();
        self.enums();
        self.domains();
        self.composites();
        self.drop_views();
        self.entities();
        self.drop_removed_tables();
        self.partitions();
        self.create_views();
        self.unforce_for_foreign_keys();
        self.empty_schemas();
        self.renames_left_off();
        self.function_bodies();
        self.finish()
    }

    /// What function and trigger bodies may name, renamed or converted: the
    /// model has no bodies, and Postgres keeps them as written (a trigger
    /// follows its table, not `NEW.email`), so this is a warning, not a
    /// statement.
    fn function_bodies(&mut self) {
        let mut named: Vec<String> = self
            .applied
            .iter()
            .filter(|r| {
                !matches!(
                    r.kind,
                    RenameKind::Constraint | RenameKind::Index | RenameKind::Policy
                )
            })
            .map(|r| {
                let what = if r.final_schema() == r.schema {
                    "renamed"
                } else {
                    "moved"
                };
                format!("{} ({what})", r.describe())
            })
            .collect();
        for ((schema, table), columns) in &self.converted {
            let Some(entity) = self.new_entities.get(&name_of(schema, table)) else {
                continue;
            };
            for column in columns {
                if let Some(c) = entity.column(column) {
                    named.push(format!("{schema}.{table}.{column} (now {})", c.r#type));
                }
            }
        }
        if named.is_empty() {
            return;
        }
        let more = named.len().saturating_sub(BODIES_SHOWN);
        named.truncate(BODIES_SHOWN);
        let mut list = named.join(", ");
        if more > 0 {
            let _ = write!(list, " and {more} more");
        }
        self.warn(
            WarningCode::FunctionBodies,
            None,
            format!(
                "Functions and triggers are not in romulus's model, and Postgres does not rewrite their bodies: check any that use {list}."
            ),
        );
    }

    fn push(&mut self, phase: Phase, kind: Kind, sql: impl Into<String>) -> &mut Statement {
        self.out.push(Planned {
            phase,
            statement: Statement {
                sql: sql.into(),
                kind,
                hazard: None,
                destructive: false,
                note: None,
                alternative: None,
                stage: Stage::Transaction,
                section: phase.section(),
            },
            needs: Vec::new(),
        });
        &mut self.out.last_mut().expect("just pushed").statement
    }

    /// The statement just pushed needs `feature`.
    fn needs(&mut self, feature: Feature) {
        if let Some(last) = self.out.last_mut() {
            last.needs.push(feature);
        }
    }

    fn warn(&mut self, code: WarningCode, object: Option<String>, message: impl Into<String>) {
        self.warnings.push(Warning {
            code,
            message: message.into(),
            object,
        });
    }

    // ---- What changes how ---------------------------------------------------

    #[allow(clippy::too_many_lines)]
    fn classify(&mut self) {
        for (name, b) in &self.old_entities {
            if let Some(a) = self.new_entities.get(name)
                && class(b) != class(a)
            {
                self.recreated.insert(name.clone());
            }
        }

        // Enums that lost or reordered labels, domains whose base type
        // changed, and domains over a recreated enum.
        let taken: BTreeSet<Name> = self
            .old
            .enums
            .iter()
            .map(|e| name_of(&e.schema, &e.name))
            .chain(self.old.domains.iter().map(|d| name_of(&d.schema, &d.name)))
            .chain(
                self.old
                    .composites
                    .iter()
                    .map(|c| name_of(&c.schema, &c.name)),
            )
            .chain(self.new.enums.iter().map(|e| name_of(&e.schema, &e.name)))
            .chain(self.new.domains.iter().map(|d| name_of(&d.schema, &d.name)))
            .chain(
                self.new
                    .composites
                    .iter()
                    .map(|c| name_of(&c.schema, &c.name)),
            )
            .chain(self.old_entities.keys().cloned())
            .chain(self.new_entities.keys().cloned())
            .collect();
        let mut recreate = Vec::new();
        for (name, b, a) in pair(&self.old.enums, &self.new.enums, |e| {
            name_of(&e.schema, &e.name)
        }) {
            if let (Some(b), Some(a)) = (b, a)
                && label_additions(&b.labels, &a.labels).is_none()
            {
                recreate.push(name);
            }
        }
        for (name, b, a) in pair(&self.old.domains, &self.new.domains, |d| {
            name_of(&d.schema, &d.name)
        }) {
            // A check dropped needs its name, which the model does not
            // record: the domain is created again rather than guessed at.
            let names_recreated = |sql: &str| recreate.iter().any(|(_, e)| mentions(sql, e));
            if let (Some(b), Some(a)) = (b, a)
                && (!same_sql(&b.base_type, &a.base_type)
                    || names_recreated(&b.base_type)
                    || b.constraints.iter().any(|c| names_recreated(c))
                    || b.default.as_deref().is_some_and(names_recreated)
                    || b.constraints
                        .iter()
                        .any(|check| !a.constraints.iter().any(|c| same_sql(c, check))))
            {
                recreate.push(name);
            }
        }
        for name in recreate {
            let mut aside = format!("{}_old", name.1);
            let mut n = 1;
            while taken.contains(&(name.0.clone(), aside.clone())) {
                n += 1;
                aside = format!("{}_old{n}", name.1);
            }
            self.aside.insert(name, aside);
        }

        // Columns converted, or dropped and added again.
        let kept: Vec<Name> = self
            .old_entities
            .iter()
            .filter(|(n, e)| {
                e.kind.is_table()
                    && self.new_entities.contains_key(*n)
                    && !self.recreated.contains(*n)
            })
            .map(|(n, _)| n.clone())
            .collect();
        for name in kept {
            let (b, a) = (self.old_entities[&name], self.new_entities[&name]);
            for column in &b.columns {
                let Some(after) = a.column(&column.name) else {
                    continue;
                };
                let new_expression = column.generated.is_some()
                    && column.generated == after.generated
                    && (!same_opt_sql(column.default.as_ref(), after.default.as_ref())
                        || after
                            .default
                            .as_deref()
                            .is_some_and(|e| self.mentions_aside(e)));
                if readd(column, after) || (new_expression && !self.options.has(17)) {
                    let why = if column.generated.is_none() {
                        "It becomes generated, which Postgres cannot do in place: dropped and added again, its values replaced by the generated ones.".to_owned()
                    } else if new_expression {
                        format!(
                            "Its expression changes, and PostgreSQL {} has no SET EXPRESSION (17 and later do): dropped and added again, its values computed anew.",
                            self.options.major.unwrap_or_default()
                        )
                    } else {
                        "Its generated storage changes, which Postgres cannot do in place: dropped and added again.".to_owned()
                    };
                    self.readded
                        .entry(name.clone())
                        .or_default()
                        .insert(column.name.clone(), why);
                } else if column.r#type != after.r#type || self.type_recreated(after) {
                    self.converted
                        .entry(name.clone())
                        .or_default()
                        .insert(column.name.clone());
                }
            }
        }

        let old_parts: BTreeMap<Name, &Partition> = self
            .old
            .partitions
            .iter()
            .map(|p| (name_of(&p.child_schema, &p.child), p))
            .collect();
        // Names a removed table holds that something new takes.
        let taken_by = |e: &Entity| -> Vec<Name> {
            let mut names: Vec<Name> = e
                .constraints
                .iter()
                .filter(|c| c.kind != ConstraintKind::ForeignKey && c.kind != ConstraintKind::Check)
                .map(|c| name_of(&e.schema, &c.name))
                .chain(e.indexes.iter().map(|i| name_of(&e.schema, &i.name)))
                .collect();
            names.extend(
                e.columns
                    .iter()
                    .filter_map(|c| c.default.as_deref().and_then(owned_sequence))
                    .map(|seq| match seq.split_once('.') {
                        Some((schema, name)) => name_of(schema, name),
                        None => name_of("public", seq),
                    }),
            );
            // Its partitions, at every level, go with it.
            let root = name_of(&e.schema, &e.name);
            names.extend(
                old_parts
                    .keys()
                    .filter(|child| {
                        tree_root(&old_parts, &self.old_entities, child).as_ref() == Some(&root)
                    })
                    .cloned(),
            );
            names
        };
        let wanted: BTreeSet<Name> = self
            .new
            .entities
            .iter()
            .flat_map(|e| {
                let mut names = taken_by(e);
                names.push(name_of(&e.schema, &e.name));
                names
            })
            .chain(
                self.new
                    .partitions
                    .iter()
                    .map(|p| name_of(&p.child_schema, &p.child)),
            )
            .collect();
        self.early_removals = self
            .old_entities
            .iter()
            .filter(|(n, e)| e.kind.is_table() && !self.new_entities.contains_key(*n))
            .any(|(_, e)| taken_by(e).iter().any(|n| wanted.contains(n)));

        // Generated columns reading a converted column block its
        // conversion: dropped before it, added again after.
        for (name, columns) in self.converted.clone() {
            let (b, a) = (self.old_entities[&name], self.new_entities[&name]);
            for generated in b.columns.iter().filter(|c| c.generated.is_some()) {
                let Some(expression) = &generated.default else {
                    continue;
                };
                let reads: Vec<&String> =
                    columns.iter().filter(|c| mentions(expression, c)).collect();
                if reads.is_empty()
                    || a.column(&generated.name)
                        .is_none_or(|c| c.generated.is_none())
                {
                    continue;
                }
                let why = format!(
                    "Postgres cannot convert a column a generated column reads: dropped before {} changes type, and added again after.",
                    reads
                        .iter()
                        .map(|c| c.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
                self.converted
                    .entry(name.clone())
                    .or_default()
                    .remove(&generated.name);
                self.readded
                    .entry(name.clone())
                    .or_default()
                    .insert(generated.name.clone(), why);
            }
        }
        self.converted.retain(|_, c| !c.is_empty());
        // A partition key's column cannot change type: the table is created
        // again (sub-partitions' keys count too).
        for (name, columns) in self.converted.clone() {
            let b = self.old_entities[&name];
            let keys: Vec<&str> = b
                .partition_key
                .iter()
                .map(String::as_str)
                .chain(
                    old_parts
                        .iter()
                        .filter(|(child, _)| {
                            tree_root(&old_parts, &self.old_entities, child).as_ref() == Some(&name)
                        })
                        .filter_map(|(_, p)| p.partition_key.as_deref()),
                )
                .collect();
            if columns.iter().any(|c| keys.iter().any(|k| mentions(k, c))) {
                self.converted.remove(&name);
                self.readded.remove(&name);
                self.recreated.insert(name);
            }
        }

        // Keys that change: foreign keys to them must go first.
        for (name, b) in &self.old_entities {
            let Some(a) = self.new_entities.get(name) else {
                continue;
            };
            if self.recreated.contains(name) || !b.kind.is_table() {
                continue;
            }
            let keys_changed = b
                .constraints
                .iter()
                .filter(|c| matches!(c.kind, ConstraintKind::PrimaryKey | ConstraintKind::Unique))
                .any(|c| {
                    a.constraints
                        .iter()
                        .find(|x| x.name == c.name)
                        .is_none_or(|x| self.constraint_changed(name, c, x))
                });
            let unique_indexes_changed = b.indexes.iter().filter(|i| i.unique).any(|i| {
                a.indexes
                    .iter()
                    .find(|x| x.name == i.name)
                    .is_none_or(|x| self.index_changed(name, i, x))
            });
            if keys_changed || unique_indexes_changed {
                self.rekeyed.insert(name.clone());
            }
        }
    }

    /// The column's type (or its element type) is an enum or domain being
    /// recreated.
    fn type_recreated(&self, column: &Column) -> bool {
        matches!(column.type_kind, TypeKind::Enum | TypeKind::Domain)
            && self
                .aside
                .contains_key(&name_of(&column.type_schema, &column.type_name))
    }

    /// The name of a recreated type that `sql` mentions, if any.
    fn mentions_aside(&self, sql: &str) -> bool {
        self.aside.keys().any(|(_, name)| mentions(sql, name))
    }

    fn constraint_changed(&self, table: &Name, b: &Constraint, a: &Constraint) -> bool {
        b.kind != a.kind
            || !same_sql(&b.definition, &a.definition)
            || self.rebuild_reason(table, &b.definition).is_some()
    }

    /// Why an unchanged constraint or index must be dropped and built
    /// again: it names a type recreated under it, or a column dropped and
    /// added again.
    fn rebuild_reason(&self, table: &Name, definition: &str) -> Option<String> {
        // Its expression holds the old type (renamed aside), which could
        // not be dropped under it.
        if let Some((schema, ty)) = self.aside.keys().find(|(_, t)| mentions(definition, t)) {
            return Some(format!(
                "Dropped above and built again: it names {schema}.{ty}, which is created again."
            ));
        }
        self.readded.get(table).and_then(|columns| {
            columns
                .keys()
                .find(|c| mentions(definition, c))
                .map(|c| format!("Dropped above and built again on {c}, which is added again."))
        })
    }

    fn index_changed(&self, table: &Name, b: &Index, a: &Index) -> bool {
        !same_sql(&b.definition, &a.definition)
            || b.unique != a.unique
            || b.partial != a.partial
            || self.mentions_aside(&b.definition)
            || self.rebuild_reason(table, &b.definition).is_some()
    }

    /// Whether what a foreign key references loses its values here: its
    /// table created again, or a column it references added again.
    fn fk_target_lost(&self, fk: &Constraint) -> bool {
        let (Some(schema), Some(table)) = (fk.ref_schema.as_deref(), fk.ref_table.as_deref())
        else {
            return false;
        };
        let target = name_of(schema, table);
        self.recreated.contains(&target)
            || self
                .readded
                .get(&target)
                .is_some_and(|columns| fk.ref_columns.iter().any(|c| columns.contains_key(c)))
    }

    /// Why a foreign key that did not change must be dropped and added
    /// again, if it must.
    fn fk_rebuilt(&self, owner: &Name, fk: &Constraint) -> Option<String> {
        let target = name_of(fk.ref_schema.as_deref()?, fk.ref_table.as_deref()?);
        if self.recreated.contains(&target) {
            return Some(format!("{} is created again", dotted(&target)));
        }
        if self.rekeyed.contains(&target) {
            return Some(format!(
                "the key it references on {} is rebuilt",
                dotted(&target)
            ));
        }
        let converted = |table: &Name, columns: &[String]| {
            self.converted
                .get(table)
                .is_some_and(|c| columns.iter().any(|x| c.contains(x)))
        };
        if converted(owner, &fk.columns) || converted(&target, &fk.ref_columns) {
            return Some("a column it joins changes type".to_owned());
        }
        None
    }

    // ---- Schemas and extensions --------------------------------------------

    fn schemas_of(schema: &Schema) -> BTreeSet<&str> {
        schema
            .entities
            .iter()
            .map(|e| e.schema.as_str())
            .chain(schema.enums.iter().map(|e| e.schema.as_str()))
            .chain(schema.domains.iter().map(|d| d.schema.as_str()))
            .chain(schema.composites.iter().map(|c| c.schema.as_str()))
            .chain(schema.partitions.iter().map(|p| p.child_schema.as_str()))
            .chain(schema.extensions.iter().map(|x| x.schema.as_str()))
            .collect()
    }

    fn schemas(&mut self) {
        let new: Vec<&str> = Self::schemas_of(self.new)
            .into_iter()
            .filter(|s| !self.old_schemas.contains(*s))
            .collect();
        for schema in new {
            if schema != "public" {
                let sql = format!("CREATE SCHEMA IF NOT EXISTS {};", quote_ident(schema));
                self.push(Phase::Schemas, Kind::CreateSchema, sql);
            }
        }
    }

    fn empty_schemas(&mut self) {
        let after = Self::schemas_of(self.new);
        let left: Vec<String> = self
            .old_schemas
            .iter()
            .filter(|s| *s != "public" && !after.contains(s.as_str()))
            .cloned()
            .collect();
        for schema in left {
            self.warn(
                WarningCode::EmptySchema,
                Some(schema.clone()),
                format!(
                    "Schema {schema} holds nothing romulus models any more. It is not dropped (it may hold functions): run DROP SCHEMA {} yourself if it is empty.",
                    quote_ident(&schema)
                ),
            );
        }
    }

    // ---- Renames -----------------------------------------------------------

    /// `ALTER … RENAME` for each rename applied, in `romulus_diff::resolve`'s
    /// order (through a temporary name where two objects swap names).
    fn renames(&mut self) {
        let steps = std::mem::take(&mut self.steps);
        for step in &steps {
            let key = step.key();
            let note = match self.candidates.iter().find(|c| c.key == key) {
                Some(c) => rename_note(c, &self.candidates),
                None => "Through a temporary name: two objects swap names.".to_owned(),
            };
            for sql in rename_statements(step) {
                self.push(Phase::Renames, Kind::Rename, sql)
                    .noted(note.clone());
            }
        }
        self.steps = steps;
    }

    /// Renames offered and left off: the drop and the add they would
    /// replace show the `RENAME` as their alternative, and a warning says
    /// what is lost meanwhile. Rivals (one column alike two others) share
    /// one note and one warning.
    fn renames_left_off(&mut self) {
        let off: Vec<Candidate> = self
            .candidates
            .iter()
            .filter(|c| !c.applied && c.follows.is_none())
            .cloned()
            .collect();
        // Per statement: whether it adds, what it keeps, the renames.
        let mut halves: BTreeMap<usize, (bool, &'static str, Vec<Rename>)> = BTreeMap::new();
        for c in &off {
            let r = &c.rename;
            let left = LeftOff::of(r);
            for (i, planned) in self.out.iter().enumerate() {
                let sql = &planned.statement.sql;
                let added = left
                    .adds
                    .iter()
                    .any(|prefix| sql.starts_with(prefix.as_str()));
                if added || left.drops.iter().any(|drop| sql == drop) {
                    halves
                        .entry(i)
                        .or_insert_with(|| (added, left.keeps, Vec::new()))
                        .2
                        .push(r.clone());
                }
            }
        }
        for (i, (added, keeps, renames)) in halves {
            let statement = &mut self.out[i].statement;
            let names = renames
                .iter()
                .map(|r| {
                    if added {
                        r.from.as_str()
                    } else {
                        r.to.as_str()
                    }
                })
                .collect::<Vec<_>>()
                .join(" or ");
            let hint = if added {
                format!("Possibly a rename of {names} (off): a RENAME keeps {keeps}.")
            } else {
                format!("Possibly renamed to {names} (off): a RENAME keeps {keeps}.")
            };
            statement.note = Some(match statement.note.take() {
                Some(own) => format!("{own} {hint}"),
                None => hint,
            });
            statement.alternative = Some(
                renames
                    .iter()
                    .flat_map(rename_statements)
                    .collect::<Vec<_>>()
                    .join("\n"),
            );
        }
        self.left_off_warnings(&off);
    }

    /// One warning per rename left off, or per group of rivals.
    fn left_off_warnings(&mut self, off: &[Candidate]) {
        let mut groups: Vec<Vec<&Candidate>> = Vec::new();
        for c in off {
            match groups.iter_mut().find(|g| {
                g.iter()
                    .any(|x| x.rivals.contains(&c.key) || c.rivals.contains(&x.key))
            }) {
                Some(group) => group.push(c),
                None => groups.push(vec![c]),
            }
        }
        for group in groups {
            let first = &group[0].rename;
            let left = LeftOff::of(first);
            let object = match &first.parent {
                Some(parent) => format!("{}.{parent}.{}", first.schema, first.from),
                None => format!("{}.{}", first.schema, first.from),
            };
            let message = if let [one] = group.as_slice() {
                format!(
                    "{} looks like a rename ({}), but the rename is off: {}. Turn it on to keep {}.",
                    one.rename.describe(),
                    lowercase_first(one.reason.trim_end_matches('.')),
                    left.instead,
                    left.keeps
                )
            } else {
                let at = match &first.parent {
                    Some(parent) => format!("{}.{parent}.", first.schema),
                    None => format!("{}.", first.schema),
                };
                let names = |f: fn(&Rename) -> &str| {
                    group
                        .iter()
                        .map(|c| f(&c.rename).to_owned())
                        .collect::<Vec<_>>()
                };
                let (froms, tos) = (names(|r| &r.from), names(|r| &r.to));
                let either = if froms.iter().all(|f| *f == froms[0]) {
                    format!("{at}{} looks renamed to {}", froms[0], tos.join(" or "))
                } else if tos.iter().all(|t| *t == tos[0]) {
                    format!("{at}{} looks renamed to {}", froms.join(" or "), tos[0])
                } else {
                    group
                        .iter()
                        .map(|c| c.rename.describe())
                        .collect::<Vec<_>>()
                        .join(" or ")
                };
                format!(
                    "{either}: equally alike, so neither is chosen and {}. Turn the right one on to keep {}.",
                    left.instead, left.keeps
                )
            };
            self.warn(WarningCode::PossibleRename, Some(object), message);
        }
    }

    fn extensions(&mut self) {
        let (old, new) = (self.old, self.new);
        let pairs = pair(&old.extensions, &new.extensions, |x| x.name.clone());
        let names: Vec<&str> = pairs.iter().map(|(n, _, _)| n.as_str()).collect();
        let order = crate::extensions::install_order(&names);
        let mut pairs: Vec<_> = pairs.iter().collect();
        pairs.sort_by_key(|(n, _, _)| order.iter().position(|o| o == n));
        // Drops in the reverse order: what requires another goes first.
        let mut drops = Vec::new();
        for (name, b, a) in pairs {
            let (name, b, a) = (name.clone(), *b, *a);
            let ext = quote_ident(&name).into_owned();
            match (b, a) {
                (None, Some(a)) => {
                    let sql = format!(
                        "CREATE EXTENSION IF NOT EXISTS {ext} WITH SCHEMA {};",
                        quote_ident(&a.schema)
                    );
                    self.push(Phase::Extensions, Kind::CreateExtension, sql);
                }
                (Some(_), None) => drops.push(ext),
                (Some(b), Some(a)) => {
                    if b.schema != a.schema {
                        let sql = format!(
                            "ALTER EXTENSION {ext} SET SCHEMA {};",
                            quote_ident(&a.schema)
                        );
                        self.push(Phase::Extensions, Kind::AlterExtension, sql);
                    }
                    if b.version != a.version {
                        let sql = format!(
                            "ALTER EXTENSION {ext} UPDATE TO {};",
                            quote_literal(&a.version)
                        );
                        self.push(Phase::Extensions, Kind::AlterExtension, sql)
                            .noted("Needs that version installed on the server.");
                    }
                }
                (None, None) => {}
            }
        }
        for ext in drops.into_iter().rev() {
            self.push(
                Phase::DropExtensions,
                Kind::DropExtension,
                format!("DROP EXTENSION {ext};"),
            )
            .noted("Fails while something still uses it.");
        }
    }

    // ---- Types ---------------------------------------------------------------

    fn enums(&mut self) {
        let (old, new) = (self.old, self.new);
        for (name, b, a) in pair(&old.enums, &new.enums, |e| name_of(&e.schema, &e.name)) {
            let ty = qualified(&name.0, &name.1);
            match (b, a) {
                (None, Some(a)) => {
                    self.push(Phase::Types, Kind::CreateType, create_enum(a));
                    if let Some(comment) = &a.comment {
                        self.comment("TYPE", &ty, Some(comment));
                    }
                }
                (Some(_), None) => {
                    self.push(Phase::DropEnums, Kind::DropType, format!("DROP TYPE {ty};"));
                }
                (Some(b), Some(a)) => {
                    let recreated = self.aside.contains_key(&name);
                    if let Some(aside) = self.aside.get(&name).cloned() {
                        self.recreate_enum(&name, b, a, &aside);
                    } else if let Some(additions) = label_additions(&b.labels, &a.labels) {
                        for (label, position) in additions {
                            let sql = format!(
                                "ALTER TYPE {ty} ADD VALUE {}{position};",
                                quote_literal(label)
                            );
                            self.push(Phase::Types, Kind::AlterType, sql);
                        }
                    }
                    if b.comment != a.comment || (recreated && a.comment.is_some()) {
                        self.comment("TYPE", &ty, a.comment.as_deref());
                    }
                }
                (None, None) => {}
            }
        }
    }

    fn recreate_enum(&mut self, name: &Name, b: &EnumType, a: &EnumType, aside: &str) {
        let ty = qualified(&name.0, &name.1);
        let removed: Vec<&str> = b
            .labels
            .iter()
            .filter(|l| !a.labels.contains(l))
            .map(String::as_str)
            .collect();
        let quoted = |labels: &[&str]| {
            labels
                .iter()
                .map(|l| quote_literal(l))
                .collect::<Vec<_>>()
                .join(", ")
        };
        // Label renames offered and left off: what would keep the type.
        let left_off: Vec<Rename> = self
            .candidates
            .iter()
            .filter(|c| {
                !c.applied
                    && c.rename.kind == RenameKind::EnumValue
                    && c.rename.schema == name.0
                    && c.rename.parent.as_deref() == Some(name.1.as_str())
            })
            .map(|c| c.rename.clone())
            .collect();
        let rename = (!left_off.is_empty()).then(|| {
            left_off
                .iter()
                .map(|r| {
                    format!(
                        "ALTER TYPE {ty} RENAME VALUE {} TO {};",
                        quote_literal(&r.from),
                        quote_literal(&r.to)
                    )
                })
                .collect::<Vec<_>>()
                .join("\n")
        });
        let why = if removed.is_empty() {
            "Postgres cannot reorder an enum's labels".to_owned()
        } else {
            format!(
                "Postgres cannot remove an enum label ({})",
                quoted(&removed)
            )
        };
        let statement = self.push(
            Phase::Types,
            Kind::AlterType,
            format!("ALTER TYPE {ty} RENAME TO {};", quote_ident(aside)),
        );
        statement.noted(format!(
            "{why}: the type is created again and every column using it converted."
        ));
        if let Some(rename) = &rename {
            statement.alternative = Some(rename.clone());
            statement.note = Some(format!(
                "{why}: the type is created again and every column using it converted. If {} was renamed to {}, the RENAME VALUE below does it in place instead (turn the rename on).",
                quoted(&left_off.iter().map(|r| r.from.as_str()).collect::<Vec<_>>()),
                quoted(&left_off.iter().map(|r| r.to.as_str()).collect::<Vec<_>>())
            ));
        }
        self.push(Phase::Types, Kind::CreateType, create_enum(a));
        self.push(
            Phase::DropEnums,
            Kind::DropType,
            format!("DROP TYPE {};", qualified(&name.0, aside)),
        );
        self.warn(
            WarningCode::TypeRecreated,
            Some(dotted(name)),
            if removed.is_empty() {
                format!("Enum {} changes the order of its labels, which Postgres cannot do in place: it is created again and its columns converted.", dotted(name))
            } else {
                format!(
                    "Enum {} loses {}: it is created again, and converting its columns fails on rows that still hold {}.",
                    dotted(name),
                    quoted(&removed),
                    if removed.len() == 1 { "it" } else { "one" }
                )
            },
        );
    }

    fn domains(&mut self) {
        let (old, new) = (self.old, self.new);
        for (name, b, a) in pair(&old.domains, &new.domains, |d| name_of(&d.schema, &d.name)) {
            let ty = qualified(&name.0, &name.1);
            match (b, a) {
                (None, Some(a)) => {
                    self.push(Phase::Domains, Kind::CreateDomain, create_domain(a));
                    if let Some(comment) = &a.comment {
                        self.comment("DOMAIN", &ty, Some(comment));
                    }
                }
                (Some(_), None) => {
                    self.push(
                        Phase::DropDomains,
                        Kind::DropDomain,
                        format!("DROP DOMAIN {ty};"),
                    );
                }
                (Some(b), Some(a)) => {
                    let recreated = self.aside.contains_key(&name);
                    if let Some(aside) = self.aside.get(&name).cloned() {
                        let over_recreated =
                            self.aside.keys().any(|(_, t)| mentions(&a.base_type, t));
                        self.push(
                            Phase::Domains,
                            Kind::AlterDomain,
                            format!("ALTER DOMAIN {ty} RENAME TO {};", quote_ident(&aside)),
                        )
                        .noted(if same_sql(&b.base_type, &a.base_type) && over_recreated {
                            format!(
                                "Its type {} is created again, so it is too, and every column using it converted.",
                                a.base_type
                            )
                        } else if same_sql(&b.base_type, &a.base_type) {
                            "A check goes, and dropping one needs its name, which the schema does not record: the domain is created again and every column using it converted.".to_owned()
                        } else {
                            format!(
                                "Postgres cannot change a domain's type ({} → {}): it is created again and every column using it converted.",
                                b.base_type, a.base_type
                            )
                        });
                        self.push(Phase::Domains, Kind::CreateDomain, create_domain(a));
                        self.push(
                            Phase::DropDomains,
                            Kind::DropDomain,
                            format!("DROP DOMAIN {};", qualified(&name.0, &aside)),
                        );
                        self.warn(
                            WarningCode::TypeRecreated,
                            Some(dotted(&name)),
                            format!(
                                "Domain {} is created again over {}, and its columns converted.",
                                dotted(&name),
                                a.base_type
                            ),
                        );
                    } else {
                        self.alter_domain(&name, b, a);
                    }
                    if b.comment != a.comment || (recreated && a.comment.is_some()) {
                        self.comment("DOMAIN", &ty, a.comment.as_deref());
                    }
                }
                (None, None) => {}
            }
        }
    }

    fn alter_domain(&mut self, name: &Name, b: &DomainType, a: &DomainType) {
        let ty = qualified(&name.0, &name.1);
        if b.default != a.default {
            let sql = match &a.default {
                Some(default) => format!("ALTER DOMAIN {ty} SET DEFAULT {default};"),
                None => format!("ALTER DOMAIN {ty} DROP DEFAULT;"),
            };
            self.push(Phase::Domains, Kind::AlterDomain, sql);
        }
        if b.not_null != a.not_null {
            if a.not_null {
                self.push(
                    Phase::Domains,
                    Kind::AlterDomain,
                    format!("ALTER DOMAIN {ty} SET NOT NULL;"),
                )
                .can_fail("Fails if a column of this domain holds a NULL.");
            } else {
                self.push(
                    Phase::Domains,
                    Kind::AlterDomain,
                    format!("ALTER DOMAIN {ty} DROP NOT NULL;"),
                );
            }
        }
        for check in &a.constraints {
            if !b.constraints.iter().any(|c| same_sql(c, check)) {
                self.push(
                    Phase::Domains,
                    Kind::AlterDomain,
                    format!("ALTER DOMAIN {ty} ADD {check};"),
                )
                .can_fail("Fails if a value of this domain already breaks it.");
            }
        }
    }

    fn composites(&mut self) {
        let (old, new) = (self.old, self.new);
        for (name, b, a) in pair(&old.composites, &new.composites, |c| {
            name_of(&c.schema, &c.name)
        }) {
            let ty = qualified(&name.0, &name.1);
            match (b, a) {
                (None, Some(a)) => {
                    self.push(Phase::Composites, Kind::CreateType, create_composite(a));
                    if let Some(comment) = &a.comment {
                        self.comment("TYPE", &ty, Some(comment));
                    }
                }
                (Some(_), None) => {
                    self.push(
                        Phase::DropComposites,
                        Kind::DropType,
                        format!("DROP TYPE {ty};"),
                    );
                }
                (Some(b), Some(a)) => {
                    self.alter_composite(&name, b, a);
                    if b.comment != a.comment {
                        self.comment("TYPE", &ty, a.comment.as_deref());
                    }
                }
                (None, None) => {}
            }
        }
    }

    fn alter_composite(&mut self, name: &Name, b: &CompositeType, a: &CompositeType) {
        let ty = qualified(&name.0, &name.1);
        let in_use = self.new.entities.iter().any(|e| {
            e.columns.iter().any(|c| {
                c.type_kind == TypeKind::Composite
                    && c.type_schema == name.0
                    && c.type_name == name.1
            })
        });
        let mut changed = false;
        for attribute in &b.attributes {
            if !a.attributes.iter().any(|x| x.name == attribute.name) {
                changed = true;
                self.push(
                    Phase::Composites,
                    Kind::AlterType,
                    format!(
                        "ALTER TYPE {ty} DROP ATTRIBUTE {};",
                        quote_ident(&attribute.name)
                    ),
                )
                .destroys("Deletes this attribute from every value of the type.");
            }
        }
        for attribute in &a.attributes {
            match b.attributes.iter().find(|x| x.name == attribute.name) {
                None => {
                    changed = true;
                    self.push(
                        Phase::Composites,
                        Kind::AlterType,
                        format!(
                            "ALTER TYPE {ty} ADD ATTRIBUTE {} {};",
                            quote_ident(&attribute.name),
                            attribute.r#type
                        ),
                    );
                }
                Some(before)
                    if before.r#type != attribute.r#type
                        || self.mentions_aside(&attribute.r#type) =>
                {
                    changed = true;
                    self.push(
                        Phase::Composites,
                        Kind::AlterType,
                        format!(
                            "ALTER TYPE {ty} ALTER ATTRIBUTE {} TYPE {};",
                            quote_ident(&attribute.name),
                            attribute.r#type
                        ),
                    )
                    .can_fail(if before.r#type == attribute.r#type {
                        format!(
                            "Points it at {}, which is created again: fails on a value that does not convert.",
                            attribute.r#type
                        )
                    } else {
                        format!(
                            "Converts {} to {}: fails on a value that does not convert.",
                            before.r#type, attribute.r#type
                        )
                    });
                }
                Some(_) => {}
            }
        }
        if changed && in_use {
            self.warn(
                WarningCode::Unsupported,
                Some(dotted(name)),
                format!(
                    "Composite type {} is used by a table column: Postgres refuses to change its attributes then. Convert those columns first.",
                    dotted(name)
                ),
            );
        }
        let kept: Vec<&str> = b
            .attributes
            .iter()
            .filter(|x| a.attributes.iter().any(|y| y.name == x.name))
            .chain(
                a.attributes
                    .iter()
                    .filter(|x| !b.attributes.iter().any(|y| y.name == x.name)),
            )
            .map(|x| x.name.as_str())
            .collect();
        let wanted: Vec<&str> = a.attributes.iter().map(|x| x.name.as_str()).collect();
        if kept != wanted {
            self.warn(
                WarningCode::ColumnOrder,
                Some(dotted(name)),
                format!(
                    "Type {} gets its attributes in another order ({}): Postgres adds them last and cannot reorder them.",
                    dotted(name),
                    kept.join(", ")
                ),
            );
        }
    }

    fn comment(&mut self, keyword: &str, target: &str, comment: Option<&str>) {
        let value = comment.map_or_else(|| "NULL".to_owned(), quote_literal);
        self.push(
            Phase::Comments,
            Kind::Comment,
            format!("COMMENT ON {keyword} {target} IS {value};"),
        );
    }

    // ---- Views -------------------------------------------------------------

    /// Old views to drop: removed, changed, or over something that changes
    /// under them (a table created again, a column converted, a view
    /// dropped, a type recreated).
    fn drop_views(&mut self) {
        let mut touched: BTreeSet<Name> = self.recreated.clone();
        touched.extend(self.converted.keys().cloned());
        touched.extend(self.readded.keys().cloned());
        touched.extend(
            self.old_entities
                .keys()
                .filter(|n| !self.new_entities.contains_key(*n))
                .cloned(),
        );
        let views: Vec<(&Name, &Entity)> = self
            .old_entities
            .iter()
            .filter(|(_, e)| e.kind.is_view())
            .map(|(n, e)| (n, *e))
            .collect();
        let mut dropped: BTreeSet<Name> = BTreeSet::new();
        for (name, view) in &views {
            // Its output columns too: a view over a renamed column keeps
            // its own names, which only `CREATE` again changes.
            let names = |e: &Entity| e.columns.iter().map(|c| c.name.clone()).collect::<Vec<_>>();
            let changed = match self.new_entities.get(*name) {
                None => true,
                Some(after) => {
                    after.kind != view.kind
                        || !same_sql(
                            view.view_definition.as_deref().unwrap_or(""),
                            after.view_definition.as_deref().unwrap_or(""),
                        )
                        || names(view) != names(after)
                }
            };
            let uses_aside = self.mentions_aside(view.view_definition.as_deref().unwrap_or(""));
            if changed || uses_aside || self.rebuild_views.contains(*name) {
                dropped.insert((*name).clone());
            }
        }
        loop {
            let more: Vec<Name> = views
                .iter()
                .filter(|(n, _)| !dropped.contains(*n))
                .filter(|(_, v)| {
                    v.depends_on.iter().any(|d| {
                        let d = name_of(&d.schema, &d.name);
                        touched.contains(&d) || dropped.contains(&d)
                    })
                })
                .map(|(n, _)| (*n).clone())
                .collect();
            if more.is_empty() {
                break;
            }
            dropped.extend(more);
        }

        let old_entities = &self.old_entities;
        let (order, cycle) = dependency_order(&dropped, |n| {
            old_entities[n]
                .depends_on
                .iter()
                .map(|d| name_of(&d.schema, &d.name))
                .collect()
        });
        // Dependents first.
        for name in order.into_iter().chain(cycle).rev() {
            let view = self.old_entities[&name];
            let sql = format!(
                "DROP {} {};",
                relation_keyword(&view.kind),
                table_name(view)
            );
            let again = self.new_entities.contains_key(&name) && !self.recreated.contains(&name);
            let statement = self.push(Phase::DropViews, Kind::DropView, sql);
            if again {
                statement.noted("Created again below: it changed, or something under it did.");
            }
        }
        self.views_dropped = dropped;
    }

    fn create_views(&mut self) {
        let to_create: BTreeSet<Name> = self
            .new_entities
            .iter()
            .filter(|(n, e)| {
                e.kind.is_view()
                    && (!self.old_entities.contains_key(*n)
                        || self.views_dropped.contains(*n)
                        || self.recreated.contains(*n))
            })
            .map(|(n, _)| n.clone())
            .collect();
        let new_entities = &self.new_entities;
        let (order, cycle) = dependency_order(&to_create, |n| {
            new_entities[n]
                .depends_on
                .iter()
                .map(|d| name_of(&d.schema, &d.name))
                .collect()
        });
        for name in order.into_iter().chain(cycle) {
            let view = self.new_entities[&name];
            let statement = self.push(Phase::Views, Kind::CreateView, create_view(view));
            if view.kind == EntityKind::MaterializedView {
                statement.noted(
                    "Runs its query to fill it. On large tables, add WITH NO DATA and REFRESH it later.",
                );
            }
            for index in &view.indexes {
                self.push(
                    Phase::Views,
                    Kind::CreateIndex,
                    format!("{};", index.definition),
                );
            }
            self.entity_comments(view, None);
        }
    }

    /// Comments of a created entity (`None`), or the ones that changed.
    fn entity_comments(&mut self, after: &Entity, before: Option<&Entity>) {
        let target = table_name(after);
        let keyword = relation_keyword(&after.kind);
        if before.map_or(after.comment.is_some(), |b| b.comment != after.comment) {
            self.comment(keyword, &target, after.comment.as_deref());
        }
        // A column dropped and added again lost its comment.
        let readded: BTreeSet<String> = self
            .readded
            .get(&name_of(&after.schema, &after.name))
            .map(|r| r.keys().cloned().collect())
            .unwrap_or_default();
        for column in &after.columns {
            let was = before
                .and_then(|b| b.column(&column.name))
                .filter(|_| !readded.contains(&column.name));
            let changed = match (before, was) {
                (None, _) | (Some(_), None) => column.comment.is_some(),
                (Some(_), Some(was)) => was.comment != column.comment,
            };
            if changed {
                let target = format!("{target}.{}", quote_ident(&column.name));
                self.comment("COLUMN", &target, column.comment.as_deref());
            }
        }
    }

    // ---- Tables ------------------------------------------------------------

    fn entities(&mut self) {
        let names: BTreeSet<Name> = self
            .old_entities
            .keys()
            .chain(self.new_entities.keys())
            .cloned()
            .collect();
        for name in names {
            let before = self.old_entities.get(&name).copied();
            let after = self.new_entities.get(&name).copied();
            let foreign =
                |e: Option<&Entity>| e.is_some_and(|e| e.kind == EntityKind::ForeignTable);
            if foreign(before) || foreign(after) {
                if before != after {
                    self.warn(
                        WarningCode::ForeignTable,
                        Some(dotted(&name)),
                        format!(
                            "Foreign table {} changed: foreign tables are not migrated (their servers are not modelled).",
                            dotted(&name)
                        ),
                    );
                }
                continue;
            }
            match (before, after) {
                (Some(b), Some(a)) if self.recreated.contains(&name) => {
                    if b.kind.is_table() && class(b) == class(a) {
                        self.push(
                            Phase::EarlyDrops,
                            Kind::DropTable,
                            format!("DROP TABLE {};", table_name(b)),
                        )
                        .destroys("A column of its partition key changes type, which Postgres cannot do in place: dropped and created again, empty. Copy its rows out first.");
                        self.warn(
                            WarningCode::TableRecreated,
                            Some(dotted(&name)),
                            format!(
                                "{} is dropped and created again, losing its rows: a column of its partition key changes type.",
                                dotted(&name)
                            ),
                        );
                    } else if b.kind.is_table() {
                        let note = if a.kind.is_table() {
                            format!(
                                "Postgres cannot turn {} into {} in place: it is dropped and created again, empty. Copy its rows out first.",
                                describe(b),
                                describe(a)
                            )
                        } else {
                            format!(
                                "{} becomes a view: the table and its rows go.",
                                dotted(&name)
                            )
                        };
                        self.push(
                            Phase::EarlyDrops,
                            Kind::DropTable,
                            format!("DROP TABLE {};", table_name(b)),
                        )
                        .destroys(note);
                        self.warn(
                            WarningCode::TableRecreated,
                            Some(dotted(&name)),
                            format!(
                                "{} changes from {} to {}: it is dropped and created again, losing its rows.",
                                dotted(&name),
                                describe(b),
                                describe(a)
                            ),
                        );
                    }
                    if a.kind.is_table() {
                        self.create_table(a);
                    }
                }
                (Some(b), Some(a)) if b.kind.is_table() => self.alter_table(&name, b, a),
                (Some(b), Some(a)) => self.alter_view(b, a),
                (None, Some(a)) if a.kind.is_table() => self.create_table(a),
                _ => {}
            }
        }
    }

    fn create_table(&mut self, table: &Entity) {
        let name = table_name(table);
        // Serial columns draw from a sequence that must exist first.
        for column in &table.columns {
            self.sequence_for(Phase::Sequences, column);
        }
        let mut lines: Vec<String> = table.columns.iter().map(column_definition).collect();
        lines.extend(
            table
                .constraints
                .iter()
                .filter(|c| c.kind != ConstraintKind::ForeignKey)
                .map(|c| format!("CONSTRAINT {} {}", quote_ident(&c.name), c.definition)),
        );
        let persistence = if table.unlogged { "UNLOGGED " } else { "" };
        let mut sql = format!("CREATE {persistence}TABLE {name} (");
        if lines.is_empty() {
            sql.push(')');
        } else {
            sql.push('\n');
            sql.push_str(
                &lines
                    .iter()
                    .map(|l| format!("    {l}"))
                    .collect::<Vec<_>>()
                    .join(",\n"),
            );
            sql.push_str("\n)");
        }
        if let Some(key) = &table.partition_key {
            sql.push_str(" PARTITION BY ");
            sql.push_str(key);
        }
        sql.push(';');
        self.push(Phase::Tables, Kind::CreateTable, sql);
        if table.partition_key.is_some()
            && table
                .constraints
                .iter()
                .any(|c| c.kind == ConstraintKind::Exclusion)
        {
            self.needs(PARTITIONED_EXCLUSION);
        }
        for column in &table.columns {
            self.own_sequence(Phase::Tables, &name, column);
        }
        for fk in table.foreign_keys() {
            if !fk_clone(self.new, table, fk) {
                self.add_constraint(&name, fk, None, None);
            }
        }
        for index in &table.indexes {
            self.push(
                Phase::Indexes,
                Kind::CreateIndex,
                format!("{};", index.definition),
            );
        }
        self.security(table, None);
        self.entity_comments(table, None);
        for constraint in &table.constraints {
            if let Some(comment) = &constraint.comment {
                self.constraint_comment(&name, constraint, Some(comment));
            }
        }
    }

    /// `CREATE SEQUENCE` for a serial column's sequence, once: before the
    /// tables for a new one (`Phase::Sequences`), where the column changes
    /// for an existing one (`Phase::Columns`: after an identity that held
    /// the name is gone).
    fn sequence_for(&mut self, phase: Phase, column: &Column) {
        let Some(sequence) = column.default.as_deref().and_then(owned_sequence) else {
            return;
        };
        if column.generated.is_some() || self.sequence_exists(sequence) {
            return;
        }
        if self.sequences.insert(sequence.to_owned()) {
            self.push(
                phase,
                Kind::CreateSequence,
                format!("CREATE SEQUENCE IF NOT EXISTS {sequence};"),
            );
        }
    }

    /// `setval` past the values an existing column holds, for a sequence
    /// this migration created for it: `nextval` would collide with them.
    fn catch_up(&mut self, table: &str, column: &Column) {
        let Some(sequence) = column.default.as_deref().and_then(owned_sequence) else {
            return;
        };
        if self.sequences.contains(sequence) {
            let col = quote_ident(&column.name);
            self.push(
                Phase::Columns,
                Kind::SetSequence,
                format!(
                    "SELECT setval({}, coalesce(max({col}), 0) + 1, false) FROM {table};",
                    quote_literal(sequence)
                ),
            )
            .noted("Moves the new sequence past the values the column already holds.");
        }
    }

    /// `ALTER SEQUENCE … OWNED BY`, for a sequence this migration created.
    fn own_sequence(&mut self, phase: Phase, table: &str, column: &Column) {
        let Some(sequence) = column.default.as_deref().and_then(owned_sequence) else {
            return;
        };
        if self.sequences.contains(sequence) && column.generated.is_none() {
            self.push(
                phase,
                Kind::AlterSequence,
                format!(
                    "ALTER SEQUENCE {sequence} OWNED BY {table}.{};",
                    quote_ident(&column.name)
                ),
            );
        }
    }

    /// A column of the new schema draws from it.
    fn sequence_in_use(&self, sequence: &str) -> bool {
        self.new.entities.iter().flat_map(|e| &e.columns).any(|c| {
            c.generated.is_none() && c.default.as_deref().and_then(owned_sequence) == Some(sequence)
        })
    }

    /// A column kept from the old schema draws from it, and so keeps
    /// owning it. A sequence owned by a column or table this migration
    /// drops is "created" (it exists: `IF NOT EXISTS`) and owned anew, or
    /// the drop would take it along.
    fn sequence_exists(&self, sequence: &str) -> bool {
        let draws = |c: &Column| c.default.as_deref().and_then(owned_sequence) == Some(sequence);
        self.old.entities.iter().any(|e| {
            let name = name_of(&e.schema, &e.name);
            let Some(after) = self.new_entities.get(&name) else {
                return false;
            };
            !self.recreated.contains(&name)
                && e.columns
                    .iter()
                    .any(|c| draws(c) && after.column(&c.name).is_some_and(draws))
        })
    }

    #[allow(clippy::too_many_lines)]
    fn security(&mut self, after: &Entity, before: Option<&Entity>) {
        let table = table_name(after);
        let (rls, forced) = before.map_or((false, false), |b| (b.rls, b.rls_forced));
        if after.rls != rls {
            let statement = self.push(
                Phase::Security,
                Kind::RowLevelSecurity,
                format!(
                    "ALTER TABLE {table} {} ROW LEVEL SECURITY;",
                    if after.rls { "ENABLE" } else { "DISABLE" }
                ),
            );
            if !after.rls {
                statement.widens("Its policies stop applying: every role with privileges on the table sees and changes all its rows.");
                self.widened(
                    after,
                    "row level security disabled: its policies stop applying",
                );
            }
        }
        if after.rls_forced != forced {
            let statement = self.push(
                Phase::Security,
                Kind::RowLevelSecurity,
                format!(
                    "ALTER TABLE {table} {} ROW LEVEL SECURITY;",
                    if after.rls_forced {
                        "FORCE"
                    } else {
                        "NO FORCE"
                    }
                ),
            );
            if !after.rls_forced {
                statement.widens("The table's owner bypasses its policies again.");
                self.widened(
                    after,
                    "row level security no longer forced: its owner bypasses the policies",
                );
            }
        }
        let old_policies = before.map_or(&[][..], |b| &b.policies[..]);
        let name = name_of(&after.schema, &after.name);
        let rebuild_all = self.converted.contains_key(&name) || self.readded.contains_key(&name);
        for (policy_name, b, a) in pair(old_policies, &after.policies, |p| p.name.clone()) {
            // Around a recreated type or table it mentions, too.
            let rebuild = rebuild_all
                || b.is_some_and(|b| {
                    self.mentions_aside(b.using.as_deref().unwrap_or(""))
                        || self.mentions_aside(b.check.as_deref().unwrap_or(""))
                        || self.recreated.iter().any(|(_, t)| policy_mentions(b, t))
                        || self
                            .old_entities
                            .keys()
                            .filter(|n| !self.new_entities.contains_key(*n))
                            .any(|(_, t)| policy_mentions(b, t))
                });
            let drop = |planner: &mut Self| {
                planner.push(
                    Phase::DropPolicies,
                    Kind::DropPolicy,
                    format!("DROP POLICY {} ON {table};", quote_ident(&policy_name)),
                );
            };
            match (b, a) {
                (Some(b), None) => {
                    drop(self);
                    let statement = &mut self.out.last_mut().expect("just pushed").statement;
                    if b.permissive {
                        statement.noted(format!(
                            "Narrows access: rows only this policy let through become invisible to {}.",
                            who(b)
                        ));
                    } else {
                        statement.widens(format!(
                            "A restrictive policy goes: the rows it kept out become visible to {}.",
                            who(b)
                        ));
                        self.widened(after, &format!("restrictive policy {policy_name} dropped"));
                    }
                }
                (None, Some(a)) => {
                    let grants = before.is_some() && a.permissive;
                    let statement = self.push(
                        Phase::Security,
                        Kind::CreatePolicy,
                        create_policy(&table, a),
                    );
                    if grants {
                        statement
                            .widens(format!("Grants {} the rows its expressions allow.", who(a)));
                        self.widened(after, &format!("permissive policy {policy_name} added"));
                    }
                }
                (Some(b), Some(a)) if rebuild => {
                    drop(self);
                    let statement = self.push(
                        Phase::Security,
                        Kind::CreatePolicy,
                        create_policy(&table, a),
                    );
                    if same_policy(b, a) {
                        statement
                            .noted("Dropped above and created again around a change it names.");
                    } else {
                        statement.widens(
                            "Changed: check its roles and expressions do not let more through.",
                        );
                        self.widened(after, &format!("policy {policy_name} changed"));
                    }
                }
                (Some(b), Some(a)) if !same_policy(b, a) => {
                    let altered = alter_policy(&table, b, a);
                    let statement = if let Some(sql) = altered {
                        self.push(Phase::Security, Kind::AlterPolicy, sql)
                    } else {
                        drop(self);
                        self.push(
                            Phase::Security,
                            Kind::CreatePolicy,
                            create_policy(&table, a),
                        )
                    };
                    statement.widens(
                        "Changed: check its roles and expressions do not let more through.",
                    );
                    self.widened(after, &format!("policy {policy_name} changed"));
                }
                _ => {}
            }
        }
    }

    /// A warning that `table` lets more through.
    fn widened(&mut self, table: &Entity, what: &str) {
        self.warn(
            WarningCode::WidensAccess,
            Some(format!("{}.{}", table.schema, table.name)),
            format!(
                "{}.{}: {what}. Check who can now read or change which rows.",
                table.schema, table.name
            ),
        );
    }

    fn alter_view(&mut self, b: &Entity, a: &Entity) {
        let name = name_of(&a.schema, &a.name);
        if self.views_dropped.contains(&name) {
            return;
        }
        // Indexes on a kept materialized view.
        self.indexes(&name, b, a);
        self.entity_comments(a, Some(b));
    }

    fn alter_table(&mut self, name: &Name, b: &Entity, a: &Entity) {
        let table = table_name(a);
        self.guessed = self
            .moves
            .guessed
            .get(name)
            .map(|g| g.iter().cloned().collect())
            .unwrap_or_default();
        self.attaching = self.moves.attached.contains(name);
        // Constraints.
        for (_, cb, ca) in pair(&b.constraints, &a.constraints, |c| c.name.clone()) {
            // Postgres makes and drops these with the key they copy.
            let cb = cb.filter(|c| !fk_clone(self.old, b, c));
            let ca = ca.filter(|c| !fk_clone(self.new, a, c));
            match (cb, ca) {
                (Some(cb), None) => self.drop_constraint(&table, cb),
                (None, Some(ca)) => {
                    let note = adding_note(ca);
                    self.add_constraint(&table, ca, Some(note), Some(Hazard::CanFail));
                    if let Some(comment) = &ca.comment {
                        self.constraint_comment(&table, ca, Some(comment));
                    }
                }
                (Some(cb), Some(ca)) => {
                    let rebuilt = if ca.kind == ConstraintKind::ForeignKey {
                        self.fk_rebuilt(name, cb)
                    } else {
                        None
                    };
                    if self.constraint_changed(name, cb, ca) {
                        let (note, hazard) =
                            if cb.kind == ca.kind && same_sql(&cb.definition, &ca.definition) {
                                (self.rebuild_reason(name, &cb.definition), None)
                            } else {
                                (Some(adding_note(ca)), Some(Hazard::CanFail))
                            };
                        self.drop_constraint(&table, cb);
                        self.add_constraint(&table, ca, note, hazard);
                        if let Some(comment) = &ca.comment {
                            self.constraint_comment(&table, ca, Some(comment));
                        }
                    } else if let Some(reason) = rebuilt {
                        self.drop_constraint(&table, cb);
                        // What it references is gone (its table created
                        // again, its columns added again): a row pointing at
                        // a value they held points nowhere now. Found by the
                        // tests on generated rows (romulus-migration-check).
                        let lost = self.fk_target_lost(cb);
                        self.add_constraint(
                            &table,
                            ca,
                            Some(if lost {
                                format!(
                                    "Dropped above and added back: {reason}. Fails if an existing row points at a value it held."
                                )
                            } else {
                                format!("Dropped above and added back: {reason}.")
                            }),
                            lost.then_some(Hazard::CanFail),
                        );
                        if let Some(comment) = &ca.comment {
                            self.constraint_comment(&table, ca, Some(comment));
                        }
                    } else if cb.comment != ca.comment {
                        self.constraint_comment(&table, ca, ca.comment.as_deref());
                    }
                }
                (None, None) => {}
            }
        }
        self.indexes(name, b, a);
        self.columns(name, b, a);
        if b.unlogged != a.unlogged {
            let verb = if a.unlogged { "UNLOGGED" } else { "LOGGED" };
            self.push(
                Phase::Columns,
                Kind::AlterTable,
                format!("ALTER TABLE {table} SET {verb};"),
            )
            .noted("Rewrites the whole table, locking it meanwhile.");
        }
        self.security(a, Some(b));
        self.entity_comments(a, Some(b));
        self.guessed.clear();
        self.attaching = false;
    }

    fn drop_constraint(&mut self, table: &str, constraint: &Constraint) {
        let phase = if constraint.kind == ConstraintKind::ForeignKey {
            Phase::DropForeignKeys
        } else {
            Phase::DropConstraints
        };
        let guessed = self.guessed.contains(&constraint.name);
        let statement = self.push(
            phase,
            Kind::DropConstraint,
            format!(
                "ALTER TABLE {table} DROP CONSTRAINT {}{};",
                if guessed { "IF EXISTS " } else { "" },
                quote_ident(&constraint.name)
            ),
        );
        if guessed {
            statement.noted(GUESSED_DROP);
        }
    }

    /// `hazard`: `can_fail` for a constraint new to a table that has rows
    /// (or changed), nothing for one created with its table or built again
    /// as it was.
    fn add_constraint(
        &mut self,
        table: &str,
        constraint: &Constraint,
        note: Option<String>,
        hazard: Option<Hazard>,
    ) {
        let phase = if constraint.kind == ConstraintKind::ForeignKey {
            self.keys_checked.push(table.to_owned());
            if let (Some(schema), Some(target)) = (&constraint.ref_schema, &constraint.ref_table) {
                self.keys_checked.push(qualified(schema, target));
            }
            Phase::ForeignKeys
        } else if self.attaching {
            Phase::Columns
        } else {
            Phase::Constraints
        };
        let statement = self.push(
            phase,
            Kind::AddConstraint,
            format!(
                "ALTER TABLE {table} ADD CONSTRAINT {} {};",
                quote_ident(&constraint.name),
                constraint.definition
            ),
        );
        statement.note = note;
        statement.hazard = hazard;
        let partitioned = self
            .new_entities
            .values()
            .any(|e| e.kind == EntityKind::PartitionedTable && table_name(e) == table);
        if constraint.kind == ConstraintKind::Exclusion && partitioned {
            self.needs(PARTITIONED_EXCLUSION);
        }
    }

    fn constraint_comment(&mut self, table: &str, constraint: &Constraint, comment: Option<&str>) {
        let target = format!("{} ON {table}", quote_ident(&constraint.name));
        self.comment("CONSTRAINT", &target, comment);
    }

    fn indexes(&mut self, name: &Name, b: &Entity, a: &Entity) {
        let dropped_columns: Vec<&str> = b
            .columns
            .iter()
            .filter(|c| a.column(&c.name).is_none())
            .map(|c| c.name.as_str())
            .collect();
        // Not around a column's conversion: the old index would be rebuilt
        // with it, inside the transaction, before a concurrent drop.
        let concurrent = self.options.concurrently
            && matches!(a.kind, EntityKind::Table | EntityKind::MaterializedView)
            && !self.converted.contains_key(name)
            && !self.readded.contains_key(name);
        for (index_name, ib, ia) in pair(&b.indexes, &a.indexes, |i| i.name.clone()) {
            let qualified_index = qualified(&name.0, &index_name);
            // Not an index on a column dropped meanwhile: the column takes it
            // along, inside the transaction, before a concurrent drop.
            let concurrently = concurrent
                && ia.is_some_and(|i| !i.unique)
                && ib.is_none_or(|i| {
                    !self.mentions_aside(&i.definition)
                        && !dropped_columns.iter().any(|c| mentions(&i.definition, c))
                });
            match (ib, ia) {
                (Some(_), None) => {
                    self.drop_index(&index_name, &qualified_index);
                }
                (None, Some(ia)) => self.create_index(ia, concurrently, true),
                (Some(ib), Some(ia)) if self.index_changed(name, ib, ia) => {
                    if concurrently {
                        let statement = self.push(
                            Phase::Indexes,
                            Kind::DropIndex,
                            format!("DROP INDEX CONCURRENTLY {qualified_index};"),
                        );
                        statement.stage = Stage::Concurrent;
                    } else {
                        self.drop_index(&index_name, &qualified_index);
                    }
                    let rebuilt = same_sql(&ib.definition, &ia.definition)
                        .then(|| self.rebuild_reason(name, &ib.definition))
                        .flatten();
                    self.create_index(ia, concurrently, true);
                    if let (Some(reason), false) = (rebuilt, concurrently) {
                        let statement = &mut self.out.last_mut().expect("just pushed").statement;
                        statement.note = Some(reason);
                        statement.hazard = None;
                    }
                }
                // A detached partition's copy of its parent's index exists,
                // unless the parent's was created ON ONLY: make sure.
                (Some(_), Some(ia)) if self.guessed.contains(&index_name) => {
                    let sql = ia
                        .definition
                        .replacen("CREATE INDEX ", "CREATE INDEX IF NOT EXISTS ", 1)
                        .replacen(
                            "CREATE UNIQUE INDEX ",
                            "CREATE UNIQUE INDEX IF NOT EXISTS ",
                            1,
                        );
                    self.push(Phase::Indexes, Kind::CreateIndex, format!("{sql};"))
                        .noted("The partition's copy of its parent's index, which a parent's index made ON ONLY lacks. Named as Postgres names it: under another name (`\\d` the table), that copy is enough and this one a duplicate.");
                }
                _ => {}
            }
        }
    }

    /// A detached partition's guessed name: `IF EXISTS`, and a note.
    fn drop_index(&mut self, name: &str, qualified_index: &str) {
        let guessed = self.guessed.contains(name);
        let statement = self.push(
            Phase::DropIndexes,
            Kind::DropIndex,
            format!(
                "DROP INDEX {}{qualified_index};",
                if guessed { "IF EXISTS " } else { "" }
            ),
        );
        if guessed {
            statement.noted(GUESSED_DROP);
        }
    }

    fn create_index(&mut self, index: &Index, concurrently: bool, existing_table: bool) {
        // A detached partition may hold it already: a table attached keeps
        // its own index as its copy of the parent's, under its own name.
        let detached = !self.guessed.is_empty();
        let mut sql = index.definition.clone();
        if concurrently {
            sql = sql.replacen(" INDEX ", " INDEX CONCURRENTLY ", 1);
        }
        if detached {
            sql = sql.replacen(
                " INDEX CONCURRENTLY ",
                " INDEX CONCURRENTLY IF NOT EXISTS ",
                1,
            );
            if !concurrently {
                sql = sql.replacen(" INDEX ", " INDEX IF NOT EXISTS ", 1);
            }
        }
        let statement = self.push(Phase::Indexes, Kind::CreateIndex, format!("{sql};"));
        if concurrently {
            statement.stage = Stage::Concurrent;
            statement.noted("Outside the transaction: CONCURRENTLY does not block writes, and cannot run in one. If it fails, drop the invalid index it leaves and run it again.");
        } else if existing_table && index.unique {
            statement.can_fail("Fails if existing rows hold duplicates.");
        }
        if detached && !concurrently && statement.note.is_none() {
            statement.noted("IF NOT EXISTS: a partition's copy of its parent's index may have this name already (a table attached keeps its own index as its copy).");
        }
    }

    // ---- Columns -----------------------------------------------------------

    #[allow(clippy::too_many_lines)]
    fn columns(&mut self, name: &Name, b: &Entity, a: &Entity) {
        let table = table_name(a);
        let removed: Vec<&Column> = b
            .columns
            .iter()
            .filter(|c| a.column(&c.name).is_none())
            .collect();
        for column in &a.columns {
            let Some(before) = b.column(&column.name) else {
                // Added.
                self.sequence_for(Phase::Columns, column);
                let statement = self.push(
                    Phase::Columns,
                    Kind::AddColumn,
                    format!(
                        "ALTER TABLE {table} ADD COLUMN {};",
                        column_definition(column)
                    ),
                );
                let fails = column.not_null
                    && column.default.is_none()
                    && column.identity.is_none()
                    && column.generated.is_none();
                if fails {
                    statement.can_fail("Fails if the table has rows: give it a DEFAULT, or add it nullable, fill it, then SET NOT NULL.");
                }
                self.own_sequence(Phase::Columns, &table, column);
                continue;
            };
            let col = quote_ident(&column.name).into_owned();
            let alter = format!("ALTER TABLE {table} ALTER COLUMN {col}");

            if let Some(why) = self
                .readded
                .get(name)
                .and_then(|r| r.get(&column.name))
                .cloned()
            {
                let statement = self.push(
                    Phase::ReaddDrops,
                    Kind::DropColumn,
                    format!("ALTER TABLE {table} DROP COLUMN {col};"),
                );
                if before.generated.is_none() {
                    statement.destroys(why);
                } else {
                    statement.noted(why);
                }
                self.push(
                    Phase::ReaddAdds,
                    Kind::AddColumn,
                    format!(
                        "ALTER TABLE {table} ADD COLUMN {};",
                        column_definition(column)
                    ),
                );
                continue;
            }

            // 1. Identity and generation removed, before any conversion
            //    (a generated column blocks the columns it reads) and any
            //    sequence taking the identity's name.
            if before.identity.is_some() && column.identity.is_none() {
                self.push(
                    Phase::UnsetColumns,
                    Kind::AlterColumn,
                    format!("{alter} DROP IDENTITY;"),
                )
                .noted("Drops the identity's sequence too.");
            }
            if before.generated.is_some() && column.generated.is_none() {
                self.push(
                    Phase::UnsetColumns,
                    Kind::AlterColumn,
                    format!("{alter} DROP EXPRESSION;"),
                )
                .noted("Keeps the values computed so far; new rows get the default instead.");
            }
            // 2. Type.
            let generated = column.generated.is_some();
            let mut default_done = false;
            if self
                .converted
                .get(name)
                .is_some_and(|c| c.contains(&column.name))
            {
                let recreated = self.type_recreated(column);
                let conversion = if recreated && column.type_kind == TypeKind::Domain {
                    let base = |s: &Schema| {
                        s.domains
                            .iter()
                            .find(|d| d.schema == column.type_schema && d.name == column.type_name)
                            .map(|d| d.base_type.clone())
                    };
                    let (from, to) = (
                        base(self.old).unwrap_or_default(),
                        base(self.new).unwrap_or_default(),
                    );
                    let mut c = conversion(&column.name, &from, false, &to, false);
                    c.using = if c.via_text || from == to {
                        Some(format!("{col}::text::{}", column.r#type))
                    } else {
                        c.using.map(|_| format!("{col}::{}", column.r#type))
                    };
                    c
                } else {
                    conversion(
                        &column.name,
                        &before.r#type,
                        before.type_kind == TypeKind::Enum,
                        &column.r#type,
                        column.type_kind == TypeKind::Enum,
                    )
                };
                let mut parts = Vec::new();
                let drop_default = before.default.is_some()
                    && before.generated.is_none()
                    && before.identity.is_none();
                if drop_default {
                    parts.push(format!("ALTER COLUMN {col} DROP DEFAULT"));
                }
                let using = conversion
                    .using
                    .as_ref()
                    .map(|u| format!(" USING {u}"))
                    .unwrap_or_default();
                parts.push(format!("ALTER COLUMN {col} TYPE {}{using}", column.r#type));
                if drop_default || before.default != column.default {
                    if let (Some(default), false) = (&column.default, generated) {
                        parts.push(format!("ALTER COLUMN {col} SET DEFAULT {default}"));
                    }
                    default_done = true;
                }
                let sql = if parts.len() == 1 {
                    format!("ALTER TABLE {table} {};", parts[0])
                } else {
                    format!("ALTER TABLE {table}\n    {};", parts.join(",\n    "))
                };
                self.sequence_for(Phase::Columns, column);
                let loses_labels = column.type_kind == TypeKind::Enum
                    && self.enum_loses_labels(&column.type_schema, &column.type_name);
                // A recreated domain refuses what the old one took: a NULL
                // (now NOT NULL), a value a new check or a narrower base
                // type refuses, a label its base enum lost. Found by the
                // tests on generated rows (romulus-migration-check).
                let domain = |s: &'a Schema| {
                    s.domains
                        .iter()
                        .find(|d| d.schema == column.type_schema && d.name == column.type_name)
                };
                let mut refuses: Vec<String> = Vec::new();
                if recreated
                    && column.type_kind == TypeKind::Domain
                    && let (Some(o), Some(n)) = (domain(self.old), domain(self.new))
                {
                    if n.base_type
                        .rsplit_once('.')
                        .is_some_and(|(schema, name)| self.enum_loses_labels(schema, name))
                    {
                        refuses.push(format!("a label {} lost", n.base_type));
                    }
                    if n.not_null && !o.not_null {
                        refuses.push("a NULL".to_owned());
                    }
                    if n.constraints.iter().any(|c| !o.constraints.contains(c)) {
                        refuses.push("a value its new check refuses".to_owned());
                    }
                    if !conversion.lossless && n.base_type != o.base_type {
                        refuses.push(format!("a value {} cannot hold", n.base_type));
                    }
                }
                let statement = self.push(Phase::Columns, Kind::AlterColumn, sql);
                if recreated {
                    let what = if column.type_kind == TypeKind::Domain {
                        "domain"
                    } else {
                        "enum"
                    };
                    if loses_labels {
                        statement.can_fail(format!(
                            "Converts to the recreated {what} {}: fails on a row holding a label it lost.",
                            column.r#type.trim_end_matches("[]")
                        ));
                    } else if !refuses.is_empty() && conversion.rounds {
                        statement.destroys(format!(
                            "Converts to the recreated domain {}: values can come out rounded or cut, and it fails on {}.",
                            column.r#type.trim_end_matches("[]"),
                            refuses.join(", ")
                        ));
                    } else if !refuses.is_empty() {
                        statement.can_fail(format!(
                            "Converts to the recreated domain {}: fails on {}.",
                            column.r#type.trim_end_matches("[]"),
                            refuses.join(", ")
                        ));
                    } else {
                        statement.noted(format!(
                            "Converts to the recreated {what} {}.",
                            column.r#type.trim_end_matches("[]")
                        ));
                    }
                } else if conversion.lossless {
                    statement.note.clone_from(&conversion.note);
                } else if conversion.rounds {
                    statement.destroys(conversion.note.clone().unwrap_or_default());
                } else {
                    statement.can_fail(conversion.note.clone().unwrap_or_default());
                }
                let via_text = conversion.via_text && !recreated;
                if via_text {
                    self.warn(
                        WarningCode::Cast,
                        Some(format!("{}.{}.{}", name.0, name.1, column.name)),
                        format!(
                            "{}.{} goes from {} to {} through text: the conversion fails unless every value reads as {}.",
                            name.1, column.name, before.r#type, column.r#type, column.r#type
                        ),
                    );
                }
                if a.partition_key
                    .as_deref()
                    .is_some_and(|k| mentions(k, &column.name))
                {
                    self.warn(
                        WarningCode::Unsupported,
                        Some(dotted(name)),
                        format!(
                            "{}.{} is in the partition key: Postgres cannot change its type. Recreate the table instead.",
                            name.1, column.name
                        ),
                    );
                }
                if default_done {
                    self.catch_up(&table, column);
                }
                self.own_sequence(Phase::Columns, &table, column);
            }
            // 3. Default (set again when it names a recreated type: the
            //    old one, renamed aside, could not be dropped under it).
            let names_aside = column
                .default
                .as_deref()
                .is_some_and(|d| self.mentions_aside(d));
            if !default_done
                && !generated
                && (!same_opt_sql(before.default.as_ref(), column.default.as_ref()) || names_aside)
            {
                match &column.default {
                    Some(default) => {
                        self.sequence_for(Phase::Columns, column);
                        self.push(
                            Phase::Columns,
                            Kind::AlterColumn,
                            format!("{alter} SET DEFAULT {default};"),
                        );
                        self.catch_up(&table, column);
                        self.own_sequence(Phase::Columns, &table, column);
                    }
                    None if before.default.is_some()
                        && (before.generated.is_none() || before.identity.is_some()) =>
                    {
                        self.push(
                            Phase::Columns,
                            Kind::AlterColumn,
                            format!("{alter} DROP DEFAULT;"),
                        );
                    }
                    None => {}
                }
            }
            // 4. Nullability.
            if before.not_null != column.not_null {
                if column.not_null {
                    self.push(
                        Phase::Columns,
                        Kind::AlterColumn,
                        format!("{alter} SET NOT NULL;"),
                    )
                    .can_fail("Fails if a row holds NULL here: fill those first.");
                } else {
                    self.push(
                        Phase::Columns,
                        Kind::AlterColumn,
                        format!("{alter} DROP NOT NULL;"),
                    );
                }
            }
            // 5. Identity and generation set.
            match (&before.identity, &column.identity) {
                (None, Some(identity)) => {
                    // A serial column's sequence goes: the identity makes
                    // its own, under the same name.
                    if let Some(sequence) = before.default.as_deref().and_then(owned_sequence)
                        && !self.sequence_in_use(sequence)
                    {
                        self.push(
                            Phase::Columns,
                            Kind::DropSequence,
                            format!("DROP SEQUENCE {sequence};"),
                        )
                        .noted("The serial sequence goes: the identity below makes its own, under the same name.");
                    }
                    self.push(
                        Phase::Columns,
                        Kind::AlterColumn,
                        format!(
                            "{alter} ADD GENERATED {} AS IDENTITY;",
                            identity_when(identity)
                        ),
                    );
                    self.push(
                        Phase::Columns,
                        Kind::SetSequence,
                        format!(
                            "SELECT setval(pg_get_serial_sequence({}, {}), coalesce(max({col}), 0) + 1, false) FROM {table};",
                            quote_literal(&table),
                            quote_literal(&column.name)
                        ),
                    )
                    .noted("The identity's sequence starts at 1: moved past the values the column already holds.");
                }
                (Some(x), Some(y)) if x != y => {
                    self.push(
                        Phase::Columns,
                        Kind::AlterColumn,
                        format!("{alter} SET GENERATED {};", identity_when(y)),
                    );
                }
                _ => {}
            }
            if let (Some(kind), Some(_), Some(expression)) =
                (&column.generated, &before.generated, &column.default)
                && (!same_opt_sql(before.default.as_ref(), column.default.as_ref())
                    || self.mentions_aside(expression))
            {
                self.push(
                    Phase::Columns,
                    Kind::AlterColumn,
                    format!("{alter} SET EXPRESSION AS ({expression});"),
                )
                .noted(format!(
                    "PostgreSQL 17 or later; it rewrites the table ({} column).",
                    storage(kind).to_lowercase()
                ));
            }
        }

        // Dropped: last, with the other removals; before the attach for a
        // table becoming a partition (it may have no column its parent lacks).
        let drop_phase = if self.moves.attached.contains(name) {
            Phase::Columns
        } else {
            Phase::DropColumns
        };
        // A generated column reads others: it goes first.
        let mut removed = removed;
        removed.sort_by_key(|c| c.generated.is_none());
        for column in &removed {
            let col = quote_ident(&column.name);
            let statement = self.push(
                drop_phase,
                Kind::DropColumn,
                format!("ALTER TABLE {table} DROP COLUMN {col};"),
            );
            statement.destroys("Deletes the column and its data.");
        }

        // Where the columns end up.
        let readded = self.readded.get(name);
        let mut order: Vec<&str> = b
            .columns
            .iter()
            .filter(|c| {
                a.column(&c.name).is_some() && !readded.is_some_and(|r| r.contains_key(&c.name))
            })
            .map(|c| c.name.as_str())
            .collect();
        for column in &a.columns {
            if !order.contains(&column.name.as_str()) {
                order.push(&column.name);
            }
        }
        let wanted: Vec<&str> = a.columns.iter().map(|c| c.name.as_str()).collect();
        // A partition's columns match its parent's by name, in any order.
        if order != wanted && !self.moves.attached.contains(name) {
            self.warn(
                WarningCode::ColumnOrder,
                Some(dotted(name)),
                format!(
                    "{}: Postgres adds columns last, so they end up as ({}). Only SELECT * and INSERT without a column list notice.",
                    dotted(name),
                    order.join(", ")
                ),
            );
        }
    }

    fn enum_loses_labels(&self, schema: &str, name: &str) -> bool {
        let find = |s: &'a Schema| {
            s.enums
                .iter()
                .find(|e| e.schema == schema && e.name == name)
        };
        match (find(self.old), find(self.new)) {
            (Some(b), Some(a)) => b.labels.iter().any(|l| !a.labels.contains(l)),
            _ => false,
        }
    }

    /// Tables only the old schema has, dropped last: tables with foreign
    /// keys to others first, cycles broken by dropping a key.
    fn drop_removed_tables(&mut self) {
        let removed: BTreeSet<Name> = self
            .old_entities
            .iter()
            .filter(|(n, e)| e.kind.is_table() && !self.new_entities.contains_key(*n))
            .map(|(n, _)| n.clone())
            .collect();
        // Their foreign keys that would block a drop earlier on.
        for name in &removed {
            let table = self.old_entities[name];
            for fk in table.foreign_keys() {
                if self.fk_rebuilt(name, fk).is_some() && !fk_clone(self.old, table, fk) {
                    self.drop_constraint(&table_name(table), fk);
                }
            }
        }
        let old_entities = &self.old_entities;
        // What each one must go before: the tables its foreign keys and its
        // policies' expressions refer to.
        let targets = |n: &Name| -> Vec<Name> {
            let table = old_entities[n];
            let mut out: Vec<Name> = table
                .foreign_keys()
                .filter_map(|fk| Some(name_of(fk.ref_schema.as_deref()?, fk.ref_table.as_deref()?)))
                .collect();
            out.extend(
                removed
                    .iter()
                    .filter(|other| *other != n && policies_mention(table, &other.1))
                    .cloned(),
            );
            out
        };
        // Referenced first, so reversed: referencing first.
        let (order, cycle) = dependency_order(&removed, targets);
        let cycle_set: BTreeSet<Name> = cycle.iter().cloned().collect();
        let phase = if self.early_removals {
            Phase::EarlyDrops
        } else {
            Phase::DropTables
        };
        for name in &cycle {
            let table = self.old_entities[name];
            for policy in &table.policies {
                if cycle_set
                    .iter()
                    .any(|other| other != name && policy_mentions(policy, &other.1))
                {
                    self.push(
                        phase,
                        Kind::DropPolicy,
                        format!(
                            "DROP POLICY {} ON {};",
                            quote_ident(&policy.name),
                            table_name(table)
                        ),
                    );
                }
            }
            for fk in table.foreign_keys() {
                let target = fk
                    .ref_schema
                    .as_deref()
                    .zip(fk.ref_table.as_deref())
                    .map(|(s, t)| name_of(s, t));
                if target.is_some_and(|t| &t != name && cycle_set.contains(&t))
                    && !fk_clone(self.old, table, fk)
                {
                    self.drop_constraint(&table_name(table), fk);
                }
            }
        }
        for name in cycle.into_iter().chain(order.into_iter().rev()) {
            let table = self.old_entities[&name];
            let what = if table.kind == EntityKind::PartitionedTable {
                "Deletes the table, its partitions and their rows."
            } else {
                "Deletes the table and its rows."
            };
            self.push(
                phase,
                Kind::DropTable,
                format!("DROP TABLE {};", table_name(table)),
            )
            .destroys(what);
        }
    }

    // ---- Partitions --------------------------------------------------------

    #[allow(clippy::too_many_lines)]
    fn partitions(&mut self) {
        let index = |s: &'a Schema| {
            s.partitions
                .iter()
                .map(|p| (name_of(&p.child_schema, &p.child), p))
                .collect::<BTreeMap<Name, &'a Partition>>()
        };
        let (old_parts, new_parts) = (index(self.old), index(self.new));
        // Partitions whose old tree's root goes (removed or recreated), and
        // whose new tree's root is created by this migration.
        let root_gone: BTreeSet<Name> = old_parts
            .keys()
            .filter(|n| {
                tree_root(&old_parts, &self.old_entities, n).is_some_and(|r| {
                    !self.new_entities.contains_key(&r) || self.recreated.contains(&r)
                })
            })
            .cloned()
            .collect();
        let root_fresh: BTreeSet<Name> = new_parts
            .keys()
            .filter(|n| {
                tree_root(&new_parts, &self.new_entities, n).is_some_and(|r| {
                    !self.old_entities.contains_key(&r) || self.recreated.contains(&r)
                })
            })
            .cloned()
            .collect();

        // Old partitions dropped (explicitly, or with an ancestor).
        let mut old_order: Vec<&Name> = old_parts.keys().collect();
        old_order.sort_by_key(|n| (tree_depth(&old_parts, n), (*n).clone()));
        let mut gone: BTreeSet<Name> = BTreeSet::new();
        let mut detach = Vec::new();
        let mut drop = Vec::new();
        for name in old_order {
            let o = old_parts[name];
            let parent = name_of(&o.parent_schema, &o.parent);
            if self.moves.detached.contains(name) {
                // A table of its own from now on, rows and all; whatever
                // differs is planned as for any table.
                self.push(
                    Phase::EarlyDetach,
                    Kind::DetachPartition,
                    format!(
                        "ALTER TABLE {} DETACH PARTITION {};",
                        qualified(&o.parent_schema, &o.parent),
                        qualified(&o.child_schema, &o.child)
                    ),
                )
                .noted("Kept, rows and all, as a table of its own.");
                let guessed = self.moves.guessed[name].clone();
                if !guessed.is_empty() {
                    let table = qualified(&o.child_schema, &o.child);
                    let names = guessed
                        .iter()
                        .map(|g| format!("`{g}`"))
                        .collect::<Vec<_>>()
                        .join(", ");
                    self.warn(
                        WarningCode::GuessedNames,
                        Some(table.clone()),
                        format!(
                            "`{table}` stops being a partition: the model has no names for its copies of `{}`'s keys and indexes, so the migration assumes the ones Postgres gives ({names}). Compare with `\\d {table}` on your database: a copy named otherwise is left as it is.",
                            qualified(&o.parent_schema, &o.parent)
                        ),
                    );
                }
                continue;
            }
            if root_gone.contains(name) || gone.contains(&parent) {
                gone.insert(name.clone());
                continue;
            }
            match new_parts.get(name) {
                None => {
                    gone.insert(name.clone());
                    drop.push((o, false));
                }
                Some(n)
                    if !same_opt_sql(o.partition_key.as_ref(), n.partition_key.as_ref())
                        || root_fresh.contains(name) =>
                {
                    gone.insert(name.clone());
                    drop.push((o, true));
                }
                Some(n) => {
                    let moved = o.parent_schema != n.parent_schema || o.parent != n.parent;
                    if moved || !same_opt_sql(o.bound.as_ref(), n.bound.as_ref()) {
                        detach.push(o);
                    }
                }
            }
        }
        for o in &detach {
            self.push(
                Phase::DetachPartitions,
                Kind::DetachPartition,
                format!(
                    "ALTER TABLE {} DETACH PARTITION {};",
                    qualified(&o.parent_schema, &o.parent),
                    qualified(&o.child_schema, &o.child)
                ),
            )
            .noted("Attached again below with its new bound.");
        }
        for (o, again) in &drop {
            let note = if *again {
                "Its partition key changes, which Postgres cannot do in place: dropped with its rows and created again below."
            } else {
                "Deletes the partition and its rows."
            };
            self.push(
                Phase::DropPartitions,
                Kind::DropTable,
                format!("DROP TABLE {};", qualified(&o.child_schema, &o.child)),
            )
            .destroys(note);
        }

        // New partitions, parents first; detached ones attached back.
        let mut new_order: Vec<&Name> = new_parts.keys().collect();
        new_order.sort_by_key(|n| (tree_depth(&new_parts, n), (*n).clone()));
        for name in new_order {
            let n = new_parts[name];
            let child = qualified(&n.child_schema, &n.child);
            let parent = qualified(&n.parent_schema, &n.parent);
            let bound = n.bound.as_deref().unwrap_or("DEFAULT");
            if self.moves.attached.contains(name) {
                self.push(
                    Phase::AttachPartitions,
                    Kind::AttachPartition,
                    format!("ALTER TABLE {parent} ATTACH PARTITION {child} {bound};"),
                )
                .can_fail("Attached with its rows: fails if one does not fit the bound (or a DEFAULT partition holds rows that belong here). Its columns were aligned with the parent's above.");
                continue;
            }
            let exists = old_parts.contains_key(name) && !gone.contains(name);
            if !exists {
                let mut sql = format!("CREATE TABLE {child} PARTITION OF {parent} {bound}");
                if let Some(key) = &n.partition_key {
                    sql.push_str(" PARTITION BY ");
                    sql.push_str(key);
                }
                sql.push(';');
                let has_default = self.new.partitions.iter().any(|p| {
                    p.parent_schema == n.parent_schema
                        && p.parent == n.parent
                        && p.bound.as_deref() == Some("DEFAULT")
                        && (p.child_schema != n.child_schema || p.child != n.child)
                        && old_parts.contains_key(&name_of(&p.child_schema, &p.child))
                });
                let statement = self.push(Phase::CreatePartitions, Kind::CreatePartition, sql);
                if has_default && !root_fresh.contains(name) {
                    statement.can_fail("Fails if the DEFAULT partition holds rows that belong here: move them out first.");
                }
            } else if detach
                .iter()
                .any(|o| name_of(&o.child_schema, &o.child) == *name)
            {
                self.push(
                    Phase::AttachPartitions,
                    Kind::AttachPartition,
                    format!("ALTER TABLE {parent} ATTACH PARTITION {child} {bound};"),
                )
                .can_fail("Scans the partition: fails if a row does not fit the new bound.");
            }
        }
    }

    /// Adding a foreign key checks the rows on both sides as the table
    /// owner, through the policies of a table that forces row level
    /// security, which fail when they read a setting the migration's
    /// session lacks. `FORCE` is lifted around the checks.
    fn unforce_for_foreign_keys(&mut self) {
        let forced: Vec<(Name, &Entity)> = self
            .old_entities
            .iter()
            .filter(|(n, e)| {
                e.rls
                    && e.rls_forced
                    && !e.policies.is_empty()
                    && self.new_entities.contains_key(*n)
                    && !self.recreated.contains(*n)
                    && self.keys_checked.contains(&table_name(e))
            })
            .map(|(n, e)| (n.clone(), *e))
            .collect();
        for (name, table) in forced {
            let table_sql = table_name(table);
            self.push(
                Phase::Constraints,
                Kind::RowLevelSecurity,
                format!("ALTER TABLE {table_sql} NO FORCE ROW LEVEL SECURITY;"),
            )
            .noted("Lifted while the foreign keys below check existing rows: as the table owner they would go through its policies, which can fail outside the application's session. Forced again after.");
            let still_forced = self.new_entities[&name].rls_forced;
            let no_force = format!("ALTER TABLE {table_sql} NO FORCE ROW LEVEL SECURITY;");
            // Its own NO FORCE (the new schema stops forcing) is done already:
            // this one carries its flag.
            let own = self
                .out
                .iter()
                .position(|p| p.phase == Phase::Security && p.statement.sql == no_force);
            if let Some(own) = own {
                let own = self.out.remove(own).statement;
                let lifted = &mut self
                    .out
                    .iter_mut()
                    .rev()
                    .find(|p| p.phase == Phase::Constraints && p.statement.sql == no_force)
                    .expect("pushed above")
                    .statement;
                lifted.hazard = own.hazard;
                lifted.note = own.note;
            }
            if still_forced {
                self.push(
                    Phase::Security,
                    Kind::RowLevelSecurity,
                    format!("ALTER TABLE {table_sql} FORCE ROW LEVEL SECURITY;"),
                )
                .noted("Forced again, now that the foreign keys are checked.");
            }
        }
    }

    // ---- Finishing ---------------------------------------------------------

    fn finish(mut self) -> Migration {
        // Enum values used later in the migration must be committed first.
        let mut commit: BTreeSet<String> = BTreeSet::new();
        for (i, planned) in self.out.iter().enumerate() {
            let s = &planned.statement;
            if s.kind != Kind::AlterType || !s.sql.contains(" ADD VALUE ") {
                continue;
            }
            let Some(label) = s.sql.split(" ADD VALUE ").nth(1).and_then(literal_prefix) else {
                continue;
            };
            let used = self.out.iter().enumerate().any(|(j, p)| {
                j != i
                    && !(p.statement.kind == Kind::AlterType
                        && p.statement.sql.contains(" ADD VALUE "))
                    && p.statement.sql.contains(&label)
            });
            if used {
                commit.insert(type_of_add_value(&s.sql).to_owned());
            }
        }
        for planned in &mut self.out {
            let s = &mut planned.statement;
            if s.kind == Kind::AlterType
                && s.sql.contains(" ADD VALUE ")
                && commit.contains(type_of_add_value(&s.sql))
            {
                s.stage = Stage::Commit;
                s.note = Some("Committed before the rest: Postgres refuses a value added in the same transaction (\"unsafe use of new value\"), and the migration uses it.".to_owned());
            }
        }
        for planned in &mut self.out {
            let s = &mut planned.statement;
            s.destructive = matches!(s.hazard, Some(Hazard::DeletesData | Hazard::CanFail));
        }
        self.guard_versions();
        self.out.sort_by_key(|p| (p.statement.stage, p.phase));
        Migration {
            statements: self.out.into_iter().map(|p| p.statement).collect(),
            warnings: self.warnings,
            major: self.options.major,
            renames: self.candidates,
            applied: self.applied,
        }
    }
}

impl Planner<'_> {
    /// What the statements need of a newer Postgres than the target major
    /// ([`Options::major`]): noted on each, one warning per feature.
    fn guard_versions(&mut self) {
        let Some(major) = self.options.major else {
            return;
        };
        let mut missing: BTreeMap<Feature, usize> = BTreeMap::new();
        for planned in &mut self.out {
            let mut needs = versions::features(planned.statement.kind, &planned.statement.sql);
            needs.extend(planned.needs.iter().copied());
            needs.retain(|f| f.since > major);
            needs.sort_by(|a, b| b.cmp(a));
            needs.dedup();
            if needs.is_empty() {
                continue;
            }
            let what = needs
                .iter()
                .map(|f| format!("{} (PostgreSQL {}+)", f.what, f.since))
                .collect::<Vec<_>>()
                .join(", ");
            let note = format!("Fails on PostgreSQL {major}: it uses {what}.");
            let s = &mut planned.statement;
            s.note = Some(match s.note.take() {
                Some(own) => format!("{own} {note}"),
                None => note,
            });
            for feature in needs {
                *missing.entry(feature).or_default() += 1;
            }
        }
        for (feature, count) in missing.into_iter().rev() {
            let statements = if count == 1 {
                "a statement".to_owned()
            } else {
                format!("{count} statements")
            };
            self.warn(
                WarningCode::NewerPostgres,
                None,
                format!(
                    "The new schema uses {}, which needs PostgreSQL {} or later; the database is on {major}, where {statements} of this migration fail. Change the schema, or move the database to PostgreSQL {}.",
                    feature.what, feature.since, feature.since
                ),
            );
        }
    }
}

fn policy_mentions(policy: &Policy, name: &str) -> bool {
    [&policy.using, &policy.check]
        .into_iter()
        .flatten()
        .any(|expr| mentions(expr, name))
}

fn policies_mention(table: &Entity, name: &str) -> bool {
    table.policies.iter().any(|p| policy_mentions(p, name))
}

/// The entity at the top of a partition's tree, following parents through
/// `parts`; `None` when the chain leads nowhere or loops.
fn tree_root(
    parts: &BTreeMap<Name, &Partition>,
    entities: &BTreeMap<Name, &Entity>,
    at: &Name,
) -> Option<Name> {
    let mut at = at.clone();
    let mut seen = BTreeSet::new();
    loop {
        if entities.contains_key(&at) {
            return Some(at);
        }
        if !seen.insert(at.clone()) {
            return None;
        }
        let p = parts.get(&at)?;
        at = name_of(&p.parent_schema, &p.parent);
    }
}

/// How many partitions up to the top of the tree (bounded, for cycles).
fn tree_depth(parts: &BTreeMap<Name, &Partition>, at: &Name) -> usize {
    let mut at = at.clone();
    let mut depth = 0;
    while let Some(p) = parts.get(&at) {
        depth += 1;
        if depth > parts.len() {
            break;
        }
        at = name_of(&p.parent_schema, &p.parent);
    }
    depth
}

/// `'it''s' AFTER 'x';` → `'it''s'`.
fn literal_prefix(text: &str) -> Option<String> {
    let mut chars = text.char_indices().peekable();
    if chars.next()?.1 != '\'' {
        return None;
    }
    while let Some((i, c)) = chars.next() {
        if c == '\'' {
            if chars.next_if(|(_, d)| *d == '\'').is_some() {
                continue;
            }
            return Some(text[..=i].to_owned());
        }
    }
    None
}

fn type_of_add_value(sql: &str) -> &str {
    sql.trim_start_matches("ALTER TYPE ")
        .split(" ADD VALUE ")
        .next()
        .unwrap_or("")
}

/// The labels to add, each with where it goes, when `new` is `old` with
/// labels inserted (and none removed or moved); `None` otherwise.
fn label_additions<'l>(old: &[String], new: &'l [String]) -> Option<Vec<(&'l str, String)>> {
    let kept: Vec<&String> = new.iter().filter(|l| old.contains(l)).collect();
    if kept.len() != old.len() || kept.iter().zip(old).any(|(a, b)| *a != b) {
        return None;
    }
    let mut current: Vec<&str> = old.iter().map(String::as_str).collect();
    let mut out = Vec::new();
    for (i, label) in new.iter().enumerate() {
        if old.contains(label) {
            continue;
        }
        let position = if i == 0 {
            match current.first() {
                Some(first) => format!(" BEFORE {}", quote_literal(first)),
                None => String::new(),
            }
        } else {
            let previous = new[i - 1].as_str();
            if current.last() == Some(&previous) {
                String::new()
            } else {
                format!(" AFTER {}", quote_literal(previous))
            }
        };
        let at = if i == 0 {
            0
        } else {
            current
                .iter()
                .position(|l| *l == new[i - 1])
                .map_or(current.len(), |p| p + 1)
        };
        current.insert(at, label);
        out.push((label.as_str(), position));
    }
    Some(out)
}

fn create_enum(e: &EnumType) -> String {
    let labels = e
        .labels
        .iter()
        .map(|l| quote_literal(l))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "CREATE TYPE {} AS ENUM ({labels});",
        qualified(&e.schema, &e.name)
    )
}

fn create_domain(d: &DomainType) -> String {
    let mut sql = format!(
        "CREATE DOMAIN {} AS {}",
        qualified(&d.schema, &d.name),
        d.base_type
    );
    if let Some(default) = &d.default {
        sql.push_str(" DEFAULT ");
        sql.push_str(default);
    }
    if d.not_null {
        sql.push_str(" NOT NULL");
    }
    for constraint in &d.constraints {
        sql.push(' ');
        sql.push_str(constraint);
    }
    sql.push(';');
    sql
}

fn create_composite(c: &CompositeType) -> String {
    let attributes = c
        .attributes
        .iter()
        .map(|a| format!("{} {}", quote_ident(&a.name), a.r#type))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "CREATE TYPE {} AS ({attributes});",
        qualified(&c.schema, &c.name)
    )
}

/// The column is dropped and added again rather than altered.
fn readd(before: &Column, after: &Column) -> bool {
    match (&before.generated, &after.generated) {
        (None, Some(_)) => true,
        (Some(x), Some(y)) => x != y,
        _ => false,
    }
}

fn same_opt_sql(a: Option<&String>, b: Option<&String>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => same_sql(a, b),
        (None, None) => true,
        _ => false,
    }
}

fn same_policy(a: &Policy, b: &Policy) -> bool {
    let mut ra = a.roles.clone();
    let mut rb = b.roles.clone();
    ra.sort();
    rb.sort();
    a.command == b.command
        && a.permissive == b.permissive
        && ra == rb
        && same_opt_sql(a.using.as_ref(), b.using.as_ref())
        && same_opt_sql(a.check.as_ref(), b.check.as_ref())
}

/// Who a policy applies to, in a sentence.
fn who(policy: &Policy) -> String {
    if policy.roles.is_empty() {
        "every role".to_owned()
    } else {
        policy.roles.join(", ")
    }
}

/// `ALTER POLICY` when it can express the change: not the command, not
/// permissive/restrictive, and no expression removed.
fn alter_policy(table: &str, b: &Policy, a: &Policy) -> Option<String> {
    if a.command != b.command
        || a.permissive != b.permissive
        || (b.using.is_some() && a.using.is_none())
        || (b.check.is_some() && a.check.is_none())
    {
        return None;
    }
    let mut sql = format!("ALTER POLICY {} ON {table}", quote_ident(&a.name));
    let mut ra = a.roles.clone();
    let mut rb = b.roles.clone();
    ra.sort();
    rb.sort();
    if ra != rb {
        let roles = policy_roles(a);
        sql.push_str(if roles.is_empty() {
            " TO PUBLIC"
        } else {
            &roles
        });
    }
    if !same_opt_sql(b.using.as_ref(), a.using.as_ref())
        && let Some(using) = &a.using
    {
        sql.push_str(" USING ");
        sql.push_str(&parenthesized(using));
    }
    if !same_opt_sql(b.check.as_ref(), a.check.as_ref())
        && let Some(check) = &a.check
    {
        sql.push_str(" WITH CHECK ");
        sql.push_str(&parenthesized(check));
    }
    sql.push(';');
    Some(sql)
}

/// What adding a constraint to a table with rows risks.
fn adding_note(constraint: &Constraint) -> String {
    match constraint.kind {
        ConstraintKind::PrimaryKey => {
            "Fails if existing rows hold duplicates or NULLs; builds an index, blocking writes meanwhile."
        }
        ConstraintKind::Unique => {
            "Fails if existing rows hold duplicates; builds an index, blocking writes meanwhile."
        }
        ConstraintKind::Check => {
            "Fails if an existing row breaks it. On a large table, add it NOT VALID, then VALIDATE CONSTRAINT."
        }
        ConstraintKind::ForeignKey => {
            "Fails if an existing row points nowhere. On a large table, add it NOT VALID, then VALIDATE CONSTRAINT."
        }
        ConstraintKind::Exclusion => "Fails if existing rows conflict; builds an index, blocking writes meanwhile.",
    }
    .to_owned()
}

fn describe(entity: &Entity) -> String {
    match entity.kind {
        EntityKind::Table => "a table".to_owned(),
        EntityKind::PartitionedTable => format!(
            "a table partitioned by {}",
            entity.partition_key.as_deref().unwrap_or("?")
        ),
        EntityKind::View => "a view".to_owned(),
        EntityKind::MaterializedView => "a materialized view".to_owned(),
        EntityKind::ForeignTable => "a foreign table".to_owned(),
    }
}

/// The statements of a rename: `ALTER … SET SCHEMA` for a move, then
/// `ALTER … RENAME` when the name changes too.
pub(crate) fn rename_statements(r: &Rename) -> Vec<String> {
    let Some(to_schema) = r.to_schema.as_deref().filter(|t| *t != r.schema) else {
        return vec![rename_statement(r)];
    };
    let keyword = match r.kind {
        RenameKind::View => "VIEW",
        RenameKind::MaterializedView => "MATERIALIZED VIEW",
        RenameKind::Enum | RenameKind::Composite => "TYPE",
        RenameKind::Domain => "DOMAIN",
        _ => "TABLE",
    };
    let mut out = vec![format!(
        "ALTER {keyword} {} SET SCHEMA {};",
        qualified(&r.schema, &r.from),
        quote_ident(to_schema)
    )];
    if r.from != r.to {
        out.push(rename_statement(&Rename {
            schema: to_schema.to_owned(),
            to_schema: None,
            ..r.clone()
        }));
    }
    out
}

/// The `ALTER … RENAME` statement of a rename in its schema.
fn rename_statement(r: &Rename) -> String {
    let q = |name: &str| qualified(&r.schema, name);
    let parent = r.parent.as_deref().unwrap_or("");
    let (from, to) = (quote_ident(&r.from), quote_ident(&r.to));
    match r.kind {
        RenameKind::Table | RenameKind::Partition => {
            format!("ALTER TABLE {} RENAME TO {to};", q(&r.from))
        }
        RenameKind::View => format!("ALTER VIEW {} RENAME TO {to};", q(&r.from)),
        RenameKind::MaterializedView => {
            format!("ALTER MATERIALIZED VIEW {} RENAME TO {to};", q(&r.from))
        }
        RenameKind::Index => format!("ALTER INDEX {} RENAME TO {to};", q(&r.from)),
        RenameKind::Sequence => format!("ALTER SEQUENCE {} RENAME TO {to};", q(&r.from)),
        RenameKind::Column => {
            format!("ALTER TABLE {} RENAME COLUMN {from} TO {to};", q(parent))
        }
        RenameKind::Constraint => {
            format!(
                "ALTER TABLE {} RENAME CONSTRAINT {from} TO {to};",
                q(parent)
            )
        }
        RenameKind::Policy => format!("ALTER POLICY {from} ON {} RENAME TO {to};", q(parent)),
        RenameKind::Enum | RenameKind::Composite => {
            format!("ALTER TYPE {} RENAME TO {to};", q(&r.from))
        }
        RenameKind::Domain => format!("ALTER DOMAIN {} RENAME TO {to};", q(&r.from)),
        RenameKind::EnumValue => format!(
            "ALTER TYPE {} RENAME VALUE {} TO {};",
            q(parent),
            quote_literal(&r.from),
            quote_literal(&r.to)
        ),
        RenameKind::Attribute => {
            format!("ALTER TYPE {} RENAME ATTRIBUTE {from} TO {to};", q(parent))
        }
    }
}

/// What a rename's statement says about it.
fn rename_note(c: &Candidate, all: &[Candidate]) -> String {
    let kept = match c.rename.kind {
        RenameKind::Column => "Keeps the column and its data.",
        RenameKind::Table | RenameKind::Partition => {
            "Keeps the table, its rows, and everything on it or referencing it."
        }
        RenameKind::EnumValue => "Rows holding the label read the new one.",
        RenameKind::Enum | RenameKind::Domain | RenameKind::Composite => {
            "Its columns keep their values."
        }
        RenameKind::View | RenameKind::MaterializedView => "What reads it keeps reading it.",
        _ => "",
    };
    let why = if let Some(parent) = &c.follows {
        let of = all
            .iter()
            .find(|x| x.key == *parent)
            .map_or_else(|| "its parent".to_owned(), |x| x.rename.describe());
        format!(
            "With the rename of {of}: Postgres keeps the old name, which the new schema does not."
        )
    } else if c.confidence == Confidence::Certain {
        if c.rename.final_schema() == c.rename.schema {
            "A known rename.".to_owned()
        } else {
            "A known move.".to_owned()
        }
    } else {
        let what = if c.rename.final_schema() == c.rename.schema {
            "rename"
        } else {
            "move"
        };
        format!(
            "Detected as a {what} ({}): turn it off to drop and add instead.",
            lowercase_first(c.reason.trim_end_matches('.'))
        )
    };
    if kept.is_empty() {
        why
    } else {
        format!("{why} {kept}")
    }
}

/// A foreign key Postgres made itself: the copy, on the referencing table,
/// of a key to a partitioned table, one per partition (`…_fkey1`,
/// `…_fkey2`), which references the partition. It comes and goes with the
/// key it copies; creating or dropping it explicitly fails.
pub(crate) fn fk_clone(schema: &Schema, table: &Entity, fk: &Constraint) -> bool {
    let (Some(target_schema), Some(target)) = (&fk.ref_schema, &fk.ref_table) else {
        return false;
    };
    let parents: BTreeMap<Name, Name> = schema
        .partitions
        .iter()
        .map(|p| {
            (
                name_of(&p.child_schema, &p.child),
                name_of(&p.parent_schema, &p.parent),
            )
        })
        .collect();
    let mut ancestors = BTreeSet::new();
    let mut at = name_of(target_schema, target);
    while let Some(up) = parents.get(&at) {
        if !ancestors.insert(up.clone()) {
            break;
        }
        at = up.clone();
    }
    !ancestors.is_empty()
        && table.foreign_keys().any(|other| {
            other.name != fk.name
                && other.columns == fk.columns
                && other.ref_columns == fk.ref_columns
                && other.on_update == fk.on_update
                && other.on_delete == fk.on_delete
                && other
                    .ref_schema
                    .as_deref()
                    .zip(other.ref_table.as_deref())
                    .is_some_and(|(s, t)| ancestors.contains(&name_of(s, t)))
        })
}

/// What a rename left off would keep, and what runs instead: the statements
/// it would replace (by prefix for the adds, whole for the drops).
struct LeftOff {
    adds: Vec<String>,
    drops: Vec<String>,
    keeps: &'static str,
    instead: &'static str,
}

impl LeftOff {
    fn of(r: &Rename) -> Self {
        let parent = r.parent.as_deref().unwrap_or("");
        let (from, to) = (
            qualified(&r.schema, &r.from),
            qualified(r.final_schema(), &r.to),
        );
        let (adds, drops, keeps, instead) = match r.kind {
            RenameKind::Column => {
                let table = qualified(&r.schema, parent);
                (
                    vec![format!(
                        "ALTER TABLE {table} ADD COLUMN {} ",
                        quote_ident(&r.to)
                    )],
                    vec![format!(
                        "ALTER TABLE {table} DROP COLUMN {};",
                        quote_ident(&r.from)
                    )],
                    "its data",
                    "the column is dropped and added, its data lost",
                )
            }
            RenameKind::Table => (
                vec![
                    format!("CREATE TABLE {to} "),
                    format!("CREATE UNLOGGED TABLE {to} "),
                ],
                vec![format!("DROP TABLE {from};")],
                "its rows",
                "the table is dropped and created, its rows lost",
            ),
            RenameKind::Partition => (
                vec![format!("CREATE TABLE {to} PARTITION OF ")],
                vec![format!("DROP TABLE {from};")],
                "its rows",
                "the partition is dropped and created, its rows lost",
            ),
            RenameKind::View | RenameKind::MaterializedView => {
                let keyword = if r.kind == RenameKind::View {
                    "VIEW"
                } else {
                    "MATERIALIZED VIEW"
                };
                (
                    vec![format!("CREATE {keyword} {to} ")],
                    vec![format!("DROP {keyword} {from};")],
                    "what reads it",
                    "it is dropped and created again",
                )
            }
            RenameKind::Enum | RenameKind::Composite => (
                vec![format!("CREATE TYPE {to} ")],
                vec![format!("DROP TYPE {from};")],
                "its columns as they are",
                "the type is created anew and its columns converted to it, which can fail",
            ),
            RenameKind::Domain => (
                vec![format!("CREATE DOMAIN {to} ")],
                vec![format!("DROP DOMAIN {from};")],
                "its columns as they are",
                "the domain is created anew and its columns converted to it, which can fail",
            ),
            _ => (
                Vec::new(),
                Vec::new(),
                "it as it is",
                "it is dropped and created again",
            ),
        };
        Self {
            adds,
            drops,
            keeps,
            instead,
        }
    }
}

fn lowercase_first(text: &str) -> String {
    let mut out = text.to_owned();
    if let Some(first) = out.get(..1) {
        let lower = first.to_lowercase();
        out.replace_range(..1, &lower);
    }
    out
}
