//! A partition becoming a table of its own, or a table becoming a
//! partition: `DETACH PARTITION` and `ATTACH PARTITION`, which keep the
//! rows, never a drop and a create.
//!
//! remus lists no partition among the entities (a partition is its parent's
//! columns, constraints and indexes), so each side of such a change is made
//! up before planning:
//!
//! - a partition that the new schema has as a table gets an old entity
//!   predicted from its root: the root's columns, its CHECKs and foreign
//!   keys under their own names, and its key, unique and exclusion
//!   constraints and indexes under the names Postgres gives a partition's
//!   copies (`<partition>_pkey`, `<partition>_<columns>_idx`…). Planning that
//!   entity against the new table aligns whatever differs after the detach;
//!   a predicted object dropped is dropped `IF EXISTS`.
//! - a table that the new schema has as a partition gets a new entity: the
//!   table as it was, with the root's columns (`ATTACH` wants the same
//!   columns, types and `NOT NULL`s) and the root's CHECKs added (it wants
//!   those too, by name).

use std::collections::{BTreeMap, BTreeSet};

use remus_core::{
    Schema,
    emit::ident::{qualified, quote_ident},
    model::{Constraint, ConstraintKind, Entity, EntityKind, Index, Partition},
};

pub(crate) type Name = (String, String);

/// The partitions detached (old partitions, new tables) and attached (old
/// tables, new partitions), after their missing side was added.
#[derive(Debug, Default)]
pub(crate) struct Moves {
    pub detached: BTreeSet<Name>,
    pub attached: BTreeSet<Name>,
}

fn child(p: &Partition) -> Name {
    (p.child_schema.clone(), p.child.clone())
}

/// The entity at the top of `p`'s tree in `schema`.
fn root<'s>(schema: &'s Schema, p: &Partition) -> Option<&'s Entity> {
    let parents: BTreeMap<Name, &Partition> = schema.partitions.iter().map(|p| (child(p), p)).collect();
    let mut at = (p.parent_schema.clone(), p.parent.clone());
    let mut seen = BTreeSet::new();
    loop {
        if let Some(e) = schema.entities.iter().find(|e| e.schema == at.0 && e.name == at.1) {
            return Some(e);
        }
        if !seen.insert(at.clone()) {
            return None;
        }
        let up = parents.get(&at)?;
        at = (up.parent_schema.clone(), up.parent.clone());
    }
}

/// Adds the missing side of every detach and attach to `old` and `new`.
pub(crate) fn reconcile(old: &mut Schema, new: &mut Schema) -> Moves {
    let mut moves = Moves::default();
    let has_entity = |s: &Schema, n: &Name| s.entities.iter().any(|e| e.schema == n.0 && e.name == n.1);
    let has_partition = |s: &Schema, n: &Name| s.partitions.iter().any(|p| child(p) == *n);

    let mut predicted = Vec::new();
    for p in &old.partitions {
        let name = child(p);
        if has_entity(new, &name) && !has_partition(new, &name) && !has_entity(old, &name)
            && let Some(entity) = root(old, p).map(|r| detached(r, p))
        {
            predicted.push(entity);
            moves.detached.insert(name);
        }
    }
    old.entities.extend(predicted);

    let mut made = Vec::new();
    for p in &new.partitions {
        let name = child(p);
        if has_entity(old, &name) && !has_partition(old, &name) && !has_entity(new, &name) {
            let table = old
                .entities
                .iter()
                .find(|e| e.schema == name.0 && e.name == name.1)
                .expect("checked above");
            if let Some(r) = root(new, p) {
                made.push(attached(table, r, p));
                moves.attached.insert(name);
            }
        }
    }
    new.entities.extend(made);
    moves
}

/// The table a partition is once detached, as far as its root tells.
fn detached(root: &Entity, p: &Partition) -> Entity {
    let name = p.child.clone();
    let mut taken: BTreeSet<String> = BTreeSet::new();
    let mut constraints = Vec::new();
    for c in &root.constraints {
        let renamed = match c.kind {
            ConstraintKind::PrimaryKey => Some(relation_name(&name, &[], "pkey", &mut taken)),
            ConstraintKind::Unique => Some(relation_name(&name, &c.columns, "key", &mut taken)),
            ConstraintKind::Exclusion => Some(relation_name(&name, &c.columns, "excl", &mut taken)),
            // CHECKs and foreign keys keep their names on partitions.
            ConstraintKind::Check | ConstraintKind::ForeignKey => None,
        };
        constraints.push(Constraint {
            name: renamed.unwrap_or_else(|| c.name.clone()),
            comment: None,
            ..c.clone()
        });
    }
    let indexes = root
        .indexes
        .iter()
        .map(|i| {
            let columns = index_columns(&i.definition);
            let index = relation_name(&name, &columns, "idx", &mut taken);
            Index {
                definition: index_on(&i.definition, &index, &qualified(&p.child_schema, &name)),
                name: index,
                ..i.clone()
            }
        })
        .collect();
    Entity {
        schema: p.child_schema.clone(),
        name,
        kind: if p.partition_key.is_some() {
            EntityKind::PartitionedTable
        } else {
            EntityKind::Table
        },
        comment: None,
        rls: false,
        rls_forced: false,
        unlogged: false,
        partition_key: p.partition_key.clone(),
        view_definition: None,
        depends_on: Vec::new(),
        columns: root
            .columns
            .iter()
            .map(|c| remus_core::model::Column {
                comment: None,
                ..c.clone()
            })
            .collect(),
        constraints,
        indexes,
        policies: Vec::new(),
    }
}

/// The table as `ATTACH` wants it: its own constraints, indexes, policies
/// and comments, the root's columns and CHECKs.
fn attached(table: &Entity, root: &Entity, p: &Partition) -> Entity {
    let columns = root
        .columns
        .iter()
        .map(|c| {
            let comment = table.column(&c.name).and_then(|own| own.comment.clone());
            remus_core::model::Column {
                comment,
                identity: None,
                ..c.clone()
            }
        })
        .collect();
    let mut constraints = table.constraints.clone();
    for check in root.constraints.iter().filter(|c| c.kind == ConstraintKind::Check) {
        if !constraints.iter().any(|c| c.name == check.name) {
            constraints.push(Constraint {
                comment: None,
                ..check.clone()
            });
        }
    }
    Entity {
        kind: if p.partition_key.is_some() {
            EntityKind::PartitionedTable
        } else {
            EntityKind::Table
        },
        partition_key: p.partition_key.clone(),
        columns,
        constraints,
        ..table.clone()
    }
}

/// The column names an index definition lists (`expr` for an
/// expression), as Postgres names a partition's copy after them.
fn index_columns(definition: &str) -> Vec<String> {
    let Some(open) = definition.find(" USING ").and_then(|at| definition[at..].find('(').map(|o| at + o)) else {
        return Vec::new();
    };
    let mut depth = 0;
    let mut part = String::new();
    let mut parts = Vec::new();
    for c in definition[open + 1..].chars() {
        match c {
            '(' => {
                depth += 1;
                part.push(c);
            }
            ')' if depth == 0 => {
                parts.push(std::mem::take(&mut part));
                break;
            }
            ')' => {
                depth -= 1;
                part.push(c);
            }
            ',' if depth == 0 => parts.push(std::mem::take(&mut part)),
            _ => part.push(c),
        }
    }
    parts
        .iter()
        .map(|p| {
            let word = p.split_whitespace().next().unwrap_or("");
            let bare = word.trim_matches('"');
            let plain = bare.chars().all(|c| c.is_alphanumeric() || c == '_') && !bare.is_empty();
            if plain { bare.to_owned() } else { "expr".to_owned() }
        })
        .collect()
}

/// `CREATE [UNIQUE] INDEX <name> ON [ONLY] <table> USING …` for another
/// name and table.
fn index_on(definition: &str, name: &str, table: &str) -> String {
    let unique = definition.starts_with("CREATE UNIQUE INDEX ");
    let rest = definition.find(" USING ").map_or("", |at| &definition[at..]);
    format!(
        "CREATE {}INDEX {} ON {table}{rest}",
        if unique { "UNIQUE " } else { "" },
        quote_ident(name)
    )
}

/// Postgres's `ChooseRelationName`: `<name>_<columns>_<label>`, cut to 63
/// bytes the way `makeObjectName` cuts it, numbered when taken.
fn relation_name(table: &str, columns: &[String], label: &str, taken: &mut BTreeSet<String>) -> String {
    let addition = columns.join("_");
    let mut n = 0;
    loop {
        let suffix = if n == 0 { label.to_owned() } else { format!("{label}{n}") };
        let candidate = object_name(table, &addition, &suffix);
        if taken.insert(candidate.clone()) {
            return candidate;
        }
        n += 1;
    }
}

/// `makeObjectName`: `name1_name2_label` within 63 bytes, the longer of the
/// two names shortened first.
fn object_name(name1: &str, name2: &str, label: &str) -> String {
    let mut overhead = label.len() + 1;
    if !name2.is_empty() {
        overhead += 1;
    }
    let available = 63usize.saturating_sub(overhead);
    let (mut n1, mut n2) = (name1.len(), name2.len());
    while n1 + n2 > available {
        if n1 > n2 {
            n1 -= 1;
        } else {
            n2 -= 1;
        }
    }
    let clip = |s: &str, mut n: usize| {
        while !s.is_char_boundary(n) {
            n -= 1;
        }
        s[..n].to_owned()
    };
    if name2.is_empty() {
        format!("{}_{label}", clip(name1, n1))
    } else {
        format!("{}_{}_{label}", clip(name1, n1), clip(name2, n2))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::{index_columns, index_on, object_name, relation_name};

    #[test]
    fn partition_copies_are_named_like_postgres_names_them() {
        let mut taken = BTreeSet::new();
        assert_eq!(relation_name("logs_2025", &[], "pkey", &mut taken), "logs_2025_pkey");
        assert_eq!(
            relation_name("logs_2025", &["at".into()], "idx", &mut taken),
            "logs_2025_at_idx"
        );
        assert_eq!(
            relation_name("logs_2025", &["at".into()], "idx", &mut taken),
            "logs_2025_at_idx1"
        );
        let long = "a".repeat(70);
        assert_eq!(object_name(&long, "b", "idx").len(), 63);
    }

    #[test]
    fn index_columns_and_targets_are_read_off_the_definition() {
        let def = "CREATE INDEX logs_at_idx ON ONLY public.logs USING btree (at, lower(line))";
        assert_eq!(index_columns(def), ["at", "expr"]);
        assert_eq!(
            index_on(def, "logs_2025_at_expr_idx", "public.logs_2025"),
            "CREATE INDEX logs_2025_at_expr_idx ON public.logs_2025 USING btree (at, lower(line))"
        );
    }
}
