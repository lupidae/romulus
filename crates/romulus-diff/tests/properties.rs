//! Properties of `diff` over random schemas.
//!
//! Both sides are drawn from the same small vocabulary of names and values,
//! so random pairs share most objects and every kind of change shows up.

use std::collections::BTreeSet;

use proptest::{
    collection::btree_map,
    option,
    prelude::*,
    sample::{Index as Pick, subsequence},
};
use remus_core::{
    Schema,
    model::{
        Column, CompositeAttribute, CompositeType, Constraint, ConstraintKind, DomainType, Entity,
        EntityKind, EnumType, Extension, Generated, Identity, Index, Partition, Policy,
        PolicyCommand, ReferentialAction, RelationRef, TypeKind,
    },
};
use romulus_diff::{Status, diff, normalize};
use serde_json::Value;

const SCHEMAS: [&str; 2] = ["public", "app"];
const TABLES: [&str; 3] = ["users", "orders", "items"];

fn one(choices: &'static [&'static str]) -> impl Strategy<Value = String> {
    prop::sample::select(choices).prop_map(str::to_owned)
}

fn maybe(choices: &'static [&'static str]) -> impl Strategy<Value = Option<String>> {
    option::of(one(choices))
}

/// Some of `names`, each once, in any order.
fn shuffled(names: &'static [&'static str]) -> impl Strategy<Value = Vec<String>> {
    subsequence(names, 0..=names.len())
        .prop_shuffle()
        .prop_map(|v| v.into_iter().map(str::to_owned).collect())
}

fn relation() -> impl Strategy<Value = RelationRef> {
    (one(&SCHEMAS), one(&TABLES)).prop_map(|(schema, name)| RelationRef { schema, name })
}

fn column(name: String) -> impl Strategy<Value = Column> {
    (
        (
            one(&["text", "bigint"]),
            one(&["pg_catalog", "public"]),
            one(&["text", "int8"]),
        ),
        prop::sample::select(vec![TypeKind::Base, TypeKind::Domain, TypeKind::Enum]),
        any::<bool>(),
        any::<bool>(),
        maybe(&["now()", "0"]),
        option::of(prop::sample::select(vec![
            Identity::Always,
            Identity::ByDefault,
        ])),
        option::of(prop::sample::select(vec![
            Generated::Stored,
            Generated::Virtual,
        ])),
        maybe(&["note", "other note"]),
    )
        .prop_map(
            move |(
                (r#type, type_schema, type_name),
                type_kind,
                is_array,
                not_null,
                default,
                identity,
                generated,
                comment,
            )| {
                Column {
                    name: name.clone(),
                    r#type,
                    type_schema,
                    type_name,
                    type_kind,
                    is_array,
                    not_null,
                    default,
                    identity,
                    generated,
                    comment,
                }
            },
        )
}

fn constraint(name: String) -> impl Strategy<Value = Constraint> {
    let action = || {
        option::of(prop::sample::select(vec![
            ReferentialAction::NoAction,
            ReferentialAction::Cascade,
        ]))
    };
    (
        prop::sample::select(vec![
            ConstraintKind::PrimaryKey,
            ConstraintKind::Unique,
            ConstraintKind::ForeignKey,
            ConstraintKind::Check,
            ConstraintKind::Exclusion,
        ]),
        subsequence(vec!["id", "a"], 0..=2),
        (
            maybe(&["public"]),
            maybe(&["users"]),
            subsequence(vec!["id"], 0..=1),
        ),
        (action(), action(), any::<bool>()),
        one(&["PRIMARY KEY (id)", "CHECK ((a > 0))"]),
        maybe(&["why"]),
    )
        .prop_map(
            move |(
                kind,
                columns,
                (ref_schema, ref_table, ref_columns),
                (on_update, on_delete, deferrable),
                definition,
                comment,
            )| {
                Constraint {
                    name: name.clone(),
                    kind,
                    columns: columns.into_iter().map(str::to_owned).collect(),
                    ref_schema,
                    ref_table,
                    ref_columns: ref_columns.into_iter().map(str::to_owned).collect(),
                    on_update,
                    on_delete,
                    deferrable,
                    definition,
                    comment,
                }
            },
        )
}

fn index(name: String) -> impl Strategy<Value = Index> {
    (
        any::<bool>(),
        any::<bool>(),
        one(&["CREATE INDEX i ON t (a)", "CREATE INDEX i ON t (b)"]),
    )
        .prop_map(move |(unique, partial, definition)| Index {
            name: name.clone(),
            unique,
            partial,
            definition,
        })
}

fn policy(name: String) -> impl Strategy<Value = Policy> {
    (
        prop::sample::select(vec![
            PolicyCommand::All,
            PolicyCommand::Select,
            PolicyCommand::Delete,
        ]),
        any::<bool>(),
        shuffled(&["reader", "writer"]),
        maybe(&["true", "(a = 1)"]),
        maybe(&["true"]),
    )
        .prop_map(move |(command, permissive, roles, using, check)| Policy {
            name: name.clone(),
            command,
            permissive,
            roles,
            using,
            check,
        })
}

fn entity((schema, name): (String, String)) -> impl Strategy<Value = Entity> {
    let columns = shuffled(&["id", "a", "b", "c", "d"])
        .prop_flat_map(|names| names.into_iter().map(column).collect::<Vec<_>>());
    let keyed = |names: &'static [&'static str]| {
        subsequence(names, 0..=names.len())
            .prop_map(|v| v.into_iter().map(str::to_owned).collect::<Vec<_>>())
    };
    let constraints = keyed(&["t_pkey", "t_fkey", "t_check"])
        .prop_flat_map(|names| names.into_iter().map(constraint).collect::<Vec<_>>())
        .prop_shuffle();
    let indexes = keyed(&["t_a_idx", "t_b_idx"])
        .prop_flat_map(|names| names.into_iter().map(index).collect::<Vec<_>>());
    let policies = keyed(&["owner", "reader"])
        .prop_flat_map(|names| names.into_iter().map(policy).collect::<Vec<_>>());
    (
        prop::sample::select(vec![
            EntityKind::Table,
            EntityKind::PartitionedTable,
            EntityKind::View,
            EntityKind::MaterializedView,
            EntityKind::ForeignTable,
        ]),
        maybe(&["about", "more about"]),
        (any::<bool>(), any::<bool>(), any::<bool>()),
        (
            maybe(&["RANGE (a)", "LIST (b)"]),
            maybe(&[" SELECT 1;", " SELECT 2;"]),
        ),
        prop::collection::btree_set(relation(), 0..3),
        (columns, constraints, indexes, policies),
    )
        .prop_map(
            move |(
                kind,
                comment,
                (rls, rls_forced, unlogged),
                (partition_key, view_definition),
                depends_on,
                (columns, constraints, indexes, policies),
            )| {
                Entity {
                    schema: schema.clone(),
                    name: name.clone(),
                    kind,
                    comment,
                    rls,
                    rls_forced,
                    unlogged,
                    partition_key,
                    view_definition,
                    // A set, shuffled: the catalog's order means nothing.
                    depends_on: depends_on.into_iter().rev().collect(),
                    columns,
                    constraints,
                    indexes,
                    policies,
                }
            },
        )
}

fn schema() -> impl Strategy<Value = Schema> {
    let entities = btree_map((one(&SCHEMAS), one(&TABLES)), Just(()), 0..4)
        .prop_flat_map(|keys| keys.into_keys().map(entity).collect::<Vec<_>>());
    let enums = btree_map(
        one(&["mood", "state"]),
        (shuffled(&["a", "b", "c", "d"]), maybe(&["x"])),
        0..3,
    )
    .prop_map(|m| {
        m.into_iter()
            .map(|(name, (labels, comment))| EnumType {
                schema: "public".into(),
                name,
                labels,
                comment,
            })
            .collect::<Vec<_>>()
    });
    let domains = btree_map(
        one(&["email", "money"]),
        (
            one(&["text", "bigint"]),
            any::<bool>(),
            maybe(&["0"]),
            prop::collection::vec(one(&["CHECK ((VALUE > 0))", "CHECK ((VALUE < 9))"]), 0..3),
            maybe(&["x"]),
        ),
        0..3,
    )
    .prop_map(|m| {
        m.into_iter()
            .map(
                |(name, (base_type, not_null, default, constraints, comment))| DomainType {
                    schema: "app".into(),
                    name,
                    base_type,
                    not_null,
                    default,
                    constraints,
                    comment,
                },
            )
            .collect::<Vec<_>>()
    });
    let attributes = shuffled(&["street", "city", "zip"]).prop_flat_map(|names| {
        names
            .into_iter()
            .map(|name| {
                one(&["text", "integer"]).prop_map(move |r#type| CompositeAttribute {
                    name: name.clone(),
                    r#type,
                })
            })
            .collect::<Vec<_>>()
    });
    let composites = btree_map(
        one(&["address", "point"]),
        (attributes, maybe(&["x"])),
        0..3,
    )
    .prop_map(|m| {
        m.into_iter()
            .map(|(name, (attributes, comment))| CompositeType {
                schema: "public".into(),
                name,
                attributes,
                comment,
            })
            .collect::<Vec<_>>()
    });
    let partitions = btree_map(
        (relation(), one(&["p1", "p2"])),
        maybe(&["DEFAULT", "FOR VALUES IN (1)"]),
        0..4,
    )
    .prop_map(|m| {
        m.into_iter()
            .map(|((parent, child), bound)| Partition {
                parent_schema: parent.schema,
                parent: parent.name,
                child_schema: "app".into(),
                child,
                bound,
            })
            .collect::<Vec<_>>()
    })
    .prop_shuffle();
    let extensions = btree_map(
        one(&["pgcrypto", "citext"]),
        (one(&["public", "ext"]), one(&["1.0", "1.1"])),
        0..3,
    )
    .prop_map(|m| {
        m.into_iter()
            .map(|(name, (schema, version))| Extension {
                name,
                schema,
                version,
            })
            .collect::<Vec<_>>()
    });
    (entities, enums, domains, composites, partitions, extensions).prop_map(
        |(entities, enums, domains, composites, partitions, extensions)| Schema {
            remus_version: "0.1".into(),
            generated_at: None,
            database: "db".into(),
            server_version: "16".into(),
            entities,
            enums,
            domains,
            composites,
            partitions,
            extensions,
        },
    )
}

/// JSON pointers to every value of the model that one edit can change: every
/// scalar and every non-empty list.
fn pointers(value: &Value, at: &str, out: &mut Vec<String>) {
    match value {
        Value::Object(fields) => {
            for (key, child) in fields {
                pointers(child, &format!("{at}/{key}"), out);
            }
        }
        Value::Array(items) => {
            if !items.is_empty() {
                out.push(at.to_owned());
            }
            for (i, child) in items.iter().enumerate() {
                pointers(child, &format!("{at}/{i}"), out);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {
            out.push(at.to_owned());
        }
    }
}

/// Catalog letters, so an edit to a coded field (kind, identity, actions…)
/// can still produce a valid model.
const CODES: [&str; 12] = ["r", "p", "v", "m", "f", "u", "c", "x", "a", "d", "s", "*"];

/// `schema` with one value edited through its JSON: a flag flipped, a text
/// changed, a null filled, a list item removed or two neighbours swapped.
/// Being generic over the JSON, it reaches every field of the model, including
/// any a future remus adds, which is the point: a field `diff` forgets to
/// compare makes the edit invisible.
fn mutate(schema: &Schema, pick: Pick, item: Pick, swap: bool) -> Option<Schema> {
    let json = serde_json::to_value(schema).ok()?;
    let mut all = Vec::new();
    pointers(&json, "", &mut all);
    if all.is_empty() {
        return None;
    }
    let at = pick.get(&all);
    let candidates: Vec<Value> = match json.pointer(at)? {
        Value::Bool(b) => vec![Value::Bool(!b)],
        Value::String(s) => std::iter::once(format!("{s}x"))
            .chain(CODES.map(str::to_owned))
            .filter(|c| c != s)
            .map(Value::String)
            .collect(),
        Value::Null => ["x", "a", "s"].map(|c| Value::String(c.into())).to_vec(),
        Value::Array(items) => {
            let mut items = items.clone();
            if swap && items.len() >= 2 {
                let i = item.index(items.len() - 1);
                items.swap(i, i + 1);
            } else {
                items.remove(item.index(items.len()));
            }
            vec![Value::Array(items)]
        }
        Value::Object(_) | Value::Number(_) => Vec::new(),
    };
    candidates.into_iter().find_map(|candidate| {
        let mut edited = json.clone();
        *edited.pointer_mut(at)? = candidate;
        serde_json::from_value(edited).ok()
    })
}

fn mutated() -> impl Strategy<Value = (Schema, Schema)> {
    (schema(), any::<Pick>(), any::<Pick>(), any::<bool>())
        .prop_filter_map("the edit yields a valid model", |(a, pick, item, swap)| {
            mutate(&a, pick, item, swap).map(|b| (a, b))
        })
}

/// What `diff` promises to see: the normalised schema without its metadata.
fn comparable(schema: &Schema) -> Schema {
    let mut schema = normalize(schema);
    schema.remus_version.clear();
    schema.database.clear();
    schema.server_version.clear();
    schema
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(4096))]

    #[test]
    fn one_edit_shows_exactly_when_it_changes_the_schema((a, b) in mutated()) {
        let d = diff(&a, &b);
        prop_assert_eq!(d.is_empty(), comparable(&a) == comparable(&b), "{:#?}", d);
        prop_assert_eq!(diff(&b, &a), d.reversed());
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn a_schema_has_no_diff_with_itself(a in schema()) {
        let d = diff(&a, &a);
        prop_assert!(d.is_empty());
        prop_assert_eq!(d.summary.text.as_str(), "no changes");
        prop_assert!(d.status.entities.values().all(|e| e.status == Status::Unchanged));
    }

    #[test]
    fn the_diff_back_is_the_mirror_image(a in schema(), b in schema()) {
        let forward = diff(&a, &b);
        prop_assert_eq!(diff(&b, &a), forward.clone().reversed());
        prop_assert_eq!(forward.clone().reversed().reversed(), forward);
    }

    #[test]
    fn empty_exactly_when_equal_after_normalising(a in schema(), b in schema()) {
        prop_assert_eq!(diff(&a, &b).is_empty(), comparable(&a) == comparable(&b));
    }

    #[test]
    fn shuffling_unordered_lists_changes_nothing(a in schema()) {
        let mut b = a.clone();
        b.entities.reverse();
        b.partitions.reverse();
        for e in &mut b.entities {
            e.constraints.reverse();
            e.policies.iter_mut().for_each(|p| p.roles.reverse());
        }
        prop_assert!(diff(&a, &b).is_empty());
    }

    #[test]
    fn the_status_map_covers_both_sides(a in schema(), b in schema()) {
        let d = diff(&a, &b);
        let names: BTreeSet<String> = a.entities.iter().chain(&b.entities).map(Entity::qualified_name).collect();
        prop_assert_eq!(d.status.entities.keys().cloned().collect::<BTreeSet<_>>(), names);
        for (name, status) in &d.status.entities {
            let columns: BTreeSet<&str> = a.entities.iter().chain(&b.entities)
                .filter(|e| &e.qualified_name() == name)
                .flat_map(|e| e.columns.iter().map(|c| c.name.as_str()))
                .collect();
            prop_assert_eq!(status.columns.keys().map(String::as_str).collect::<BTreeSet<_>>(), columns);
        }
        let listed = d.entities.iter().map(|e| (e.qualified_name.clone(), e.status));
        for (name, status) in listed {
            prop_assert_eq!(d.status.entities[&name].status, status);
        }
    }

    #[test]
    fn every_listed_object_says_something(a in schema(), b in schema()) {
        let d = diff(&a, &b);
        prop_assert!(d.entities.iter().all(|e| !e.lines.is_empty() && e.status != Status::Unchanged));
        prop_assert!(d.enums.iter().all(|e| !e.lines.is_empty()));
        prop_assert!(d.domains.iter().all(|x| !x.lines.is_empty()));
        prop_assert!(d.composites.iter().all(|c| !c.lines.is_empty()));
        prop_assert!(d.extensions.iter().all(|x| !x.lines.is_empty()));
        prop_assert_eq!(d.summary.text == "no changes", d.is_empty());
    }
}
