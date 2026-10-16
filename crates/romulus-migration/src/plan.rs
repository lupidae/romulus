//! Planning the statements.
//!
//! Each statement gets a [`Phase`]; the migration is the statements sorted
//! by stage, then phase, then the order they were planned in. The phases are
//! the dependency order:
//!
//! 1. schemas and extensions the rest needs;
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

use std::collections::{BTreeMap, BTreeSet};

use remus_core::{
    Schema,
    emit::ident::{qualified, quote_ident, quote_literal},
    model::{
        Column, CompositeType, Constraint, ConstraintKind, DomainType, Entity, EntityKind,
        EnumType, Index, Partition, Policy, TypeKind,
    },
};

use crate::{
    Kind, Migration, Options, Section, Stage, Statement, Warning, WarningCode,
    cast::conversion,
    sql::{
        column_definition, create_policy, create_view, identity_when, mentions, owned_sequence,
        parenthesized, policy_roles, relation_keyword, same_sql, storage, table_name,
    },
};

/// A schema and a name: the identity of every schema-qualified object.
type Name = (String, String);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Phase {
    Schemas,
    Extensions,
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
            P::DropPolicies
            | P::DropViews
            | P::DropForeignKeys
            | P::DropIndexes
            | P::DropConstraints
            | P::EarlyDrops => Section::Dependents,
            P::Types | P::Domains | P::Composites => Section::Types,
            P::Sequences
            | P::Tables
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
}

impl Statement {
    fn destroys(&mut self, note: impl Into<String>) -> &mut Self {
        self.destructive = true;
        self.note = Some(note.into());
        self
    }

    fn noted(&mut self, note: impl Into<String>) -> &mut Self {
        self.note = Some(note.into());
        self
    }
}

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
    /// The names Postgres gave the old domains' unnamed checks, guessed:
    /// (schema, domain, definition) → name.
    check_names: BTreeMap<(String, String, String), String>,
    /// Tables on either side of a foreign key added (quoted names).
    keys_checked: Vec<String>,
    out: Vec<Planned>,
    warnings: Vec<Warning>,
}

impl<'a> Planner<'a> {
    pub(crate) fn new(
        old: &'a Schema,
        new: &'a Schema,
        options: &'a Options,
        check_names: BTreeMap<(String, String, String), String>,
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
            check_names,
            keys_checked: Vec::new(),
            out: Vec::new(),
            warnings: Vec::new(),
        }
    }

    pub(crate) fn plan(mut self) -> Migration {
        self.classify();
        self.schemas();
        self.extensions();
        self.enums();
        self.domains();
        self.composites();
        self.drop_views();
        self.entities();
        self.drop_removed_tables();
        self.possible_table_renames();
        self.partitions();
        self.create_views();
        self.unforce_for_foreign_keys();
        self.empty_schemas();
        self.finish()
    }

    fn push(&mut self, phase: Phase, kind: Kind, sql: impl Into<String>) -> &mut Statement {
        self.out.push(Planned {
            phase,
            statement: Statement {
                sql: sql.into(),
                kind,
                destructive: false,
                note: None,
                alternative: None,
                stage: Stage::Transaction,
                section: phase.section(),
            },
        });
        &mut self.out.last_mut().expect("just pushed").statement
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
            if let (Some(b), Some(a)) = (b, a)
                && (!same_sql(&b.base_type, &a.base_type)
                    || recreate.iter().any(|(_, e)| mentions(&b.base_type, e)))
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
                if readd(column, after) {
                    let why = if column.generated.is_none() {
                        "It becomes generated, which Postgres cannot do in place: dropped and added again, its values replaced by the generated ones."
                    } else {
                        "Its generated storage changes, which Postgres cannot do in place: dropped and added again."
                    };
                    self.readded
                        .entry(name.clone())
                        .or_default()
                        .insert(column.name.clone(), why.to_owned());
                } else if column.r#type != after.r#type || self.type_recreated(after) {
                    self.converted
                        .entry(name.clone())
                        .or_default()
                        .insert(column.name.clone());
                }
            }
        }

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
        let old_parts: BTreeMap<Name, &Partition> = self
            .old
            .partitions
            .iter()
            .map(|p| (name_of(&p.child_schema, &p.child), p))
            .collect();
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
        if self.converted.contains_key(table)
            && let Some((schema, ty)) = self.aside.keys().find(|(_, t)| mentions(definition, t))
        {
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
        let before = Self::schemas_of(self.old);
        let after = Self::schemas_of(self.new);
        for schema in after.difference(&before) {
            if *schema != "public" {
                let sql = format!("CREATE SCHEMA IF NOT EXISTS {};", quote_ident(schema));
                self.push(Phase::Schemas, Kind::CreateSchema, sql);
            }
        }
    }

    fn empty_schemas(&mut self) {
        let before = Self::schemas_of(self.old);
        let after = Self::schemas_of(self.new);
        let left: Vec<String> = before
            .difference(&after)
            .filter(|s| **s != "public")
            .map(|s| (*s).to_owned())
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
        let added: Vec<&str> = a
            .labels
            .iter()
            .filter(|l| !b.labels.contains(l))
            .map(String::as_str)
            .collect();
        let quoted = |labels: &[&str]| {
            labels
                .iter()
                .map(|l| quote_literal(l))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let rename = (removed.len() == 1 && added.len() == 1).then(|| {
            format!(
                "ALTER TYPE {ty} RENAME VALUE {} TO {};",
                quote_literal(removed[0]),
                quote_literal(added[0])
            )
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
                "{why}: the type is created again and every column using it converted. If {} was renamed to {}, the RENAME VALUE below does it in place instead.",
                quoted(&removed),
                quoted(&added)
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
                    "Enum {} loses {}: it is created again, and converting its columns fails on any row that still holds {}.",
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
                        self.push(
                            Phase::Domains,
                            Kind::AlterDomain,
                            format!("ALTER DOMAIN {ty} RENAME TO {};", quote_ident(&aside)),
                        )
                        .noted(if same_sql(&b.base_type, &a.base_type) {
                            format!(
                                "Its type {} is created again, so it is too, and every column using it converted.",
                                a.base_type
                            )
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
        // The model lists checks by definition, without their names.
        for check in &b.constraints {
            if a.constraints.iter().any(|c| same_sql(c, check)) {
                continue;
            }
            let guessed = self
                .check_names
                .get(&(name.0.clone(), name.1.clone(), check.clone()))
                .cloned()
                .unwrap_or_else(|| format!("{}_check", name.1));
            self.push(
                Phase::Domains,
                Kind::AlterDomain,
                format!("ALTER DOMAIN {ty} DROP CONSTRAINT {};", quote_ident(&guessed)),
            )
            .noted(format!(
                "Drops {check}, by the name Postgres gives an unnamed check: if yours has another name, use it."
            ));
            self.warn(
                WarningCode::ConstraintName,
                Some(dotted(name)),
                format!(
                    "Domain {} drops a check by a guessed name ({guessed}): the model does not record domain check names. Check yours with \\dD+ {}.",
                    dotted(name),
                    dotted(name)
                ),
            );
        }
        if b.not_null != a.not_null {
            if a.not_null {
                self.push(
                    Phase::Domains,
                    Kind::AlterDomain,
                    format!("ALTER DOMAIN {ty} SET NOT NULL;"),
                )
                .destroys("Fails if a column of this domain holds a NULL.");
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
                .noted("Fails if a value of this domain already breaks it.");
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
                Some(before) if before.r#type != attribute.r#type => {
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
                    .destroys(format!(
                        "Converts {} to {}: values that do not convert are lost.",
                        before.r#type, attribute.r#type
                    ));
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
            let changed = match self.new_entities.get(*name) {
                None => true,
                Some(after) => {
                    after.kind != view.kind
                        || !same_sql(
                            view.view_definition.as_deref().unwrap_or(""),
                            after.view_definition.as_deref().unwrap_or(""),
                        )
                }
            };
            let uses_aside = self.mentions_aside(view.view_definition.as_deref().unwrap_or(""));
            if changed || uses_aside {
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
        for column in &after.columns {
            let was = before.and_then(|b| b.column(&column.name));
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
            self.sequence_for(column);
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
        for column in &table.columns {
            self.own_sequence(Phase::Tables, &name, column);
        }
        for fk in table.foreign_keys() {
            self.add_constraint(&name, fk, None);
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

    /// `CREATE SEQUENCE` for a serial column's sequence, once.
    fn sequence_for(&mut self, column: &Column) {
        let Some(sequence) = column.default.as_deref().and_then(owned_sequence) else {
            return;
        };
        if column.generated.is_some() || self.sequence_exists(sequence) {
            return;
        }
        if self.sequences.insert(sequence.to_owned()) {
            self.push(
                Phase::Sequences,
                Kind::CreateSequence,
                format!("CREATE SEQUENCE IF NOT EXISTS {sequence};"),
            );
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

    fn security(&mut self, after: &Entity, before: Option<&Entity>) {
        let table = table_name(after);
        let (rls, forced) = before.map_or((false, false), |b| (b.rls, b.rls_forced));
        if after.rls != rls {
            let verb = if after.rls { "ENABLE" } else { "DISABLE" };
            self.push(
                Phase::Security,
                Kind::RowLevelSecurity,
                format!("ALTER TABLE {table} {verb} ROW LEVEL SECURITY;"),
            );
        }
        if after.rls_forced != forced {
            let verb = if after.rls_forced {
                "FORCE"
            } else {
                "NO FORCE"
            };
            self.push(
                Phase::Security,
                Kind::RowLevelSecurity,
                format!("ALTER TABLE {table} {verb} ROW LEVEL SECURITY;"),
            );
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
                (Some(_), None) => drop(self),
                (None, Some(a)) => {
                    self.push(
                        Phase::Security,
                        Kind::CreatePolicy,
                        create_policy(&table, a),
                    );
                }
                (Some(_), Some(a)) if rebuild => {
                    drop(self);
                    self.push(
                        Phase::Security,
                        Kind::CreatePolicy,
                        create_policy(&table, a),
                    )
                    .noted("Dropped above and created again around a column's type change.");
                }
                (Some(b), Some(a)) if !same_policy(b, a) => {
                    if let Some(sql) = alter_policy(&table, b, a) {
                        self.push(Phase::Security, Kind::AlterPolicy, sql);
                    } else {
                        drop(self);
                        self.push(
                            Phase::Security,
                            Kind::CreatePolicy,
                            create_policy(&table, a),
                        );
                    }
                }
                _ => {}
            }
        }
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
        // Constraints.
        for (_, cb, ca) in pair(&b.constraints, &a.constraints, |c| c.name.clone()) {
            match (cb, ca) {
                (Some(cb), None) => self.drop_constraint(&table, cb),
                (None, Some(ca)) => {
                    let note = adding_note(ca);
                    self.add_constraint(&table, ca, Some(note));
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
                        let note = if cb.kind == ca.kind && same_sql(&cb.definition, &ca.definition)
                        {
                            self.rebuild_reason(name, &cb.definition)
                        } else {
                            Some(adding_note(ca))
                        };
                        self.drop_constraint(&table, cb);
                        self.add_constraint(&table, ca, note);
                        if let Some(comment) = &ca.comment {
                            self.constraint_comment(&table, ca, Some(comment));
                        }
                    } else if let Some(reason) = rebuilt {
                        self.drop_constraint(&table, cb);
                        self.add_constraint(
                            &table,
                            ca,
                            Some(format!("Dropped above and added back: {reason}.")),
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
    }

    fn drop_constraint(&mut self, table: &str, constraint: &Constraint) {
        let phase = if constraint.kind == ConstraintKind::ForeignKey {
            Phase::DropForeignKeys
        } else {
            Phase::DropConstraints
        };
        self.push(
            phase,
            Kind::DropConstraint,
            format!(
                "ALTER TABLE {table} DROP CONSTRAINT {};",
                quote_ident(&constraint.name)
            ),
        );
    }

    fn add_constraint(&mut self, table: &str, constraint: &Constraint, note: Option<String>) {
        let phase = if constraint.kind == ConstraintKind::ForeignKey {
            self.keys_checked.push(table.to_owned());
            if let (Some(schema), Some(target)) = (&constraint.ref_schema, &constraint.ref_table) {
                self.keys_checked.push(qualified(schema, target));
            }
            Phase::ForeignKeys
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
    }

    fn constraint_comment(&mut self, table: &str, constraint: &Constraint, comment: Option<&str>) {
        let target = format!("{} ON {table}", quote_ident(&constraint.name));
        self.comment("CONSTRAINT", &target, comment);
    }

    fn indexes(&mut self, name: &Name, b: &Entity, a: &Entity) {
        // Not around a column's conversion: the old index would be rebuilt
        // with it, inside the transaction, before a concurrent drop.
        let concurrent = self.options.concurrently
            && matches!(a.kind, EntityKind::Table | EntityKind::MaterializedView)
            && !self.converted.contains_key(name)
            && !self.readded.contains_key(name);
        for (index_name, ib, ia) in pair(&b.indexes, &a.indexes, |i| i.name.clone()) {
            let qualified_index = qualified(&name.0, &index_name);
            let concurrently = concurrent
                && ia.is_some_and(|i| !i.unique)
                && ib.is_none_or(|i| !self.mentions_aside(&i.definition));
            match (ib, ia) {
                (Some(_), None) => {
                    self.push(
                        Phase::DropIndexes,
                        Kind::DropIndex,
                        format!("DROP INDEX {qualified_index};"),
                    );
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
                        self.push(
                            Phase::DropIndexes,
                            Kind::DropIndex,
                            format!("DROP INDEX {qualified_index};"),
                        );
                    }
                    let rebuilt = same_sql(&ib.definition, &ia.definition)
                        .then(|| self.rebuild_reason(name, &ib.definition))
                        .flatten();
                    self.create_index(ia, concurrently, true);
                    if let (Some(reason), false) = (rebuilt, concurrently) {
                        self.out.last_mut().expect("just pushed").statement.note = Some(reason);
                    }
                }
                _ => {}
            }
        }
    }

    fn create_index(&mut self, index: &Index, concurrently: bool, existing_table: bool) {
        if concurrently {
            let sql = index
                .definition
                .replacen(" INDEX ", " INDEX CONCURRENTLY ", 1);
            let statement = self.push(Phase::Indexes, Kind::CreateIndex, format!("{sql};"));
            statement.stage = Stage::Concurrent;
            statement.noted("Outside the transaction: CONCURRENTLY does not block writes, and cannot run in one. If it fails, drop the invalid index it leaves and run it again.");
        } else {
            let statement = self.push(
                Phase::Indexes,
                Kind::CreateIndex,
                format!("{};", index.definition),
            );
            if existing_table && index.unique {
                statement.noted("Fails if existing rows hold duplicates.");
            }
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
        let added: Vec<&Column> = a
            .columns
            .iter()
            .filter(|c| b.column(&c.name).is_none())
            .collect();
        // A removed and an added column alike: maybe a rename.
        let mut renames: BTreeMap<&str, &str> = BTreeMap::new();
        for r in &removed {
            if let Some(x) = added
                .iter()
                .find(|x| !renames.values().any(|v| *v == x.name) && same_shape(r, x))
            {
                renames.insert(&r.name, &x.name);
            }
        }

        for column in &a.columns {
            let Some(before) = b.column(&column.name) else {
                // Added.
                self.sequence_for(column);
                let rename_from = renames
                    .iter()
                    .find(|(_, to)| **to == column.name)
                    .map(|(from, _)| *from);
                let statement = self.push(
                    Phase::Columns,
                    Kind::AddColumn,
                    format!(
                        "ALTER TABLE {table} ADD COLUMN {};",
                        column_definition(column)
                    ),
                );
                if let Some(from) = rename_from {
                    statement.alternative = Some(format!(
                        "ALTER TABLE {table} RENAME COLUMN {} TO {};",
                        quote_ident(from),
                        quote_ident(&column.name)
                    ));
                    statement.note = Some(format!(
                        "Possibly a rename of {from}: the RENAME keeps its data, where this ADD and the DROP below lose it."
                    ));
                } else if column.not_null
                    && column.default.is_none()
                    && column.identity.is_none()
                    && column.generated.is_none()
                {
                    statement.destroys("Fails if the table has rows: give it a DEFAULT, or add it nullable, fill it, then SET NOT NULL.");
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

            // 1. Identity and generation removed.
            if before.identity.is_some() && column.identity.is_none() {
                self.push(
                    Phase::Columns,
                    Kind::AlterColumn,
                    format!("{alter} DROP IDENTITY;"),
                );
            }
            if before.generated.is_some() && column.generated.is_none() {
                self.push(
                    Phase::Columns,
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
                self.sequence_for(column);
                let loses_labels = column.type_kind == TypeKind::Enum
                    && self.enum_loses_labels(&column.type_schema, &column.type_name);
                let statement = self.push(Phase::Columns, Kind::AlterColumn, sql);
                if recreated {
                    let what = if column.type_kind == TypeKind::Domain {
                        "domain"
                    } else {
                        "enum"
                    };
                    if loses_labels {
                        statement.destroys(format!(
                            "Converts to the recreated {what} {}: fails on a row holding a label it lost.",
                            column.r#type.trim_end_matches("[]")
                        ));
                    } else {
                        statement.noted(format!(
                            "Converts to the recreated {what} {}.",
                            column.r#type.trim_end_matches("[]")
                        ));
                    }
                } else if conversion.lossless {
                    statement.note.clone_from(&conversion.note);
                } else {
                    statement.destroys(conversion.note.clone().unwrap_or_default());
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
                self.own_sequence(Phase::Columns, &table, column);
            }
            // 3. Default.
            if !default_done
                && !generated
                && !same_opt_sql(before.default.as_ref(), column.default.as_ref())
            {
                match &column.default {
                    Some(default) => {
                        self.sequence_for(column);
                        self.push(
                            Phase::Columns,
                            Kind::AlterColumn,
                            format!("{alter} SET DEFAULT {default};"),
                        );
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
                    .destroys("Fails if a row holds NULL here: fill those first.");
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
                    self.push(
                        Phase::Columns,
                        Kind::AlterColumn,
                        format!(
                            "{alter} ADD GENERATED {} AS IDENTITY;",
                            identity_when(identity)
                        ),
                    )
                    .noted(format!(
                        "Its sequence starts at 1: if the table has rows, move it past them (SELECT setval(pg_get_serial_sequence('{}', '{}'), max({col})) FROM {table}).",
                        table.replace('\'', "''"),
                        column.name.replace('\'', "''")
                    ));
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
                && !same_opt_sql(before.default.as_ref(), column.default.as_ref())
            {
                self.push(
                    Phase::Columns,
                    Kind::AlterColumn,
                    format!("{alter} SET EXPRESSION AS ({expression});"),
                )
                .noted(format!(
                    "PostgreSQL 17 or later; it rewrites the table ({} column). On older servers, drop and add the column.",
                    storage(kind).to_lowercase()
                ));
            }
        }

        // Dropped: last, with the other removals.
        for column in &removed {
            let col = quote_ident(&column.name);
            let statement = self.push(
                Phase::DropColumns,
                Kind::DropColumn,
                format!("ALTER TABLE {table} DROP COLUMN {col};"),
            );
            if let Some(to) = renames.get(column.name.as_str()) {
                statement.destroys(format!(
                    "Deletes the column and its data. Possibly renamed to {to}: see the ADD COLUMN above."
                ));
                statement.alternative = Some(format!(
                    "ALTER TABLE {table} RENAME COLUMN {col} TO {};",
                    quote_ident(to)
                ));
            } else {
                statement.destroys("Deletes the column and its data.");
            }
        }
        for (from, to) in &renames {
            self.warn(
                WarningCode::PossibleRename,
                Some(format!("{}.{}.{to}", name.0, name.1)),
                format!(
                    "{}.{from} is dropped and {}.{to} added with the same type: if it is a rename, use RENAME COLUMN (in the comment) to keep the data.",
                    name.1, name.1
                ),
            );
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
        if order != wanted {
            let moved: Vec<&str> = wanted
                .iter()
                .zip(&order)
                .filter(|(w, o)| w != o)
                .map(|(w, _)| *w)
                .collect();
            self.warn(
                WarningCode::ColumnOrder,
                Some(dotted(name)),
                format!(
                    "{} ends up with its columns as ({}), not in the new model's order: Postgres adds columns last and cannot move them. Only SELECT * and INSERT without a column list notice ({} placed differently).",
                    dotted(name),
                    order.join(", "),
                    moved.join(", ")
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

    fn possible_table_renames(&mut self) {
        let removed: Vec<&Entity> = self
            .old_entities
            .iter()
            .filter(|(n, e)| e.kind == EntityKind::Table && !self.new_entities.contains_key(*n))
            .map(|(_, e)| *e)
            .collect();
        let added: Vec<&Entity> = self
            .new_entities
            .iter()
            .filter(|(n, e)| e.kind == EntityKind::Table && !self.old_entities.contains_key(*n))
            .map(|(_, e)| *e)
            .collect();
        let shape = |e: &Entity| {
            e.columns
                .iter()
                .map(|c| (c.name.clone(), c.r#type.clone()))
                .collect::<Vec<_>>()
        };
        let mut taken = BTreeSet::new();
        for r in removed {
            let Some(x) = added.iter().find(|x| {
                !taken.contains(&table_name(x))
                    && x.schema == r.schema
                    && !r.columns.is_empty()
                    && shape(x) == shape(r)
            }) else {
                continue;
            };
            taken.insert(table_name(x));
            let rename = format!(
                "ALTER TABLE {} RENAME TO {};",
                table_name(r),
                quote_ident(&x.name)
            );
            let create = format!("CREATE TABLE {} ", table_name(x));
            let create_paren = format!("CREATE TABLE {} (", table_name(x));
            let drop = format!("DROP TABLE {};", table_name(r));
            for planned in &mut self.out {
                let s = &mut planned.statement;
                if s.kind == Kind::CreateTable
                    && (s.sql.starts_with(&create) || s.sql.starts_with(&create_paren))
                {
                    s.alternative = Some(rename.clone());
                    s.note = Some(format!(
                        "Possibly a rename of {}: the RENAME keeps its rows, where this CREATE and the DROP below lose them.",
                        r.name
                    ));
                } else if s.kind == Kind::DropTable && s.sql == drop {
                    s.alternative = Some(rename.clone());
                    s.note = Some(format!(
                        "Deletes the table and its rows. Possibly renamed to {}: see the CREATE TABLE above.",
                        x.name
                    ));
                }
            }
            self.warn(
                WarningCode::PossibleRename,
                Some(dotted(&name_of(&x.schema, &x.name))),
                format!(
                    "Table {} is dropped and {} created with the same columns: if it is a rename, use ALTER TABLE … RENAME TO (in the comment) to keep the rows.",
                    dotted(&name_of(&r.schema, &r.name)),
                    dotted(&name_of(&x.schema, &x.name))
                ),
            );
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
                if self.fk_rebuilt(name, fk).is_some() {
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
                if target.is_some_and(|t| &t != name && cycle_set.contains(&t)) {
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
                    statement.noted("Fails if the DEFAULT partition holds rows that belong here: move them out first.");
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
                .noted("Scans the partition to check every row fits the bound.");
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
            // Its own NO FORCE (the new schema stops forcing) is done already.
            self.out
                .retain(|p| !(p.phase == Phase::Security && p.statement.sql == no_force));
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
        self.out.sort_by_key(|p| (p.statement.stage, p.phase));
        Migration {
            statements: self.out.into_iter().map(|p| p.statement).collect(),
            warnings: self.warnings,
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

/// Two columns a rename would explain: everything alike but the name.
fn same_shape(a: &Column, b: &Column) -> bool {
    a.r#type == b.r#type
        && a.not_null == b.not_null
        && a.identity == b.identity
        && a.generated == b.generated
        && same_opt_sql(a.default.as_ref(), b.default.as_ref())
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
