//! Comparing two models, object by object and field by field.
//!
//! Every field of the model is compared, so an empty diff means the schemas
//! are the same up to [`crate::normalize`]. Lists whose order the catalog does
//! not define (policy roles, domain checks, view dependencies) compare as
//! sorted lists.

use std::collections::{BTreeMap, BTreeSet};

use remus_core::{Schema, model as m};

use crate::{
    model::{
        AttributeChange, AttributeDiff, ColumnChange, ColumnDiff, CompositeChange, CompositeDiff,
        ConstraintChange, ConstraintDiff, ConstraintKind, DomainChange, DomainDiff, EntityChange,
        EntityDiff, EntityKind, EntityStatus, EnumChange, EnumDiff, ExtensionChange, ExtensionDiff,
        Generated, Identity, IndexChange, IndexDiff, LabelDiff, PartitionChange, PartitionDiff,
        PolicyChange, PolicyCommand, PolicyDiff, Positions, SchemaDiff, Sides, Status, TypeKind,
    },
    order::{by_key, placed},
    sql,
};

/// Pushes `$variant { before, after }` onto `$out` when the two values differ,
/// converting each with `$convert` (cloning by default).
macro_rules! field {
    ($out:ident, $enum:ident :: $variant:ident, $before:expr, $after:expr) => {
        field!($out, $enum::$variant, $before, $after, Clone::clone)
    };
    ($out:ident, $enum:ident :: $variant:ident, $before:expr, $after:expr, $convert:expr) => {
        if $before != $after {
            $out.push($enum::$variant {
                before: ($convert)(&$before),
                after: ($convert)(&$after),
            });
        }
    };
}

pub(crate) fn schema_diff(old: &Schema, new: &Schema) -> SchemaDiff {
    let mut out = SchemaDiff::default();

    let old_parts = by_parent(&old.partitions);
    let new_parts = by_parent(&new.partitions);
    let mut parents = BTreeSet::new();
    for (_, before, after) in by_key(&old.entities, &new.entities, |e| (&e.schema, &e.name)) {
        let Some(any) = after.or(before) else {
            continue;
        };
        let key = any.qualified_name();
        let (status, diff) = entity(
            old,
            new,
            before,
            after,
            old_parts.get(&key).map_or(&[], Vec::as_slice),
            new_parts.get(&key).map_or(&[], Vec::as_slice),
        );
        out.status.entities.insert(key.clone(), status);
        out.entities.extend(diff);
        parents.insert(key);
    }
    out.orphan_partitions = partitions(&orphans(old, &parents), &orphans(new, &parents));

    for (_, before, after) in by_key(&old.enums, &new.enums, |e| (&e.schema, &e.name)) {
        let Some(any) = after.or(before) else {
            continue;
        };
        let key = qualified(&any.schema, &any.name);
        let diff = enum_type(old, new, before, after);
        let status = diff.as_ref().map_or(Status::Unchanged, |d| d.status);
        out.status.enums.insert(key, status);
        out.enums.extend(diff);
    }
    for (_, before, after) in by_key(&old.domains, &new.domains, |d| (&d.schema, &d.name)) {
        let Some(any) = after.or(before) else {
            continue;
        };
        let key = qualified(&any.schema, &any.name);
        let diff = domain(old, new, before, after);
        let status = diff.as_ref().map_or(Status::Unchanged, |d| d.status);
        out.status.domains.insert(key, status);
        out.domains.extend(diff);
    }
    for (_, before, after) in by_key(&old.composites, &new.composites, |c| (&c.schema, &c.name)) {
        let Some(any) = after.or(before) else {
            continue;
        };
        let key = qualified(&any.schema, &any.name);
        let diff = composite(old, new, before, after);
        let status = diff.as_ref().map_or(Status::Unchanged, |d| d.status);
        out.status.composites.insert(key, status);
        out.composites.extend(diff);
    }
    for (name, before, after) in by_key(&old.extensions, &new.extensions, |x| &x.name) {
        let diff = extension(old, new, before, after);
        let status = diff.as_ref().map_or(Status::Unchanged, |d| d.status);
        out.status.extensions.insert(name.clone(), status);
        out.extensions.extend(diff);
    }

    out.finish();
    out
}

/// Added or removed by presence; otherwise changed when anything differs.
fn presence<T>(before: Option<T>, after: Option<T>, changed: bool) -> Status {
    match (before.is_some(), after.is_some()) {
        (false, true) => Status::Added,
        (true, false) => Status::Removed,
        _ if changed => Status::Changed,
        _ => Status::Unchanged,
    }
}

fn qualified(schema: &str, name: &str) -> String {
    format!("{schema}.{name}")
}

fn parent_name(p: &m::Partition) -> String {
    qualified(&p.parent_schema, &p.parent)
}

fn orphans<'a>(schema: &'a Schema, parents: &BTreeSet<String>) -> Vec<&'a m::Partition> {
    schema
        .partitions
        .iter()
        .filter(|p| !parents.contains(&parent_name(p)))
        .collect()
}

fn by_parent(partitions: &[m::Partition]) -> BTreeMap<String, Vec<&m::Partition>> {
    let mut map: BTreeMap<String, Vec<&m::Partition>> = BTreeMap::new();
    for p in partitions {
        map.entry(parent_name(p)).or_default().push(p);
    }
    map
}

fn sorted<T: Ord + Clone>(items: &[T]) -> Vec<T> {
    let mut items = items.to_vec();
    items.sort();
    items
}

fn entity(
    old: &Schema,
    new: &Schema,
    before: Option<&m::Entity>,
    after: Option<&m::Entity>,
    old_parts: &[&m::Partition],
    new_parts: &[&m::Partition],
) -> (EntityStatus, Option<EntityDiff>) {
    let (columns, column_status) = columns(
        before.map_or(&[], |e| &e.columns),
        after.map_or(&[], |e| &e.columns),
    );
    let (constraints, constraint_status) = constraints(
        before.map_or(&[], |e| &e.constraints),
        after.map_or(&[], |e| &e.constraints),
    );
    let indexes = indexes(
        before.map_or(&[], |e| &e.indexes),
        after.map_or(&[], |e| &e.indexes),
    );
    let policies = policies(
        before.map_or(&[], |e| &e.policies),
        after.map_or(&[], |e| &e.policies),
    );
    let partitions = partitions(old_parts, new_parts);
    let changes = match (before, after) {
        (Some(b), Some(a)) => entity_changes(b, a),
        _ => Vec::new(),
    };
    let changed = !changes.is_empty()
        || !columns.is_empty()
        || !constraints.is_empty()
        || !indexes.is_empty()
        || !policies.is_empty()
        || !partitions.is_empty();
    let status = presence(before, after, changed);
    let summary = EntityStatus {
        status,
        columns: column_status,
        constraints: constraint_status,
    };
    let Some(any) = after.or(before).filter(|_| status != Status::Unchanged) else {
        return (summary, None);
    };
    let diff = EntityDiff {
        schema: any.schema.clone(),
        name: any.name.clone(),
        qualified_name: any.qualified_name(),
        kind: EntityKind::from(&any.kind),
        status,
        changes,
        columns,
        constraints,
        indexes,
        policies,
        partitions,
        lines: Vec::new(),
        sql: Sides {
            before: before.map(|e| sql::entity_sql(old, e, old_parts)),
            after: after.map(|e| sql::entity_sql(new, e, new_parts)),
        },
    };
    (summary, Some(diff))
}

fn entity_changes(b: &m::Entity, a: &m::Entity) -> Vec<EntityChange> {
    let mut out = Vec::new();
    field!(out, EntityChange::Kind, b.kind, a.kind, EntityKind::from);
    field!(out, EntityChange::Comment, b.comment, a.comment);
    field!(out, EntityChange::Rls, b.rls, a.rls);
    field!(out, EntityChange::RlsForced, b.rls_forced, a.rls_forced);
    field!(out, EntityChange::Unlogged, b.unlogged, a.unlogged);
    field!(
        out,
        EntityChange::PartitionKey,
        b.partition_key,
        a.partition_key
    );
    field!(
        out,
        EntityChange::ViewDefinition,
        b.view_definition,
        a.view_definition
    );
    let depends = |e: &m::Entity| -> Vec<String> {
        sorted(
            &e.depends_on
                .iter()
                .map(|r| qualified(&r.schema, &r.name))
                .collect::<Vec<_>>(),
        )
    };
    field!(out, EntityChange::DependsOn, depends(b), depends(a));
    out
}

fn columns(old: &[m::Column], new: &[m::Column]) -> (Vec<ColumnDiff>, BTreeMap<String, Status>) {
    let mut diffs = Vec::new();
    let mut statuses = BTreeMap::new();
    for p in placed(old, new, |c| c.name.as_str()) {
        let changes = match (p.before, p.after) {
            (Some((i, b)), Some((j, a))) => column_changes(b, a, p.moved.then_some((i, j))),
            _ => Vec::new(),
        };
        let status = presence(p.before, p.after, !changes.is_empty());
        statuses.insert(p.name.to_owned(), status);
        if status != Status::Unchanged {
            diffs.push(ColumnDiff {
                name: p.name.to_owned(),
                status,
                position: Positions {
                    before: p.before.map(|(i, _)| i),
                    after: p.after.map(|(i, _)| i),
                },
                changes,
                definition: Sides {
                    before: p.before.map(|(_, c)| sql::column_definition(c)),
                    after: p.after.map(|(_, c)| sql::column_definition(c)),
                },
            });
        }
    }
    (diffs, statuses)
}

fn column_changes(
    b: &m::Column,
    a: &m::Column,
    moved: Option<(usize, usize)>,
) -> Vec<ColumnChange> {
    let mut out = Vec::new();
    let type_of = |c: &m::Column| {
        (
            c.r#type.clone(),
            c.type_schema.clone(),
            c.type_name.clone(),
            c.is_array,
        )
    };
    if type_of(b) != type_of(a) {
        out.push(ColumnChange::Type {
            before: b.r#type.clone(),
            after: a.r#type.clone(),
        });
    }
    field!(
        out,
        ColumnChange::TypeKind,
        b.type_kind,
        a.type_kind,
        TypeKind::from
    );
    field!(out, ColumnChange::NotNull, b.not_null, a.not_null);
    if b.generated.is_some() && a.generated.is_some() {
        field!(out, ColumnChange::Expression, b.default, a.default);
    } else {
        field!(out, ColumnChange::Default, b.default, a.default);
    }
    field!(
        out,
        ColumnChange::Identity,
        b.identity,
        a.identity,
        |v: &Option<m::Identity>| { v.as_ref().map(Identity::from) }
    );
    field!(
        out,
        ColumnChange::Generated,
        b.generated,
        a.generated,
        |v: &Option<m::Generated>| { v.as_ref().map(Generated::from) }
    );
    field!(out, ColumnChange::Comment, b.comment, a.comment);
    if let Some((before, after)) = moved {
        out.push(ColumnChange::Position { before, after });
    }
    out
}

fn constraints(
    old: &[m::Constraint],
    new: &[m::Constraint],
) -> (Vec<ConstraintDiff>, BTreeMap<String, Status>) {
    let mut diffs = Vec::new();
    let mut statuses = BTreeMap::new();
    for (name, before, after) in by_key(old, new, |c| c.name.as_str()) {
        let changes = match (before, after) {
            (Some(b), Some(a)) => constraint_changes(b, a),
            _ => Vec::new(),
        };
        let status = presence(before, after, !changes.is_empty());
        statuses.insert(name.to_owned(), status);
        let Some(any) = after.or(before).filter(|_| status != Status::Unchanged) else {
            continue;
        };
        diffs.push(ConstraintDiff {
            name: name.to_owned(),
            kind: ConstraintKind::from(&any.kind),
            status,
            changes,
            definition: Sides {
                before: before.map(|c| c.definition.clone()),
                after: after.map(|c| c.definition.clone()),
            },
        });
    }
    (diffs, statuses)
}

fn constraint_changes(b: &m::Constraint, a: &m::Constraint) -> Vec<ConstraintChange> {
    let mut out = Vec::new();
    field!(
        out,
        ConstraintChange::Kind,
        b.kind,
        a.kind,
        ConstraintKind::from
    );
    // The structural fields are parsed from the same catalog row as the
    // definition; one line says it, but a difference in any of them counts.
    let shape = |c: &m::Constraint| {
        (
            c.definition.clone(),
            c.columns.clone(),
            c.ref_schema.clone(),
            c.ref_table.clone(),
            c.ref_columns.clone(),
            c.on_update.clone(),
            c.on_delete.clone(),
            c.deferrable,
        )
    };
    if shape(b) != shape(a) {
        out.push(ConstraintChange::Definition {
            before: b.definition.clone(),
            after: a.definition.clone(),
        });
    }
    field!(out, ConstraintChange::Comment, b.comment, a.comment);
    out
}

fn indexes(old: &[m::Index], new: &[m::Index]) -> Vec<IndexDiff> {
    let mut diffs = Vec::new();
    for (name, before, after) in by_key(old, new, |i| i.name.as_str()) {
        let mut changes = Vec::new();
        if let (Some(b), Some(a)) = (before, after)
            && (&b.definition, b.unique, b.partial) != (&a.definition, a.unique, a.partial)
        {
            changes.push(IndexChange::Definition {
                before: b.definition.clone(),
                after: a.definition.clone(),
            });
        }
        let status = presence(before, after, !changes.is_empty());
        if status != Status::Unchanged {
            diffs.push(IndexDiff {
                name: name.to_owned(),
                status,
                changes,
                definition: Sides {
                    before: before.map(|i| i.definition.clone()),
                    after: after.map(|i| i.definition.clone()),
                },
            });
        }
    }
    diffs
}

fn policies(old: &[m::Policy], new: &[m::Policy]) -> Vec<PolicyDiff> {
    let mut diffs = Vec::new();
    for (name, before, after) in by_key(old, new, |p| p.name.as_str()) {
        let mut changes = Vec::new();
        if let (Some(b), Some(a)) = (before, after) {
            field!(
                changes,
                PolicyChange::Command,
                b.command,
                a.command,
                PolicyCommand::from
            );
            field!(
                changes,
                PolicyChange::Permissive,
                b.permissive,
                a.permissive
            );
            field!(
                changes,
                PolicyChange::Roles,
                sorted(&b.roles),
                sorted(&a.roles)
            );
            field!(changes, PolicyChange::Using, b.using, a.using);
            field!(changes, PolicyChange::Check, b.check, a.check);
        }
        let status = presence(before, after, !changes.is_empty());
        if status != Status::Unchanged {
            diffs.push(PolicyDiff {
                name: name.to_owned(),
                status,
                changes,
                definition: Sides {
                    before: before.map(sql::policy_definition),
                    after: after.map(sql::policy_definition),
                },
            });
        }
    }
    diffs
}

/// Keyed by parent and child, so a partition attached to another parent is a
/// removal and an addition, like any rename.
fn partitions(old: &[&m::Partition], new: &[&m::Partition]) -> Vec<PartitionDiff> {
    let key = |p: &&m::Partition| {
        (
            p.parent_schema.clone(),
            p.parent.clone(),
            p.child_schema.clone(),
            p.child.clone(),
        )
    };
    let mut diffs = Vec::new();
    for (_, before, after) in by_key(old, new, key) {
        let Some(any) = after.or(before) else {
            continue;
        };
        let mut changes = Vec::new();
        if let (Some(b), Some(a)) = (before, after) {
            field!(changes, PartitionChange::Bound, b.bound, a.bound);
        }
        let status = presence(before, after, !changes.is_empty());
        if status != Status::Unchanged {
            diffs.push(PartitionDiff {
                schema: any.child_schema.clone(),
                name: any.child.clone(),
                qualified_name: qualified(&any.child_schema, &any.child),
                parent: parent_name(any),
                status,
                changes,
                definition: Sides {
                    before: before.map(|p| sql::partition_bound(p)),
                    after: after.map(|p| sql::partition_bound(p)),
                },
            });
        }
    }
    diffs
}

fn enum_type(
    old: &Schema,
    new: &Schema,
    before: Option<&m::EnumType>,
    after: Option<&m::EnumType>,
) -> Option<EnumDiff> {
    let any = after.or(before)?;
    let mut labels = Vec::new();
    let old_labels = before.map_or(&[][..], |e| &e.labels);
    let new_labels = after.map_or(&[][..], |e| &e.labels);
    for p in placed(old_labels, new_labels, String::as_str) {
        let status = presence(p.before, p.after, p.moved);
        if status != Status::Unchanged {
            labels.push(LabelDiff {
                label: p.name.to_owned(),
                status,
                position: Positions {
                    before: p.before.map(|(i, _)| i),
                    after: p.after.map(|(i, _)| i),
                },
            });
        }
    }
    let mut changes = Vec::new();
    if let (Some(b), Some(a)) = (before, after) {
        field!(changes, EnumChange::Comment, b.comment, a.comment);
    }
    let status = presence(before, after, !changes.is_empty() || !labels.is_empty());
    (status != Status::Unchanged).then(|| EnumDiff {
        schema: any.schema.clone(),
        name: any.name.clone(),
        qualified_name: qualified(&any.schema, &any.name),
        status,
        changes,
        labels,
        lines: Vec::new(),
        sql: Sides {
            before: before.map(|e| sql::enum_sql(old, e)),
            after: after.map(|e| sql::enum_sql(new, e)),
        },
    })
}

fn domain(
    old: &Schema,
    new: &Schema,
    before: Option<&m::DomainType>,
    after: Option<&m::DomainType>,
) -> Option<DomainDiff> {
    let any = after.or(before)?;
    let mut changes = Vec::new();
    if let (Some(b), Some(a)) = (before, after) {
        field!(changes, DomainChange::BaseType, b.base_type, a.base_type);
        field!(changes, DomainChange::NotNull, b.not_null, a.not_null);
        field!(changes, DomainChange::Default, b.default, a.default);
        field!(
            changes,
            DomainChange::Constraints,
            sorted(&b.constraints),
            sorted(&a.constraints)
        );
        field!(changes, DomainChange::Comment, b.comment, a.comment);
    }
    let status = presence(before, after, !changes.is_empty());
    (status != Status::Unchanged).then(|| DomainDiff {
        schema: any.schema.clone(),
        name: any.name.clone(),
        qualified_name: qualified(&any.schema, &any.name),
        status,
        changes,
        lines: Vec::new(),
        sql: Sides {
            before: before.map(|d| sql::domain_sql(old, d)),
            after: after.map(|d| sql::domain_sql(new, d)),
        },
    })
}

fn composite(
    old: &Schema,
    new: &Schema,
    before: Option<&m::CompositeType>,
    after: Option<&m::CompositeType>,
) -> Option<CompositeDiff> {
    let any = after.or(before)?;
    let mut attributes = Vec::new();
    let old_attributes = before.map_or(&[][..], |c| &c.attributes);
    let new_attributes = after.map_or(&[][..], |c| &c.attributes);
    for p in placed(old_attributes, new_attributes, |a| a.name.as_str()) {
        let mut changes = Vec::new();
        if let (Some((i, b)), Some((j, a))) = (p.before, p.after) {
            field!(changes, AttributeChange::Type, b.r#type, a.r#type);
            if p.moved {
                changes.push(AttributeChange::Position {
                    before: i,
                    after: j,
                });
            }
        }
        let status = presence(p.before, p.after, !changes.is_empty());
        if status != Status::Unchanged {
            attributes.push(AttributeDiff {
                name: p.name.to_owned(),
                status,
                position: Positions {
                    before: p.before.map(|(i, _)| i),
                    after: p.after.map(|(i, _)| i),
                },
                changes,
                definition: Sides {
                    before: p.before.map(|(_, a)| a.r#type.clone()),
                    after: p.after.map(|(_, a)| a.r#type.clone()),
                },
            });
        }
    }
    let mut changes = Vec::new();
    if let (Some(b), Some(a)) = (before, after) {
        field!(changes, CompositeChange::Comment, b.comment, a.comment);
    }
    let status = presence(before, after, !changes.is_empty() || !attributes.is_empty());
    (status != Status::Unchanged).then(|| CompositeDiff {
        schema: any.schema.clone(),
        name: any.name.clone(),
        qualified_name: qualified(&any.schema, &any.name),
        status,
        changes,
        attributes,
        lines: Vec::new(),
        sql: Sides {
            before: before.map(|c| sql::composite_sql(old, c)),
            after: after.map(|c| sql::composite_sql(new, c)),
        },
    })
}

fn extension(
    old: &Schema,
    new: &Schema,
    before: Option<&m::Extension>,
    after: Option<&m::Extension>,
) -> Option<ExtensionDiff> {
    let any = after.or(before)?;
    let mut changes = Vec::new();
    if let (Some(b), Some(a)) = (before, after) {
        field!(changes, ExtensionChange::Schema, b.schema, a.schema);
        field!(changes, ExtensionChange::Version, b.version, a.version);
    }
    let status = presence(before, after, !changes.is_empty());
    (status != Status::Unchanged).then(|| ExtensionDiff {
        name: any.name.clone(),
        status,
        changes,
        lines: Vec::new(),
        sql: Sides {
            before: before.map(|x| sql::extension_sql(old, x)),
            after: after.map(|x| sql::extension_sql(new, x)),
        },
    })
}

impl From<&m::EntityKind> for EntityKind {
    fn from(kind: &m::EntityKind) -> Self {
        match kind {
            m::EntityKind::Table => Self::Table,
            m::EntityKind::PartitionedTable => Self::PartitionedTable,
            m::EntityKind::View => Self::View,
            m::EntityKind::MaterializedView => Self::MaterializedView,
            m::EntityKind::ForeignTable => Self::ForeignTable,
        }
    }
}

impl From<&m::ConstraintKind> for ConstraintKind {
    fn from(kind: &m::ConstraintKind) -> Self {
        match kind {
            m::ConstraintKind::PrimaryKey => Self::PrimaryKey,
            m::ConstraintKind::Unique => Self::Unique,
            m::ConstraintKind::ForeignKey => Self::ForeignKey,
            m::ConstraintKind::Check => Self::Check,
            m::ConstraintKind::Exclusion => Self::Exclusion,
        }
    }
}

impl From<&m::TypeKind> for TypeKind {
    fn from(kind: &m::TypeKind) -> Self {
        match kind {
            m::TypeKind::Base => Self::Base,
            m::TypeKind::Enum => Self::Enum,
            m::TypeKind::Domain => Self::Domain,
            m::TypeKind::Composite => Self::Composite,
            m::TypeKind::Range => Self::Range,
            m::TypeKind::Multirange => Self::Multirange,
            m::TypeKind::Pseudo => Self::Pseudo,
        }
    }
}

impl From<&m::Identity> for Identity {
    fn from(identity: &m::Identity) -> Self {
        match identity {
            m::Identity::Always => Self::Always,
            m::Identity::ByDefault => Self::ByDefault,
        }
    }
}

impl From<&m::Generated> for Generated {
    fn from(generated: &m::Generated) -> Self {
        match generated {
            m::Generated::Stored => Self::Stored,
            m::Generated::Virtual => Self::Virtual,
        }
    }
}

impl From<&m::PolicyCommand> for PolicyCommand {
    fn from(command: &m::PolicyCommand) -> Self {
        match command {
            m::PolicyCommand::All => Self::All,
            m::PolicyCommand::Select => Self::Select,
            m::PolicyCommand::Insert => Self::Insert,
            m::PolicyCommand::Update => Self::Update,
            m::PolicyCommand::Delete => Self::Delete,
        }
    }
}
