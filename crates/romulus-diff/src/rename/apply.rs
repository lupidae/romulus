//! One rename done to a model, everything that depends on the object
//! following as it does in Postgres (see `sql.rs` for how each kind of text
//! is rewritten).

use std::collections::BTreeSet;

use remus_core::{
    Schema,
    model::{ConstraintKind, Entity, RelationRef, TypeKind},
};

use super::{
    Rename, RenameKind,
    sql::{
        self, ViewRewrite, qualifiers, rename_bare_column, rename_constraint_column,
        rename_index_column, rename_index_definition, rename_label, rename_qualified_column,
        rename_reference_column, rename_relation, rename_sequence, rename_type, rename_type_name,
        view_column,
    },
};

type Name = (String, String);

/// Does `rename` to `schema`. Views whose new print cannot be told are
/// added to `rebuild` (by their name once this rename is done).
pub(crate) fn apply(schema: &mut Schema, rename: &Rename, rebuild: &mut BTreeSet<Name>) {
    // A move first (`ALTER … SET SCHEMA`), then the rename in its new
    // schema, if it has one too.
    if let Some(to_schema) = rename.to_schema.as_deref().filter(|t| *t != rename.schema) {
        let (s, name) = (rename.schema.as_str(), rename.from.as_str());
        match rename.kind {
            RenameKind::Table | RenameKind::View | RenameKind::MaterializedView => {
                move_relation(schema, s, name, to_schema, rebuild);
            }
            RenameKind::Enum | RenameKind::Domain | RenameKind::Composite => {
                move_type_to(schema, rename.kind, s, name, to_schema);
            }
            _ => return,
        }
        if rename.from != rename.to {
            let renamed = Rename {
                schema: to_schema.to_owned(),
                to_schema: None,
                ..rename.clone()
            };
            apply(schema, &renamed, rebuild);
        }
        return;
    }
    let (s, from, to) = (
        rename.schema.as_str(),
        rename.from.as_str(),
        rename.to.as_str(),
    );
    let parent = rename.parent.as_deref().unwrap_or("");
    match rename.kind {
        RenameKind::Table | RenameKind::View | RenameKind::MaterializedView => {
            relation(schema, s, from, to, rebuild);
        }
        RenameKind::Partition => {
            for p in &mut schema.partitions {
                if p.child_schema == s && p.child == from {
                    to.clone_into(&mut p.child);
                }
            }
            relation(schema, s, from, to, rebuild);
        }
        RenameKind::Column => column(schema, s, parent, from, to, rebuild),
        RenameKind::Constraint => {
            if let Some(c) = entity_mut(schema, s, parent)
                .and_then(|e| e.constraints.iter_mut().find(|c| c.name == from))
            {
                to.clone_into(&mut c.name);
            }
        }
        RenameKind::Index => {
            if let Some(i) = entity_mut(schema, s, parent)
                .and_then(|e| e.indexes.iter_mut().find(|i| i.name == from))
            {
                to.clone_into(&mut i.name);
                i.definition = rename_index_definition(&i.definition, from, to);
            }
        }
        RenameKind::Policy => {
            if let Some(p) = entity_mut(schema, s, parent)
                .and_then(|e| e.policies.iter_mut().find(|p| p.name == from))
            {
                to.clone_into(&mut p.name);
            }
        }
        RenameKind::Sequence => {
            for e in &mut schema.entities {
                for c in &mut e.columns {
                    if let Some(d) = &mut c.default {
                        *d = rename_sequence(d, s, from, to);
                    }
                }
            }
        }
        RenameKind::Enum | RenameKind::Domain | RenameKind::Composite => {
            type_name(schema, rename.kind, s, from, to);
        }
        RenameKind::EnumValue => label(schema, s, parent, from, to),
        RenameKind::Attribute => {
            if let Some(c) = schema
                .composites
                .iter_mut()
                .find(|c| c.schema == s && c.name == parent)
                && let Some(a) = c.attributes.iter_mut().find(|a| a.name == from)
            {
                to.clone_into(&mut a.name);
            }
        }
    }
}

fn entity_mut<'a>(schema: &'a mut Schema, s: &str, name: &str) -> Option<&'a mut Entity> {
    schema
        .entities
        .iter_mut()
        .find(|e| e.schema == s && e.name == name)
}

/// Every expression of the model, with what owns it: for the rewrites that
/// apply anywhere (types, labels, relations).
fn each_expression(schema: &mut Schema, mut f: impl FnMut(&mut String)) {
    for e in &mut schema.entities {
        for c in &mut e.columns {
            if let Some(d) = &mut c.default {
                f(d);
            }
        }
        for c in &mut e.constraints {
            f(&mut c.definition);
        }
        for i in &mut e.indexes {
            f(&mut i.definition);
        }
        for p in &mut e.policies {
            if let Some(u) = &mut p.using {
                f(u);
            }
            if let Some(c) = &mut p.check {
                f(c);
            }
        }
        if let Some(v) = &mut e.view_definition {
            f(v);
        }
    }
    for d in &mut schema.domains {
        if let Some(default) = &mut d.default {
            f(default);
        }
        for c in &mut d.constraints {
            f(c);
        }
    }
}

/// A table, view, materialized view or partition renamed: foreign keys to
/// it, indexes on it, and every view and policy reading it follow.
fn relation(schema: &mut Schema, s: &str, from: &str, to: &str, rebuild: &mut BTreeSet<Name>) {
    for e in &mut schema.entities {
        if e.schema == s && e.name == from {
            to.clone_into(&mut e.name);
        }
        for c in &mut e.constraints {
            if c.kind == ConstraintKind::ForeignKey
                && c.ref_schema.as_deref() == Some(s)
                && c.ref_table.as_deref() == Some(from)
            {
                c.ref_table = Some(to.to_owned());
                c.definition = rename_relation(&c.definition, s, from, to);
            }
        }
        for i in &mut e.indexes {
            i.definition = rename_relation(&i.definition, s, from, to);
        }
        for p in &mut e.policies {
            for expr in [&mut p.using, &mut p.check].into_iter().flatten() {
                *expr = rename_relation(expr, s, from, to);
            }
        }
        if let Some(v) = &mut e.view_definition {
            *v = rename_relation(v, s, from, to);
        }
        for d in &mut e.depends_on {
            if d.schema == s && d.name == from {
                to.clone_into(&mut d.name);
            }
        }
    }
    for p in &mut schema.partitions {
        if p.parent_schema == s && p.parent == from {
            to.clone_into(&mut p.parent);
        }
    }
    let old = (s.to_owned(), from.to_owned());
    if rebuild.remove(&old) {
        rebuild.insert((s.to_owned(), to.to_owned()));
    }
}

/// A column of `table` renamed: its keys, indexes, policies, generated
/// columns and partition keys name it anew; foreign keys to it, the views
/// over it and other tables' policies reading it follow.
#[allow(clippy::too_many_lines)]
fn column(
    schema: &mut Schema,
    s: &str,
    table: &str,
    from: &str,
    to: &str,
    rebuild: &mut BTreeSet<Name>,
) {
    let target = RelationRef {
        schema: s.to_owned(),
        name: table.to_owned(),
    };
    for e in &mut schema.entities {
        let own = e.schema == s && e.name == table;
        if own {
            for c in &mut e.columns {
                if c.name == from {
                    to.clone_into(&mut c.name);
                } else if c.generated.is_some()
                    && let Some(expression) = &mut c.default
                {
                    *expression = rename_bare_column(expression, from, to);
                }
            }
            for c in &mut e.constraints {
                let self_reference = c.kind == ConstraintKind::ForeignKey
                    && c.ref_schema.as_deref() == Some(s)
                    && c.ref_table.as_deref() == Some(table);
                for x in &mut c.columns {
                    if x == from {
                        to.clone_into(x);
                    }
                }
                if self_reference {
                    for x in &mut c.ref_columns {
                        if x == from {
                            to.clone_into(x);
                        }
                    }
                }
                c.definition = rename_constraint_column(&c.definition, from, to, self_reference);
            }
            for i in &mut e.indexes {
                i.definition = rename_index_column(&i.definition, from, to);
            }
            for p in &mut e.policies {
                for expr in [&mut p.using, &mut p.check].into_iter().flatten() {
                    let renamed = rename_bare_column(expr, from, to);
                    *expr = rename_qualified_column(&renamed, &[table.to_owned()], from, to);
                }
            }
            if let Some(key) = &mut e.partition_key {
                *key = rename_bare_column(key, from, to);
            }
            continue;
        }
        for c in &mut e.constraints {
            if c.kind == ConstraintKind::ForeignKey
                && c.ref_schema.as_deref() == Some(s)
                && c.ref_table.as_deref() == Some(table)
            {
                for x in &mut c.ref_columns {
                    if x == from {
                        to.clone_into(x);
                    }
                }
                c.definition = rename_reference_column(&c.definition, from, to);
            }
        }
        for p in &mut e.policies {
            for expr in [&mut p.using, &mut p.check].into_iter().flatten() {
                // Only where the expression reads the table: its name may
                // be another table's alias otherwise.
                let q = qualifiers(expr, s, table);
                if !q.is_empty() {
                    *expr = rename_qualified_column(expr, &q, from, to);
                }
            }
        }
        if let Some(definition) = &mut e.view_definition
            && e.depends_on.contains(&target)
            && super::sql::tokens(definition).iter().any(|t| t.is(from))
        {
            let mut q = qualifiers(definition, s, table);
            if q.is_empty() {
                q.push(table.to_owned());
            }
            let bare = e.depends_on.len() == 1;
            match view_column(definition, &q, bare, from, to) {
                ViewRewrite::Text(text) => *definition = text,
                ViewRewrite::Unknown => {
                    rebuild.insert((e.schema.clone(), e.name.clone()));
                }
            }
        }
    }
    // Sub-partitions of the table partition by its columns too.
    let mut tree = BTreeSet::from([(s.to_owned(), table.to_owned())]);
    loop {
        let before = tree.len();
        for p in &schema.partitions {
            if tree.contains(&(p.parent_schema.clone(), p.parent.clone())) {
                tree.insert((p.child_schema.clone(), p.child.clone()));
            }
        }
        if tree.len() == before {
            break;
        }
    }
    for p in &mut schema.partitions {
        if tree.contains(&(p.child_schema.clone(), p.child.clone()))
            && let Some(key) = &mut p.partition_key
        {
            *key = rename_bare_column(key, from, to);
        }
    }
}

/// An enum, domain or composite renamed: the columns, domains and
/// attributes of that type say its new name, and so do the casts to it.
fn type_name(schema: &mut Schema, kind: RenameKind, s: &str, from: &str, to: &str) {
    match kind {
        RenameKind::Enum => {
            for e in &mut schema.enums {
                if e.schema == s && e.name == from {
                    to.clone_into(&mut e.name);
                }
            }
        }
        RenameKind::Domain => {
            for d in &mut schema.domains {
                if d.schema == s && d.name == from {
                    to.clone_into(&mut d.name);
                }
            }
        }
        _ => {
            for c in &mut schema.composites {
                if c.schema == s && c.name == from {
                    to.clone_into(&mut c.name);
                }
            }
        }
    }
    let type_kind = match kind {
        RenameKind::Enum => TypeKind::Enum,
        RenameKind::Domain => TypeKind::Domain,
        _ => TypeKind::Composite,
    };
    for e in &mut schema.entities {
        for c in &mut e.columns {
            if c.type_kind == type_kind && c.type_schema == s && c.type_name == from {
                to.clone_into(&mut c.type_name);
                c.r#type = rename_type_name(&c.r#type, s, from, to);
            }
        }
    }
    for d in &mut schema.domains {
        d.base_type = rename_type_name(&d.base_type, s, from, to);
    }
    for c in &mut schema.composites {
        for a in &mut c.attributes {
            a.r#type = rename_type_name(&a.r#type, s, from, to);
        }
    }
    each_expression(schema, |expr| *expr = rename_type(expr, s, from, to));
}

/// An enum label renamed: the enum, and the constants of it.
fn label(schema: &mut Schema, s: &str, ty: &str, from: &str, to: &str) {
    for e in &mut schema.enums {
        if e.schema == s && e.name == ty {
            for l in &mut e.labels {
                if l == from {
                    to.clone_into(l);
                }
            }
        }
    }
    each_expression(schema, |expr| *expr = rename_label(expr, s, ty, from, to));
}

/// A table, view or materialized view moved to another schema: what names
/// it qualified follows, and the sequences its columns draw from move with
/// it (Postgres moves a table's own sequences, indexes and keys along).
fn move_relation(schema: &mut Schema, s: &str, name: &str, to: &str, rebuild: &mut BTreeSet<Name>) {
    for e in &mut schema.entities {
        let own = e.schema == s && e.name == name;
        if own {
            to.clone_into(&mut e.schema);
            for c in &mut e.columns {
                if c.generated.is_none()
                    && let Some(d) = &mut c.default
                {
                    *d = sql::move_sequences(d, s, to);
                }
            }
        }
        for c in &mut e.constraints {
            if c.kind == ConstraintKind::ForeignKey
                && c.ref_schema.as_deref() == Some(s)
                && c.ref_table.as_deref() == Some(name)
            {
                c.ref_schema = Some(to.to_owned());
                c.definition = sql::move_relation(&c.definition, s, name, to);
            }
        }
        for i in &mut e.indexes {
            i.definition = sql::move_relation(&i.definition, s, name, to);
        }
        for p in &mut e.policies {
            for expr in [&mut p.using, &mut p.check].into_iter().flatten() {
                *expr = sql::move_relation(expr, s, name, to);
            }
        }
        if let Some(v) = &mut e.view_definition {
            *v = sql::move_relation(v, s, name, to);
        }
        for d in &mut e.depends_on {
            if d.schema == s && d.name == name {
                to.clone_into(&mut d.schema);
            }
        }
    }
    for p in &mut schema.partitions {
        if p.parent_schema == s && p.parent == name {
            to.clone_into(&mut p.parent_schema);
        }
    }
    if rebuild.remove(&(s.to_owned(), name.to_owned())) {
        rebuild.insert((to.to_owned(), name.to_owned()));
    }
}

/// An enum, domain or composite moved to another schema: its columns,
/// domains, attributes and casts name it there.
fn move_type_to(schema: &mut Schema, kind: RenameKind, s: &str, name: &str, to: &str) {
    match kind {
        RenameKind::Enum => {
            for e in schema
                .enums
                .iter_mut()
                .filter(|e| e.schema == s && e.name == name)
            {
                to.clone_into(&mut e.schema);
            }
        }
        RenameKind::Domain => {
            for d in schema
                .domains
                .iter_mut()
                .filter(|d| d.schema == s && d.name == name)
            {
                to.clone_into(&mut d.schema);
            }
        }
        _ => {
            for c in schema
                .composites
                .iter_mut()
                .filter(|c| c.schema == s && c.name == name)
            {
                to.clone_into(&mut c.schema);
            }
        }
    }
    let type_kind = match kind {
        RenameKind::Enum => TypeKind::Enum,
        RenameKind::Domain => TypeKind::Domain,
        _ => TypeKind::Composite,
    };
    for e in &mut schema.entities {
        for c in &mut e.columns {
            if c.type_kind == type_kind && c.type_schema == s && c.type_name == name {
                to.clone_into(&mut c.type_schema);
                c.r#type = sql::move_type(&c.r#type, s, name, to);
            }
        }
    }
    for d in &mut schema.domains {
        d.base_type = sql::move_type(&d.base_type, s, name, to);
    }
    for c in &mut schema.composites {
        for a in &mut c.attributes {
            a.r#type = sql::move_type(&a.r#type, s, name, to);
        }
    }
    each_expression(schema, |expr| *expr = sql::move_type(expr, s, name, to));
}
