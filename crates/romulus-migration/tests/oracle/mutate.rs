//! Random mutations of the corpus schemas: each case applies a few edits a
//! person makes (add or drop a column, change a type, an index, a label…)
//! to a compiled model, compiles the result to get a canonical new model,
//! and checks the migration both ways. A mutation the sandbox rejects
//! (a check on a column that became text, say) is not a case; the test
//! fails if too few are valid.
//!
//! Seeds are fixed, so a failure names a case that can be run again:
//! `ROMULUS_ORACLE_SEEDS=<n>` checks more seeds per schema (default 12).

use remus_core::{
    Schema,
    emit::ident::{qualified, quote_ident, quote_literal},
    model::{
        Column, Constraint, ConstraintKind, Entity, EntityKind, EnumType, Index, Policy,
        PolicyCommand, RelationRef, TypeKind,
    },
};
use romulus_sandbox::render_sql;

use crate::{Case, check_all, compile, corpus, with_compiler};

/// splitmix64: small, fast, and the same everywhere.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            usize::try_from(self.next() % n as u64).unwrap_or(0)
        }
    }

    fn chance(&mut self, percent: u64) -> bool {
        self.next() % 100 < percent
    }

    fn pick<'a, T>(&mut self, items: &'a [T]) -> Option<&'a T> {
        if items.is_empty() {
            None
        } else {
            items.get(self.below(items.len()))
        }
    }
}

fn mentions(sql: &str, name: &str) -> bool {
    sql.split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .any(|word| word == name)
}

/// Indexes of the tables (not views, not foreign tables).
fn tables(schema: &Schema) -> Vec<usize> {
    schema
        .entities
        .iter()
        .enumerate()
        .filter(|(_, e)| e.kind.is_table())
        .map(|(i, _)| i)
        .collect()
}

fn column(
    name: &str,
    r#type: &str,
    kind: TypeKind,
    default: Option<&str>,
    not_null: bool,
) -> Column {
    Column {
        name: name.to_owned(),
        r#type: r#type.to_owned(),
        type_schema: "pg_catalog".to_owned(),
        type_name: r#type.to_owned(),
        type_kind: kind,
        is_array: r#type.ends_with("[]"),
        not_null,
        default: default.map(str::to_owned),
        identity: None,
        generated: None,
        comment: None,
    }
}

fn constraint(name: String, kind: ConstraintKind, definition: String) -> Constraint {
    Constraint {
        name,
        kind,
        columns: Vec::new(),
        ref_schema: None,
        ref_table: None,
        ref_columns: Vec::new(),
        on_update: None,
        on_delete: None,
        deferrable: false,
        definition,
        comment: None,
    }
}

/// Remove views that read `table` (and the views over those).
fn drop_views_over(schema: &mut Schema, table: &RelationRef) {
    let mut gone = vec![table.clone()];
    loop {
        let before = schema.entities.len();
        let doomed: Vec<RelationRef> = schema
            .entities
            .iter()
            .filter(|e| e.kind.is_view() && e.depends_on.iter().any(|d| gone.contains(d)))
            .map(Entity::reference)
            .collect();
        schema.entities.retain(|e| !doomed.contains(&e.reference()));
        gone.extend(doomed);
        if schema.entities.len() == before {
            break;
        }
    }
}

/// One edit, applied when the schema has something to apply it to.
#[allow(clippy::too_many_lines)]
fn mutate(schema: &mut Schema, rng: &mut Rng, n: usize) -> Option<String> {
    let tables = tables(schema);
    match rng.below(26) {
        // Add a column, anywhere.
        0 | 1 => {
            let t = *rng.pick(&tables)?;
            let mut choices: Vec<(String, TypeKind, Option<String>)> = vec![
                ("text".into(), TypeKind::Base, None),
                ("integer".into(), TypeKind::Base, Some("0".into())),
                ("bigint".into(), TypeKind::Base, None),
                ("boolean".into(), TypeKind::Base, Some("false".into())),
                (
                    "timestamp with time zone".into(),
                    TypeKind::Base,
                    Some("now()".into()),
                ),
                ("numeric(10,2)".into(), TypeKind::Base, None),
                ("jsonb".into(), TypeKind::Base, Some("'{}'::jsonb".into())),
                (
                    "uuid".into(),
                    TypeKind::Base,
                    Some("gen_random_uuid()".into()),
                ),
                ("character varying(40)".into(), TypeKind::Base, None),
                ("text[]".into(), TypeKind::Base, None),
            ];
            if let Some(e) = schema.enums.first() {
                let default = e
                    .labels
                    .first()
                    .map(|l| format!("{}::{}", quote_literal(l), qualified(&e.schema, &e.name)));
                choices.push((qualified(&e.schema, &e.name), TypeKind::Enum, default));
            }
            let (ty, kind, default) = rng.pick(&choices)?.clone();
            let default = if rng.chance(50) { default } else { None };
            let not_null = default.is_some() && rng.chance(50) || rng.chance(10);
            let name = format!("m{n}_col");
            let entity = &mut schema.entities[t];
            let at = rng.below(entity.columns.len() + 1);
            entity
                .columns
                .insert(at, column(&name, &ty, kind, default.as_deref(), not_null));
            Some(format!("add column {}.{name} {ty} at {at}", entity.name))
        }
        // Drop a column nothing else names.
        2 | 3 => {
            let t = *rng.pick(&tables)?;
            let entity = &schema.entities[t];
            let free: Vec<String> = entity
                .columns
                .iter()
                .filter(|c| {
                    let named = |sql: &str| mentions(sql, &c.name);
                    !entity.constraints.iter().any(|x| named(&x.definition))
                        && !entity.indexes.iter().any(|x| named(&x.definition))
                        && !entity.policies.iter().any(|p| {
                            named(p.using.as_deref().unwrap_or(""))
                                || named(p.check.as_deref().unwrap_or(""))
                        })
                        && !entity.partition_key.as_deref().is_some_and(named)
                        && !entity.columns.iter().any(|o| {
                            o.generated.is_some() && o.default.as_deref().is_some_and(named)
                        })
                })
                .map(|c| c.name.clone())
                .collect();
            let name = rng.pick(&free)?.clone();
            let reference = entity.reference();
            let views_use_it = schema.entities.iter().any(|v| {
                v.kind.is_view()
                    && v.depends_on.contains(&reference)
                    && v.view_definition
                        .as_deref()
                        .is_some_and(|d| mentions(d, &name))
            });
            if views_use_it {
                drop_views_over(schema, &reference);
            }
            let entity = schema
                .entities
                .iter_mut()
                .find(|e| e.reference() == reference)?;
            entity.columns.retain(|c| c.name != name);
            Some(format!("drop column {}.{name}", reference.name))
        }
        // Change a column's type.
        4 | 5 => {
            let t = *rng.pick(&tables)?;
            let entity = &schema.entities[t];
            let candidates: Vec<usize> = entity
                .columns
                .iter()
                .enumerate()
                .filter(|(_, c)| {
                    c.identity.is_none()
                        && c.generated.is_none()
                        && !entity
                            .partition_key
                            .as_deref()
                            .is_some_and(|k| mentions(k, &c.name))
                })
                .map(|(i, _)| i)
                .collect();
            let c = *rng.pick(&candidates)?;
            let from = entity.columns[c].r#type.clone();
            let options: &[&str] = match from.as_str() {
                "integer" => &["bigint", "text", "numeric(12,2)", "smallint"],
                "bigint" => &["integer", "text", "numeric"],
                "smallint" => &["integer", "bigint"],
                "text" => &["character varying(200)", "integer", "jsonb"],
                "boolean" => &["integer", "text"],
                "timestamp with time zone" => &["timestamp without time zone", "date", "text"],
                "timestamp without time zone" => &["timestamp with time zone"],
                "date" => &["timestamp with time zone", "text"],
                "uuid" => &["text", "bigint"],
                "jsonb" => &["json", "text"],
                "double precision" => &["numeric", "real"],
                _ if from.starts_with("numeric") => {
                    &["double precision", "numeric(20,4)", "integer"]
                }
                _ if from.starts_with("character varying") => {
                    &["text", "character varying(5)", "character varying(400)"]
                }
                _ => &["text"],
            };
            let to = (*rng.pick(options)?).to_owned();
            let entity = &mut schema.entities[t];
            let column = &mut entity.columns[c];
            column.r#type.clone_from(&to);
            column.type_kind = TypeKind::Base;
            if rng.chance(50) {
                column.default = None;
            }
            Some(format!(
                "type {}.{}: {from} → {to}",
                entity.name, column.name
            ))
        }
        // Nullability.
        6 => {
            let t = *rng.pick(&tables)?;
            let entity = &mut schema.entities[t];
            let c = rng.below(entity.columns.len());
            let column = entity.columns.get_mut(c)?;
            if column.identity.is_some() {
                return None;
            }
            column.not_null = !column.not_null;
            Some(format!(
                "not null {}.{} = {}",
                entity.name, column.name, column.not_null
            ))
        }
        // Default.
        7 => {
            let t = *rng.pick(&tables)?;
            let entity = &mut schema.entities[t];
            let c = rng.below(entity.columns.len());
            let column = entity.columns.get_mut(c)?;
            if column.identity.is_some() || column.generated.is_some() {
                return None;
            }
            column.default = match column.r#type.as_str() {
                _ if column.default.is_some() && rng.chance(50) => None,
                "text" => Some("'m'::text".into()),
                "integer" | "bigint" | "smallint" => Some("7".into()),
                "boolean" => Some("true".into()),
                "timestamp with time zone" => Some("now()".into()),
                _ => None,
            };
            Some(format!(
                "default {}.{} = {:?}",
                entity.name, column.name, column.default
            ))
        }
        // Add an index.
        8 | 9 => {
            let t = *rng.pick(&tables)?;
            let entity = &schema.entities[t];
            let column = rng.pick(&entity.columns)?.name.clone();
            let name = format!("{}_m{n}_idx", entity.name);
            let unique = rng.chance(25);
            let mut definition = format!(
                "CREATE {}INDEX {} ON {} USING btree ({})",
                if unique { "UNIQUE " } else { "" },
                quote_ident(&name),
                qualified(&entity.schema, &entity.name),
                quote_ident(&column)
            );
            let partial = rng.chance(30);
            if partial {
                definition = format!("{definition} WHERE ({} IS NOT NULL)", quote_ident(&column));
            }
            schema.entities[t].indexes.push(Index {
                name: name.clone(),
                unique,
                partial,
                definition,
            });
            Some(format!("add index {name}"))
        }
        // Drop or change an index.
        10 => {
            let t = *rng.pick(&tables)?;
            let entity = &mut schema.entities[t];
            let i = rng.below(entity.indexes.len());
            let index = entity.indexes.get_mut(i)?;
            if rng.chance(50) {
                let name = index.name.clone();
                entity.indexes.remove(i);
                Some(format!("drop index {name}"))
            } else if let Some((head, _)) = index.definition.split_once(" WHERE ") {
                index.definition = head.to_owned();
                index.partial = false;
                Some(format!("index {} no longer partial", index.name))
            } else {
                index.definition.push_str(" WHERE true");
                index.partial = true;
                Some(format!("index {} partial", index.name))
            }
        }
        // Add a check or a unique constraint.
        11 => {
            let t = *rng.pick(&tables)?;
            let entity = &schema.entities[t];
            let column = rng.pick(&entity.columns)?;
            let col = quote_ident(&column.name).into_owned();
            let (name, kind, definition) = if rng.chance(50) {
                let check = match column.r#type.as_str() {
                    "integer" | "bigint" | "smallint" | "numeric" => {
                        format!("CHECK (({col} >= 0))")
                    }
                    "text" => format!("CHECK ((length({col}) < 1000))"),
                    _ => format!("CHECK (({col} IS NOT NULL))"),
                };
                (
                    format!("{}_m{n}_check", entity.name),
                    ConstraintKind::Check,
                    check,
                )
            } else {
                (
                    format!("{}_m{n}_key", entity.name),
                    ConstraintKind::Unique,
                    format!("UNIQUE ({col})"),
                )
            };
            let mut c = constraint(name.clone(), kind, definition);
            c.columns = vec![column.name.clone()];
            schema.entities[t].constraints.push(c);
            Some(format!("add constraint {name}"))
        }
        // Drop a constraint.
        12 => {
            let t = *rng.pick(&tables)?;
            let entity = &mut schema.entities[t];
            let i = rng.below(entity.constraints.len());
            let name = entity.constraints.get(i)?.name.clone();
            entity.constraints.remove(i);
            Some(format!("drop constraint {name}"))
        }
        // A foreign key to another table's single-column primary key.
        13 => {
            let from = *rng.pick(&tables)?;
            let to = *rng.pick(&tables)?;
            let target = &schema.entities[to];
            let pk = target.primary_key()?;
            let [key] = pk.columns.as_slice() else {
                return None;
            };
            let key = key.clone();
            let key_type = target.column(&key)?.r#type.clone();
            let target_name = qualified(&target.schema, &target.name);
            let reference = target.reference();
            let column_name = format!("m{n}_ref");
            let entity = &mut schema.entities[from];
            if entity.kind == EntityKind::PartitionedTable {
                return None;
            }
            entity
                .columns
                .push(column(&column_name, &key_type, TypeKind::Base, None, false));
            let mut fk = constraint(
                format!("{}_m{n}_fkey", entity.name),
                ConstraintKind::ForeignKey,
                format!(
                    "FOREIGN KEY ({}) REFERENCES {target_name}({})",
                    quote_ident(&column_name),
                    quote_ident(&key)
                ),
            );
            fk.ref_schema = Some(reference.schema);
            fk.ref_table = Some(reference.name);
            entity.constraints.push(fk);
            Some(format!("add foreign key {}.{column_name}", entity.name))
        }
        // Enum labels: add, remove, swap, rename.
        14 | 15 => {
            let e = rng.below(schema.enums.len());
            let mood: &mut EnumType = schema.enums.get_mut(e)?;
            let what = rng.below(4);
            let len = mood.labels.len();
            match what {
                0 => {
                    let at = rng.below(len + 1);
                    mood.labels.insert(at, format!("m{n}"));
                }
                1 if len > 1 => {
                    mood.labels.remove(rng.below(len));
                }
                2 if len > 1 => {
                    let (a, b) = (rng.below(len), rng.below(len));
                    mood.labels.swap(a, b);
                }
                _ if len > 0 => {
                    let at = rng.below(len);
                    mood.labels[at] = format!("m{n}_renamed");
                }
                _ => return None,
            }
            Some(format!("enum {} labels {:?}", mood.name, mood.labels))
        }
        // Comments.
        16 => {
            let i = rng.below(schema.entities.len());
            let entity = schema.entities.get_mut(i)?;
            if entity.kind == EntityKind::ForeignTable {
                return None;
            }
            if rng.chance(50) {
                entity.comment = if entity.comment.is_some() && rng.chance(50) {
                    None
                } else {
                    Some(format!("Note {n}, it's here"))
                };
            } else {
                let c = rng.below(entity.columns.len());
                let column = entity.columns.get_mut(c)?;
                column.comment = if column.comment.is_some() {
                    None
                } else {
                    Some(format!("Column note {n}"))
                };
            }
            Some(format!("comment on {}", entity.name))
        }
        // Row level security and policies.
        17 => {
            let t = *rng.pick(&tables)?;
            let entity = &mut schema.entities[t];
            match rng.below(4) {
                0 => entity.rls = !entity.rls,
                1 => entity.rls_forced = !entity.rls_forced,
                2 => entity.policies.push(Policy {
                    name: format!("m{n}_policy"),
                    command: if rng.chance(50) {
                        PolicyCommand::Select
                    } else {
                        PolicyCommand::All
                    },
                    permissive: rng.chance(70),
                    roles: if rng.chance(50) {
                        vec!["app_user".into()]
                    } else {
                        Vec::new()
                    },
                    using: Some("true".into()),
                    check: None,
                }),
                _ => {
                    let p = rng.below(entity.policies.len());
                    let policy = entity.policies.get_mut(p)?;
                    if policy.roles.is_empty() {
                        policy.roles.push("app_reader".into());
                    } else {
                        policy.roles.clear();
                    }
                }
            }
            Some(format!("security on {}", entity.name))
        }
        // Views: add one, drop one, change one.
        18 => {
            let t = *rng.pick(&tables)?;
            let target = schema.entities[t].reference();
            let name = format!("m{n}_view");
            let materialized = rng.chance(30);
            schema.entities.push(Entity {
                schema: target.schema.clone(),
                name: name.clone(),
                kind: if materialized {
                    EntityKind::MaterializedView
                } else {
                    EntityKind::View
                },
                comment: None,
                rls: false,
                rls_forced: false,
                unlogged: false,
                partition_key: None,
                view_definition: Some(format!(
                    " SELECT count(*) AS n\n   FROM {};",
                    qualified(&target.schema, &target.name)
                )),
                depends_on: vec![target],
                columns: Vec::new(),
                constraints: Vec::new(),
                indexes: Vec::new(),
                policies: Vec::new(),
            });
            Some(format!("add view {name}"))
        }
        19 => {
            let views: Vec<usize> = schema
                .entities
                .iter()
                .enumerate()
                .filter(|(_, e)| e.kind.is_view())
                .map(|(i, _)| i)
                .collect();
            let v = *rng.pick(&views)?;
            let reference = schema.entities[v].reference();
            if rng.chance(50) {
                schema.entities.remove(v);
                drop_views_over(schema, &reference);
                Some(format!("drop view {}", reference.name))
            } else {
                let view = &mut schema.entities[v];
                let body = view
                    .view_definition
                    .as_deref()?
                    .trim()
                    .trim_end_matches(';')
                    .to_owned();
                view.view_definition = Some(format!(" SELECT *\n   FROM ({body}) m{n};"));
                Some(format!("change view {}", reference.name))
            }
        }
        // Partitions: drop one, or widen a list bound.
        20 => {
            let p = rng.below(schema.partitions.len());
            let partition = schema.partitions.get(p)?.clone();
            if rng.chance(50) {
                // With everything under it.
                let mut gone = vec![(partition.child_schema.clone(), partition.child.clone())];
                loop {
                    let before = gone.len();
                    for q in &schema.partitions {
                        let parent = (q.parent_schema.clone(), q.parent.clone());
                        let child = (q.child_schema.clone(), q.child.clone());
                        if gone.contains(&parent) && !gone.contains(&child) {
                            gone.push(child);
                        }
                    }
                    if gone.len() == before {
                        break;
                    }
                }
                schema
                    .partitions
                    .retain(|q| !gone.contains(&(q.child_schema.clone(), q.child.clone())));
                Some(format!("drop partition {}", partition.child))
            } else {
                let bound = partition.bound.as_deref()?;
                let list = bound.strip_prefix("FOR VALUES IN (")?.strip_suffix(')')?;
                if !list.starts_with('\'') {
                    return None;
                }
                schema.partitions[p].bound = Some(format!("FOR VALUES IN ({list}, 'm{n}')"));
                Some(format!("widen partition {}", partition.child))
            }
        }
        // Unlogged.
        21 => {
            let t = *rng.pick(&tables)?;
            let entity = &mut schema.entities[t];
            if entity.kind != EntityKind::Table {
                return None;
            }
            entity.unlogged = !entity.unlogged;
            Some(format!("unlogged {} = {}", entity.name, entity.unlogged))
        }
        // Drop a table, with what depends on it.
        22 => {
            let t = *rng.pick(&tables)?;
            let reference = schema.entities[t].reference();
            schema.entities.remove(t);
            drop_views_over(schema, &reference);
            for entity in &mut schema.entities {
                entity
                    .constraints
                    .retain(|c| c.target().as_ref() != Some(&reference));
                entity.policies.retain(|p| {
                    !mentions(p.using.as_deref().unwrap_or(""), &reference.name)
                        && !mentions(p.check.as_deref().unwrap_or(""), &reference.name)
                });
            }
            schema
                .partitions
                .retain(|p| !(p.parent_schema == reference.schema && p.parent == reference.name));
            Some(format!("drop table {}", reference.name))
        }
        // Rename a column or a table nothing else names.
        23 => {
            let t = *rng.pick(&tables)?;
            let entity = &schema.entities[t];
            let reference = entity.reference();
            let referenced = schema.entities.iter().any(|e| {
                e.depends_on.contains(&reference)
                    || e.constraints
                        .iter()
                        .any(|c| c.target().as_ref() == Some(&reference))
                    || e.policies
                        .iter()
                        .any(|p| mentions(p.using.as_deref().unwrap_or(""), &reference.name))
            }) || schema
                .partitions
                .iter()
                .any(|p| p.parent == reference.name || p.child == reference.name);
            if rng.chance(50) && !referenced && entity.indexes.is_empty() {
                let name = format!("{}_m{n}", entity.name);
                schema.entities[t].name.clone_from(&name);
                for c in &mut schema.entities[t].constraints {
                    c.definition = c.definition.replace(&reference.name, &name);
                }
                Some(format!("rename table {} → {name}", reference.name))
            } else {
                let free: Vec<String> = entity
                    .columns
                    .iter()
                    .filter(|c| {
                        let named = |sql: &str| mentions(sql, &c.name);
                        !entity.constraints.iter().any(|x| named(&x.definition))
                            && !entity.indexes.iter().any(|x| named(&x.definition))
                            && !entity
                                .policies
                                .iter()
                                .any(|p| named(p.using.as_deref().unwrap_or("")))
                            && !entity.partition_key.as_deref().is_some_and(named)
                            && !entity
                                .columns
                                .iter()
                                .any(|o| o.default.as_deref().is_some_and(named))
                            && !schema
                                .entities
                                .iter()
                                .any(|v| v.view_definition.as_deref().is_some_and(named))
                    })
                    .map(|c| c.name.clone())
                    .collect();
                let old = rng.pick(&free)?.clone();
                let column = schema.entities[t]
                    .columns
                    .iter_mut()
                    .find(|c| c.name == old)?;
                column.name = format!("{old}_m{n}");
                Some(format!("rename column {}.{old}", reference.name))
            }
        }
        // Domains.
        24 => {
            let d = rng.below(schema.domains.len());
            let domain = schema.domains.get_mut(d)?;
            match rng.below(3) {
                0 => domain.not_null = !domain.not_null,
                1 if !domain.constraints.is_empty() => {
                    domain.constraints.pop();
                }
                _ => domain
                    .constraints
                    .push("CHECK ((VALUE IS NOT NULL))".to_owned()),
            }
            Some(format!("domain {}", domain.name))
        }
        // A new table.
        _ => {
            let schema_name = schema
                .entities
                .first()
                .map_or_else(|| "public".to_owned(), |e| e.schema.clone());
            let name = format!("m{n}_table");
            let mut id = column("id", "bigint", TypeKind::Base, None, true);
            id.identity = Some(remus_core::model::Identity::Always);
            let mut pk = constraint(
                format!("{name}_pkey"),
                ConstraintKind::PrimaryKey,
                "PRIMARY KEY (id)".into(),
            );
            pk.columns = vec!["id".into()];
            schema.entities.push(Entity {
                schema: schema_name,
                name: name.clone(),
                kind: EntityKind::Table,
                comment: None,
                rls: false,
                rls_forced: false,
                unlogged: false,
                partition_key: None,
                view_definition: None,
                depends_on: Vec::new(),
                columns: vec![id, column("note", "text", TypeKind::Base, None, false)],
                constraints: vec![pk],
                indexes: Vec::new(),
                policies: Vec::new(),
            });
            Some(format!("add table {name}"))
        }
    }
}

fn seeds() -> u64 {
    std::env::var("ROMULUS_ORACLE_SEEDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(12)
}

#[test]
fn random_edits_of_the_corpus_migrate_both_ways() {
    with_compiler(|compiler| async move {
        let models = corpus::models(&compiler).await;
        let mut cases = Vec::new();
        let mut tried = 0;
        let mut invalid = 0;
        for (name, model) in models {
            for seed in 0..seeds() {
                let mut rng = Rng(seed.wrapping_mul(1_000_003)
                    ^ name.len() as u64
                    ^ u64::from(name.as_bytes()[0]) << 32);
                let mut mutated = model.clone();
                let mut edits = Vec::new();
                let count = 1 + rng.below(3);
                let mut attempts = 0;
                while edits.len() < count && attempts < 20 {
                    attempts += 1;
                    if let Some(edit) = mutate(&mut mutated, &mut rng, edits.len() + 1) {
                        edits.push(edit);
                    }
                }
                tried += 1;
                let Ok(new) = compile(&compiler, &render_sql(&mutated)).await else {
                    invalid += 1;
                    continue;
                };
                if romulus_diff::diff(model, &new).is_empty() {
                    continue;
                }
                let label = format!("{name} seed {seed}: {}", edits.join("; "));
                let case = Case::new(label, model, &new);
                if seed % 4 == 0 {
                    cases.push(case.concurrently());
                }
                cases.push(case.reversed());
                cases.push(case);
            }
        }
        #[allow(clippy::print_stderr)]
        {
            eprintln!("{tried} mutated schemas, {invalid} rejected by the sandbox");
        }
        assert!(
            invalid * 2 < tried,
            "{invalid} of {tried} mutated schemas do not compile: the generator needs fixing"
        );
        check_all(&compiler, cases).await;
    });
}
