//! SQL text for the side-by-side view: whole schemas, single objects, and the
//! one-line definitions of their parts.
//!
//! Everything goes through `remus-sql`, so the text is what a user sees when
//! they export the version, after [`normalize`] has put both sides in the
//! same order.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write,
};

use remus_core::{
    Schema,
    emit::ident::quote_ident,
    model::{
        Column, CompositeType, ConstraintKind, DomainType, Entity, EnumType, Extension, Generated,
        Identity, Partition, Policy, PolicyCommand,
    },
};

/// A copy of `schema` in a canonical order, so two versions render in the
/// same order and a line diff of their SQL shows only real changes.
///
/// Starts from [`Schema::canonical`], then sorts everything whose order the
/// model does not define: objects by schema and name, constraints by kind and
/// name (as `introspect.sql` does), indexes and policies by name, partitions
/// by their path from the top of their tree (so a sub-partitioned partition
/// comes before its own partitions), and the lists the catalog returns in OID
/// or constraint-name order (policy roles, domain checks, view dependencies).
/// A partition without a bound gets `DEFAULT`, which is what `remus-sql`
/// prints for it anyway. Column, enum label and composite attribute order is
/// meaningful and kept.
pub fn normalize(schema: &Schema) -> Schema {
    let mut schema = schema.clone().canonical();
    schema
        .entities
        .sort_by(|a, b| (&a.schema, &a.name).cmp(&(&b.schema, &b.name)));
    for entity in &mut schema.entities {
        normalize_entity(entity);
    }
    schema
        .enums
        .sort_by(|a, b| (&a.schema, &a.name).cmp(&(&b.schema, &b.name)));
    schema
        .domains
        .sort_by(|a, b| (&a.schema, &a.name).cmp(&(&b.schema, &b.name)));
    for domain in &mut schema.domains {
        domain.constraints.sort();
    }
    schema
        .composites
        .sort_by(|a, b| (&a.schema, &a.name).cmp(&(&b.schema, &b.name)));
    for partition in &mut schema.partitions {
        partition.bound.get_or_insert_with(|| "DEFAULT".to_owned());
    }
    let paths = partition_paths(&schema.partitions);
    schema
        .partitions
        .sort_by_cached_key(|p| paths.get(&child(p)).cloned().unwrap_or_default());
    schema.extensions.sort_by(|a, b| a.name.cmp(&b.name));
    schema
}

/// The whole schema rendered by `remus-sql` after [`normalize`]. Render both
/// sides with it for a full side-by-side diff. The header names the database
/// and server version, so it differs when those do.
pub fn render_sql(schema: &Schema) -> String {
    remus_sql::render(&normalize(schema))
}

fn normalize_entity(entity: &mut Entity) {
    entity
        .constraints
        .sort_by(|a, b| (kind_code(&a.kind), &a.name).cmp(&(kind_code(&b.kind), &b.name)));
    entity.indexes.sort_by(|a, b| a.name.cmp(&b.name));
    entity.policies.sort_by(|a, b| a.name.cmp(&b.name));
    for policy in &mut entity.policies {
        policy.roles.sort();
    }
    entity.depends_on.sort();
}

/// `introspect.sql` orders constraints by `contype`, the catalog letter.
fn kind_code(kind: &ConstraintKind) -> char {
    match kind {
        ConstraintKind::Check => 'c',
        ConstraintKind::ForeignKey => 'f',
        ConstraintKind::PrimaryKey => 'p',
        ConstraintKind::Unique => 'u',
        ConstraintKind::Exclusion => 'x',
    }
}

type Name = (String, String);

fn child(p: &Partition) -> Name {
    (p.child_schema.clone(), p.child.clone())
}

fn parent(p: &Partition) -> Name {
    (p.parent_schema.clone(), p.parent.clone())
}

/// For each partition, the names from the top of its tree down to it. The
/// walk stops at a name seen twice, so a hand-written cycle cannot hang it.
fn partition_paths(partitions: &[Partition]) -> BTreeMap<Name, Vec<Name>> {
    let mut parents: BTreeMap<Name, Name> = BTreeMap::new();
    for p in partitions {
        parents.entry(child(p)).or_insert_with(|| parent(p));
    }
    parents
        .keys()
        .map(|leaf| {
            let mut path = vec![leaf.clone()];
            let mut seen = BTreeSet::from([leaf]);
            let mut at = leaf;
            while let Some(up) = parents.get(at) {
                if !seen.insert(up) {
                    break;
                }
                path.push(up.clone());
                at = up;
            }
            path.reverse();
            (leaf.clone(), path)
        })
        .collect()
}

/// `sql` with every whitespace run outside quotes reduced to one space, and
/// none at either end: what two Postgres majors agree on when they deparse
/// the same view or check differently. Quoted literals and identifiers are
/// kept byte for byte, since a space inside them is part of the value.
pub(crate) fn squash(sql: &str) -> String {
    let mut out = String::with_capacity(sql.len());
    let mut quote = None;
    let mut space = false;
    for c in sql.chars() {
        match quote {
            Some(q) => {
                out.push(c);
                if c == q {
                    quote = None;
                }
            }
            None if c.is_whitespace() => space = true,
            None => {
                if space && !out.is_empty() {
                    out.push(' ');
                }
                space = false;
                if c == '\'' || c == '"' {
                    quote = Some(c);
                }
                out.push(c);
            }
        }
    }
    out
}

/// A schema holding nothing, with the metadata of `like` (which only reaches
/// the header, and the header is cut).
fn empty_like(like: &Schema) -> Schema {
    Schema {
        remus_version: like.remus_version.clone(),
        generated_at: None,
        database: like.database.clone(),
        server_version: like.server_version.clone(),
        entities: Vec::new(),
        enums: Vec::new(),
        domains: Vec::new(),
        composites: Vec::new(),
        partitions: Vec::new(),
        extensions: Vec::new(),
    }
}

/// `remus-sql` output for a schema holding one object, minus the header and
/// the `CREATE SCHEMA` lines, which belong to no object in particular.
fn render_alone(schema: Schema) -> String {
    let sql = remus_sql::render(&normalize(&schema));
    let body = sql.split_once("\n\n").map_or("", |(_, body)| body);
    let mut out = String::with_capacity(body.len());
    for line in body.lines() {
        if !line.starts_with("CREATE SCHEMA IF NOT EXISTS ") {
            out.push_str(line);
            out.push('\n');
        }
    }
    let trimmed = out.trim();
    if trimmed.is_empty() {
        String::new()
    } else {
        format!("{trimmed}\n")
    }
}

pub(crate) fn entity_sql(like: &Schema, entity: &Entity, partitions: &[&Partition]) -> String {
    let mut schema = empty_like(like);
    schema.entities.push(entity.clone());
    schema.partitions = partitions.iter().map(|p| (*p).clone()).collect();
    render_alone(schema)
}

/// Partitions alone, for the ones no entity owns.
pub(crate) fn partitions_sql(like: &Schema, partitions: &[&Partition]) -> String {
    let mut schema = empty_like(like);
    schema.partitions = partitions.iter().map(|p| (*p).clone()).collect();
    render_alone(schema)
}

pub(crate) fn enum_sql(like: &Schema, e: &EnumType) -> String {
    let mut schema = empty_like(like);
    schema.enums.push(e.clone());
    render_alone(schema)
}

pub(crate) fn domain_sql(like: &Schema, d: &DomainType) -> String {
    let mut schema = empty_like(like);
    schema.domains.push(d.clone());
    render_alone(schema)
}

pub(crate) fn composite_sql(like: &Schema, c: &CompositeType) -> String {
    let mut schema = empty_like(like);
    schema.composites.push(c.clone());
    render_alone(schema)
}

pub(crate) fn extension_sql(like: &Schema, x: &Extension) -> String {
    let mut schema = empty_like(like);
    schema.extensions.push(x.clone());
    render_alone(schema)
}

/// A column as `remus-sql` writes it inside `CREATE TABLE`, without the name.
pub(crate) fn column_definition(column: &Column) -> String {
    let mut line = column.r#type.clone();
    match (&column.generated, &column.identity, &column.default) {
        (Some(kind), _, Some(expression)) => {
            let storage = match kind {
                Generated::Stored => "STORED",
                Generated::Virtual => "VIRTUAL",
            };
            let _ = write!(line, " GENERATED ALWAYS AS ({expression}) {storage}");
        }
        (Some(_), _, None) => {}
        (None, Some(identity), _) => {
            let when = match identity {
                Identity::Always => "ALWAYS",
                Identity::ByDefault => "BY DEFAULT",
            };
            let _ = write!(line, " GENERATED {when} AS IDENTITY");
        }
        (None, None, Some(default)) => {
            let _ = write!(line, " DEFAULT {default}");
        }
        (None, None, None) => {}
    }
    if column.not_null {
        line.push_str(" NOT NULL");
    }
    line
}

/// A policy as `remus-sql` writes it after `CREATE POLICY name ON table`.
pub(crate) fn policy_definition(policy: &Policy) -> String {
    let mut out = String::new();
    if !policy.permissive {
        out.push_str("AS RESTRICTIVE ");
    }
    let command = match policy.command {
        PolicyCommand::All => "ALL",
        PolicyCommand::Select => "SELECT",
        PolicyCommand::Insert => "INSERT",
        PolicyCommand::Update => "UPDATE",
        PolicyCommand::Delete => "DELETE",
    };
    let _ = write!(out, "FOR {command}");
    if !policy.roles.is_empty() {
        // Sorted before quoting, as `normalize` leaves them for `remus-sql`.
        let mut roles: Vec<&str> = policy.roles.iter().map(String::as_str).collect();
        roles.sort_unstable();
        let roles: Vec<_> = roles.into_iter().map(quote_ident).collect();
        let _ = write!(out, " TO {}", roles.join(", "));
    }
    if let Some(using) = &policy.using {
        let _ = write!(out, " USING {using}");
    }
    if let Some(check) = &policy.check {
        let _ = write!(out, " WITH CHECK {check}");
    }
    out
}

pub(crate) fn partition_bound(partition: &Partition) -> String {
    let bound = partition.bound.as_deref().unwrap_or("DEFAULT");
    match &partition.partition_key {
        Some(key) => format!("{bound} PARTITION BY {key}"),
        None => bound.to_owned(),
    }
}
