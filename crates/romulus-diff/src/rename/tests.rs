//! Rename detection on small hand-built models: the scores and their
//! thresholds, ambiguity, decisions, known renames (chains, swaps, ones
//! that do not apply), and the old model rewritten as Postgres prints it.

use std::collections::BTreeMap;

use remus_core::{
    Schema,
    model::{
        Column, Constraint, ConstraintKind, Entity, EntityKind, EnumType, Index, Policy,
        PolicyCommand, RelationRef, TypeKind,
    },
};

use super::{Confidence, Rename, RenameChoices, RenameKind, Resolved, detect::similarity, resolve};

fn schema(entities: Vec<Entity>) -> Schema {
    let mut s =
        Schema::from_json(r#"{"remus_version": "0.2", "database": "db", "server_version": "17"}"#)
            .unwrap();
    s.entities = entities;
    s
}

fn table(name: &str, columns: Vec<Column>) -> Entity {
    Entity {
        schema: "public".into(),
        name: name.into(),
        kind: EntityKind::Table,
        comment: None,
        rls: false,
        rls_forced: false,
        unlogged: false,
        partition_key: None,
        view_definition: None,
        depends_on: Vec::new(),
        columns,
        constraints: Vec::new(),
        indexes: Vec::new(),
        policies: Vec::new(),
    }
}

fn col(name: &str, ty: &str) -> Column {
    Column {
        name: name.into(),
        r#type: ty.into(),
        type_schema: "pg_catalog".into(),
        type_name: ty.into(),
        type_kind: TypeKind::Base,
        is_array: false,
        not_null: false,
        default: None,
        identity: None,
        generated: None,
        comment: None,
    }
}

fn not_null(mut c: Column) -> Column {
    c.not_null = true;
    c
}

fn serial(name: &str, sequence: &str) -> Column {
    let mut c = not_null(col(name, "integer"));
    c.default = Some(format!("nextval('public.{sequence}'::regclass)"));
    c
}

fn key(name: &str, kind: ConstraintKind, columns: &[&str], definition: &str) -> Constraint {
    Constraint {
        name: name.into(),
        kind,
        columns: columns.iter().map(|c| (*c).to_owned()).collect(),
        ref_schema: None,
        ref_table: None,
        ref_columns: Vec::new(),
        on_update: None,
        on_delete: None,
        deferrable: false,
        definition: definition.into(),
        comment: None,
    }
}

fn fk(name: &str, column: &str, target: &str, target_column: &str) -> Constraint {
    Constraint {
        ref_schema: Some("public".into()),
        ref_table: Some(target.into()),
        ref_columns: vec![target_column.into()],
        ..key(
            name,
            ConstraintKind::ForeignKey,
            &[column],
            &format!("FOREIGN KEY ({column}) REFERENCES public.{target}({target_column})"),
        )
    }
}

fn index(name: &str, table: &str, columns: &str) -> Index {
    Index {
        name: name.into(),
        unique: false,
        partial: false,
        definition: format!("CREATE INDEX {name} ON public.{table} USING btree ({columns})"),
    }
}

fn view(name: &str, over: &str, definition: &str, columns: &[&str]) -> Entity {
    Entity {
        kind: EntityKind::View,
        view_definition: Some(definition.into()),
        depends_on: vec![RelationRef {
            schema: "public".into(),
            name: over.into(),
        }],
        ..table(name, columns.iter().map(|c| col(c, "text")).collect())
    }
}

fn detected(old: &Schema, new: &Schema) -> Resolved {
    resolve(old, new, &RenameChoices::default())
}

fn deciding(old: &Schema, new: &Schema, decisions: &[(&str, bool)]) -> Resolved {
    let decisions: BTreeMap<String, bool> = decisions
        .iter()
        .map(|(k, v)| ((*k).to_owned(), *v))
        .collect();
    resolve(
        old,
        new,
        &RenameChoices {
            decisions,
            ..RenameChoices::default()
        },
    )
}

fn summary(r: &Resolved) -> Vec<String> {
    r.candidates
        .iter()
        .map(|c| {
            format!(
                "{} {} {:?} {}{}",
                c.rename.kind.word(),
                c.rename.describe(),
                c.confidence,
                if c.applied { "on" } else { "off" },
                c.follows
                    .as_ref()
                    .map(|f| format!(" (with {f})"))
                    .unwrap_or_default()
            )
        })
        .collect()
}

#[test]
fn names_compare_by_words_edits_and_abbreviations() {
    assert!((similarity("email", "email") - 1.0).abs() < f64::EPSILON);
    assert!(similarity("email", "email_address") > 0.6);
    assert!(similarity("userEmail", "user_email") > 0.99);
    assert!(similarity("desc", "description") >= 0.6);
    assert!(similarity("legacy", "nickname") < 0.4);
    assert!(similarity("first_name", "given_name") >= 0.5);
}

#[test]
fn a_column_renamed_in_place_is_a_high_rename_applied() {
    let old = schema(vec![table(
        "users",
        vec![
            col("id", "integer"),
            col("email", "text"),
            col("note", "text"),
        ],
    )]);
    let new = schema(vec![table(
        "users",
        vec![
            col("id", "integer"),
            col("email_address", "text"),
            col("note", "text"),
        ],
    )]);
    let r = detected(&old, &new);
    assert_eq!(
        summary(&r),
        ["column public.users.email → email_address High on"]
    );
    let c = &r.candidates[0];
    assert_eq!(c.key, "column/public/users/email/email_address");
    assert!(c.score >= 90, "{}", c.score);
    assert_eq!(
        c.reason,
        "Same type, nullability, default and keys; same place; similar name."
    );
    assert_eq!(r.schema.entities[0].columns[1].name, "email_address");
    assert!(crate::diff(&r.schema, &new).is_empty());
}

#[test]
fn a_column_alike_elsewhere_with_another_name_is_medium_and_off() {
    // name → full_name is high (similar); legacy → nickname is alike but
    // neither placed nor named alike.
    let old = schema(vec![table(
        "users",
        vec![
            col("id", "integer"),
            not_null(col("name", "text")),
            col("legacy", "text"),
            col("email", "text"),
        ],
    )]);
    let new = schema(vec![table(
        "users",
        vec![
            col("id", "integer"),
            col("nickname", "text"),
            not_null(col("full_name", "text")),
            col("email", "text"),
        ],
    )]);
    let r = detected(&old, &new);
    assert_eq!(
        summary(&r),
        [
            "column public.users.name → full_name High on",
            "column public.users.legacy → nickname Medium off",
        ]
    );
    let medium = &r.candidates[1];
    assert!((45..72).contains(&medium.score), "{}", medium.score);
    assert_eq!(medium.reason, "Same type, nullability, default and keys.");
    // Turned on, it is applied.
    let r = deciding(
        &old,
        &new,
        &[
            (&medium.key, true),
            ("column/public/users/name/full_name", false),
        ],
    );
    assert_eq!(
        summary(&r),
        [
            "column public.users.name → full_name High off",
            "column public.users.legacy → nickname Medium on",
        ]
    );
}

#[test]
fn a_column_that_changes_type_in_place_is_offered_but_off() {
    let old = schema(vec![table(
        "t",
        vec![
            col("id", "integer"),
            col("code", "character varying(10)"),
            col("z", "text"),
        ],
    )]);
    let new = schema(vec![table(
        "t",
        vec![col("id", "integer"), col("label", "text"), col("z", "text")],
    )]);
    let r = detected(&old, &new);
    assert_eq!(summary(&r), ["column public.t.code → label Medium off"]);
    assert_eq!(r.candidates[0].reason, "Compatible type; same place.");
    // Unrelated types are not offered at all.
    let new = schema(vec![table(
        "t",
        vec![
            col("id", "integer"),
            col("label", "boolean"),
            col("z", "text"),
        ],
    )]);
    assert!(detected(&old, &new).candidates.is_empty());
}

#[test]
fn two_equally_good_candidates_are_both_offered_as_rivals_and_neither_chosen() {
    // Neither in a's place, neither named like it.
    let old = schema(vec![table(
        "t",
        vec![col("a", "text"), col("id", "integer")],
    )]);
    let new = schema(vec![table(
        "t",
        vec![col("id", "integer"), col("b", "text"), col("c", "text")],
    )]);
    let r = detected(&old, &new);
    assert_eq!(
        summary(&r),
        [
            "column public.t.a → b Medium off",
            "column public.t.a → c Medium off"
        ]
    );
    assert_eq!(r.candidates[0].rivals, [r.candidates[1].key.clone()]);
    assert_eq!(r.candidates[1].rivals, [r.candidates[0].key.clone()]);
    assert!(r.renames.is_empty());
    // Turning both on applies the first only.
    let keys: Vec<String> = r.candidates.iter().map(|c| c.key.clone()).collect();
    let r = deciding(&old, &new, &[(&keys[0], true), (&keys[1], true)]);
    assert_eq!(
        summary(&r),
        [
            "column public.t.a → b Medium on",
            "column public.t.a → c Medium off"
        ]
    );
}

#[test]
fn a_clear_winner_is_not_a_tie() {
    // a → a_x (similar name, same place) beats a → zzz by far.
    let old = schema(vec![table(
        "t",
        vec![col("id", "integer"), col("a_name", "text")],
    )]);
    let new = schema(vec![table(
        "t",
        vec![
            col("id", "integer"),
            col("a_label", "text"),
            col("zzz", "text"),
        ],
    )]);
    let r = detected(&old, &new);
    assert_eq!(summary(&r), ["column public.t.a_name → a_label High on"]);
}

#[test]
fn a_renamed_column_takes_its_keys_and_indexes_along() {
    let mut users = table(
        "users",
        vec![not_null(col("id", "integer")), col("email", "text")],
    );
    users.constraints = vec![
        key(
            "users_pkey",
            ConstraintKind::PrimaryKey,
            &["id"],
            "PRIMARY KEY (id)",
        ),
        key(
            "users_email_key",
            ConstraintKind::Unique,
            &["email"],
            "UNIQUE (email)",
        ),
        key(
            "users_email_check",
            ConstraintKind::Check,
            &[],
            "CHECK ((email <> ''::text))",
        ),
    ];
    users.indexes = vec![index("users_email_lower", "users", "lower(email)")];
    let mut renamed = users.clone();
    renamed.columns[1].name = "email_address".into();
    renamed.constraints[1] = key(
        "users_email_address_key",
        ConstraintKind::Unique,
        &["email_address"],
        "UNIQUE (email_address)",
    );
    renamed.constraints[2] = key(
        "users_email_address_check",
        ConstraintKind::Check,
        &[],
        "CHECK ((email_address <> ''::text))",
    );
    renamed.indexes = vec![index("users_email_lower", "users", "lower(email_address)")];
    let r = detected(&schema(vec![users]), &schema(vec![renamed.clone()]));
    let column = "column/public/users/email/email_address";
    assert_eq!(
        summary(&r),
        [
            "column public.users.email → email_address High on".to_owned(),
            format!(
                "constraint public.users.users_email_key → users_email_address_key High on (with {column})"
            ),
            format!(
                "constraint public.users.users_email_check → users_email_address_check High on (with {column})"
            ),
        ]
    );
    assert!(crate::diff(&r.schema, &schema(vec![renamed])).is_empty());
}

#[test]
fn a_renamed_table_takes_its_key_sequence_and_indexes_along_and_foreign_keys_follow() {
    let mut users = table(
        "users",
        vec![serial("id", "users_id_seq"), col("email", "text")],
    );
    users.constraints = vec![key(
        "users_pkey",
        ConstraintKind::PrimaryKey,
        &["id"],
        "PRIMARY KEY (id)",
    )];
    users.indexes = vec![index("users_email_idx", "users", "email")];
    let mut orders = table(
        "orders",
        vec![col("id", "integer"), col("user_id", "integer")],
    );
    orders.constraints = vec![fk("orders_user_id_fkey", "user_id", "users", "id")];

    let mut accounts = users.clone();
    accounts.name = "accounts".into();
    accounts.columns[0] = serial("id", "accounts_id_seq");
    accounts.constraints = vec![key(
        "accounts_pkey",
        ConstraintKind::PrimaryKey,
        &["id"],
        "PRIMARY KEY (id)",
    )];
    accounts.indexes = vec![index("accounts_email_idx", "accounts", "email")];
    let mut orders_after = orders.clone();
    orders_after.constraints = vec![fk("orders_user_id_fkey", "user_id", "accounts", "id")];

    let old = schema(vec![orders, users]);
    let new = schema(vec![accounts, orders_after]);
    let r = detected(&old, &new);
    let table = "table/public//users/accounts";
    assert_eq!(
        summary(&r),
        [
            "table public.users → accounts High on".to_owned(),
            format!(
                "sequence public.accounts.users_id_seq → accounts_id_seq High on (with {table})"
            ),
            format!("constraint public.accounts.users_pkey → accounts_pkey High on (with {table})"),
            format!(
                "index public.accounts.users_email_idx → accounts_email_idx High on (with {table})"
            ),
        ]
    );
    assert_eq!(
        r.candidates[0].reason,
        "Same columns, keys and indexes; its foreign keys follow it."
    );
    assert!(
        crate::diff(&r.schema, &new).is_empty(),
        "{:?}",
        crate::diff(&r.schema, &new).lines().collect::<Vec<_>>()
    );
}

#[test]
fn a_table_that_also_gains_a_column_is_medium() {
    let old = schema(vec![table(
        "people",
        vec![
            col("id", "integer"),
            col("a", "text"),
            col("b", "text"),
            col("c", "text"),
        ],
    )]);
    let new = schema(vec![table(
        "persons",
        vec![
            col("id", "integer"),
            col("a", "text"),
            col("b", "text"),
            col("c", "text"),
            col("d", "text"),
        ],
    )]);
    let r = detected(&old, &new);
    assert_eq!(summary(&r), ["table public.people → persons Medium off"]);
    assert_eq!(r.candidates[0].reason, "4 of 5 columns alike.");
    // A table with little in common is not offered.
    let new = schema(vec![table(
        "persons",
        vec![col("x", "integer"), col("y", "text")],
    )]);
    assert!(detected(&old, &new).candidates.is_empty());
}

fn enumeration(name: &str, labels: &[&str]) -> EnumType {
    EnumType {
        schema: "public".into(),
        name: name.into(),
        labels: labels.iter().map(|l| (*l).to_owned()).collect(),
        comment: None,
    }
}

fn typed(name: &str, ty: &str) -> Column {
    Column {
        type_schema: "public".into(),
        type_name: ty.into(),
        type_kind: TypeKind::Enum,
        ..col(name, ty)
    }
}

#[test]
fn an_enum_label_renamed_in_place_is_a_rename_value() {
    let mut old = schema(vec![]);
    old.enums = vec![enumeration("size", &["small", "medium", "large"])];
    let mut t = table("shirts", vec![typed("size", "size")]);
    t.constraints = vec![key(
        "not_medium",
        ConstraintKind::Check,
        &[],
        "CHECK ((size <> 'medium'::size))",
    )];
    old.entities = vec![t.clone()];
    let mut new = old.clone();
    new.enums = vec![enumeration("size", &["small", "regular", "large"])];
    new.entities[0].constraints[0].definition = "CHECK ((size <> 'regular'::size))".into();
    // No name evidence: offered, off; turned on, it renames the value.
    let r = detected(&old, &new);
    assert_eq!(
        summary(&r),
        ["enum_value public.size.medium → regular Medium off"]
    );
    let r = deciding(
        &old,
        &new,
        &[("enum_value/public/size/medium/regular", true)],
    );
    assert!(crate::diff(&r.schema, &new).is_empty());
    // Two out and two in between the same labels pair in order, unsure.
    let mut old = schema(vec![]);
    old.enums = vec![enumeration("e", &["a", "b", "c", "z"])];
    let mut new = old.clone();
    new.enums = vec![enumeration("e", &["a", "x", "y", "z"])];
    assert_eq!(
        summary(&detected(&old, &new)),
        [
            "enum_value public.e.b → x Medium off",
            "enum_value public.e.c → y Medium off"
        ]
    );
}

#[test]
fn an_enum_renamed_with_one_of_its_labels_is_still_one_type_renamed() {
    let mut old = schema(vec![]);
    old.enums = vec![enumeration("mood", &["happy", "ok", "sad"])];
    let mut t = table("people", vec![typed("mood", "mood")]);
    t.columns[0].default = Some("'ok'::mood".into());
    old.entities = vec![t];
    let mut new = schema(vec![]);
    new.enums = vec![enumeration("feeling", &["happy", "fine", "sad"])];
    let mut t = table("people", vec![typed("mood", "feeling")]);
    t.columns[0].default = Some("'fine'::feeling".into());
    new.entities = vec![t];
    let r = detected(&old, &new);
    assert_eq!(
        summary(&r),
        [
            "enum public.mood → feeling High on",
            // A label in another's place, nothing alike in the names:
            // offered, off (rows holding it would read another label).
            "enum_value public.feeling.ok → fine Medium off",
        ]
    );
}

#[test]
fn a_renamed_type_is_followed_by_its_columns_and_casts() {
    let mut old = schema(vec![]);
    old.enums = vec![enumeration("mood", &["happy", "sad"])];
    let mut t = table("people", vec![typed("mood", "mood")]);
    t.columns[0].default = Some("'happy'::mood".into());
    old.entities = vec![t];
    let mut new = schema(vec![]);
    new.enums = vec![enumeration("feeling", &["happy", "sad"])];
    let mut t = table("people", vec![typed("mood", "feeling")]);
    t.columns[0].default = Some("'happy'::feeling".into());
    new.entities = vec![t];
    let r = detected(&old, &new);
    assert_eq!(summary(&r), ["enum public.mood → feeling High on"]);
    assert_eq!(
        r.candidates[0].reason,
        "Same labels; its columns switch to it."
    );
    assert!(crate::diff(&r.schema, &new).is_empty());
}

#[test]
fn a_view_over_a_renamed_column_keeps_its_output_name() {
    let old = schema(vec![
        table("accounts", vec![col("id", "integer"), col("code", "text")]),
        view(
            "codes",
            "accounts",
            " SELECT id,\n    code\n   FROM public.accounts;",
            &["id", "code"],
        ),
    ]);
    // Renamed with ALTER TABLE … RENAME COLUMN: the view's column is still
    // `code`, and Postgres prints it so.
    let new = schema(vec![
        table("accounts", vec![col("id", "integer"), col("kode", "text")]),
        view(
            "codes",
            "accounts",
            " SELECT id,\n    kode AS code\n   FROM public.accounts;",
            &["id", "code"],
        ),
    ]);
    let r = detected(&old, &new);
    assert_eq!(summary(&r), ["column public.accounts.code → kode High on"]);
    assert!(
        crate::diff(&r.schema, &new).is_empty(),
        "{:?}",
        crate::diff(&r.schema, &new).lines().collect::<Vec<_>>()
    );
    assert!(r.rebuild_views.is_empty());
}

#[test]
fn a_view_the_rewrite_cannot_follow_is_built_again() {
    let old = schema(vec![
        table("accounts", vec![col("id", "integer"), col("code", "text")]),
        view(
            "codes",
            "accounts",
            " SELECT accounts.code\n   FROM public.accounts\nUNION\n SELECT accounts.code\n   FROM public.accounts;",
            &["code"],
        ),
    ]);
    let mut new = old.clone();
    new.entities[0].columns[1].name = "kode".into();
    let r = detected(&old, &new);
    assert!(
        r.rebuild_views
            .contains(&("public".to_owned(), "codes".to_owned()))
    );
}

#[test]
fn policies_and_other_tables_policies_follow_a_renamed_column() {
    let mut notes = table(
        "notes",
        vec![col("id", "integer"), col("event_id", "integer")],
    );
    notes.policies = vec![Policy {
        name: "with_event".into(),
        command: PolicyCommand::All,
        permissive: true,
        roles: Vec::new(),
        using: Some(
            "(EXISTS ( SELECT 1\n   FROM public.events e\n  WHERE (e.id = notes.event_id)))".into(),
        ),
        check: None,
    }];
    let events = table("events", vec![col("id", "integer"), col("at", "date")]);
    let old = schema(vec![events.clone(), notes]);
    let r = resolve(
        &old,
        &old,
        &RenameChoices::exactly(vec![Rename {
            kind: RenameKind::Column,
            schema: "public".into(),
            parent: Some("events".into()),
            from: "id".into(),
            to: "event_id".into(),
            to_schema: None,
        }]),
    );
    // The rename is not in the new model (it is `old`): ignored.
    assert!(r.renames.is_empty());
    let mut new = old.clone();
    new.entities[0].columns[0].name = "event_id".into();
    new.entities[1].policies[0].using = Some(
        "(EXISTS ( SELECT 1\n   FROM public.events e\n  WHERE (e.event_id = notes.event_id)))"
            .into(),
    );
    let r = resolve(
        &old,
        &new,
        &RenameChoices::exactly(vec![Rename {
            kind: RenameKind::Column,
            schema: "public".into(),
            parent: Some("events".into()),
            from: "id".into(),
            to: "event_id".into(),
            to_schema: None,
        }]),
    );
    assert_eq!(
        summary(&r),
        ["column public.events.id → event_id Certain on"]
    );
    assert!(crate::diff(&r.schema, &new).is_empty());
}

fn rename(kind: RenameKind, parent: Option<&str>, from: &str, to: &str) -> Rename {
    Rename {
        kind,
        schema: "public".into(),
        parent: parent.map(str::to_owned),
        from: from.into(),
        to: to.into(),
        to_schema: None,
    }
}

#[test]
fn known_renames_follow_chains_and_parents_renamed_first() {
    let old = schema(vec![table(
        "users",
        vec![col("id", "integer"), col("email", "text")],
    )]);
    let new = schema(vec![table(
        "people",
        vec![col("id", "integer"), col("contact", "text")],
    )]);
    let known = vec![
        rename(RenameKind::Table, None, "users", "accounts"),
        rename(RenameKind::Column, Some("accounts"), "email", "mail"),
        rename(RenameKind::Table, None, "accounts", "people"),
        rename(RenameKind::Column, Some("people"), "mail", "contact"),
    ];
    let r = resolve(
        &old,
        &new,
        &RenameChoices {
            known,
            ..RenameChoices::default()
        },
    );
    assert_eq!(
        summary(&r),
        [
            "table public.users → people Certain on",
            "column public.people.email → contact Certain on",
        ]
    );
    assert_eq!(r.steps, r.renames);
    assert!(crate::diff(&r.schema, &new).is_empty());
}

#[test]
fn known_swaps_go_through_a_temporary_name() {
    let old = schema(vec![table(
        "t",
        vec![col("a", "integer"), col("b", "text")],
    )]);
    let new = schema(vec![table(
        "t",
        vec![col("b", "integer"), col("a", "text")],
    )]);
    let known = vec![
        rename(RenameKind::Column, Some("t"), "a", "tmp"),
        rename(RenameKind::Column, Some("t"), "b", "a"),
        rename(RenameKind::Column, Some("t"), "tmp", "b"),
    ];
    let r = resolve(
        &old,
        &new,
        &RenameChoices {
            known,
            ..RenameChoices::default()
        },
    );
    assert_eq!(
        r.renames,
        [
            rename(RenameKind::Column, Some("t"), "a", "b"),
            rename(RenameKind::Column, Some("t"), "b", "a"),
        ]
    );
    assert_eq!(
        r.steps,
        [
            rename(RenameKind::Column, Some("t"), "a", "a_romulus_1"),
            rename(RenameKind::Column, Some("t"), "b", "a"),
            rename(RenameKind::Column, Some("t"), "a_romulus_1", "b"),
        ]
    );
    assert!(crate::diff(&r.schema, &new).is_empty());
}

#[test]
fn known_renames_of_what_the_old_model_lacks_or_the_new_one_drops_are_ignored() {
    let old = schema(vec![table("t", vec![col("a", "integer")])]);
    // Created by the text, then renamed: not in the old model.
    let new = schema(vec![
        table("t", vec![col("a", "integer")]),
        table("y", vec![col("x", "integer")]),
    ]);
    let known = vec![rename(RenameKind::Table, None, "x", "y")];
    let r = resolve(&old, &new, &RenameChoices::exactly(known));
    assert!(r.candidates.is_empty());
    // Renamed, then dropped.
    let new = schema(vec![]);
    let known = vec![rename(RenameKind::Table, None, "t", "u")];
    assert!(
        resolve(&old, &new, &RenameChoices::exactly(known))
            .candidates
            .is_empty()
    );
    // A name another object still holds.
    let old = schema(vec![table(
        "t",
        vec![col("a", "integer"), col("b", "integer")],
    )]);
    let known = vec![rename(RenameKind::Column, Some("t"), "a", "b")];
    assert!(
        resolve(&old, &old, &RenameChoices::exactly(known))
            .candidates
            .is_empty()
    );
}

#[test]
fn an_index_renamed_with_alter_table_and_a_key_with_alter_index_land_on_their_kinds() {
    let mut t = table("t", vec![not_null(col("id", "integer"))]);
    t.constraints = vec![key(
        "t_pkey",
        ConstraintKind::PrimaryKey,
        &["id"],
        "PRIMARY KEY (id)",
    )];
    t.indexes = vec![index("t_id_idx", "t", "id")];
    let old = schema(vec![t.clone()]);
    let mut new = old.clone();
    new.entities[0].constraints[0].name = "t_key".into();
    new.entities[0].indexes = vec![index("t_by_id", "t", "id")];
    let known = vec![
        rename(RenameKind::Table, None, "t_id_idx", "t_by_id"),
        rename(RenameKind::Index, None, "t_pkey", "t_key"),
    ];
    let r = resolve(&old, &new, &RenameChoices::exactly(known));
    assert_eq!(
        summary(&r),
        [
            "constraint public.t.t_pkey → t_key Certain on",
            "index public.t.t_id_idx → t_by_id Certain on"
        ]
    );
    assert!(crate::diff(&r.schema, &new).is_empty());
}

#[test]
fn reversing_the_renames_of_a_migration_mirrors_it() {
    let mut users = table(
        "users",
        vec![
            serial("id", "users_id_seq"),
            col("email", "text"),
            col("name", "text"),
            col("note", "text"),
        ],
    );
    users.constraints = vec![key(
        "users_pkey",
        ConstraintKind::PrimaryKey,
        &["id"],
        "PRIMARY KEY (id)",
    )];
    let mut accounts = users.clone();
    accounts.name = "accounts".into();
    accounts.columns[0] = serial("id", "accounts_id_seq");
    accounts.columns[1].name = "email_address".into();
    accounts.constraints = vec![key(
        "accounts_pkey",
        ConstraintKind::PrimaryKey,
        &["id"],
        "PRIMARY KEY (id)",
    )];
    let (old, new) = (schema(vec![users]), schema(vec![accounts]));
    // The table renamed with one of its columns: only most of its columns
    // are alike, so it is unsure until turned on.
    let first = detected(&old, &new);
    assert_eq!(
        summary(&first),
        ["table public.users → accounts Medium off"]
    );
    let up = deciding(&old, &new, &[("table/public//users/accounts", true)]);
    assert_eq!(
        summary(&up),
        [
            "table public.users → accounts Medium on",
            "column public.accounts.email → email_address High on",
            "sequence public.accounts.users_id_seq → accounts_id_seq High on (with table/public//users/accounts)",
            "constraint public.accounts.users_pkey → accounts_pkey High on (with table/public//users/accounts)",
        ]
    );
    assert!(crate::diff(&up.schema, &new).is_empty());
    let down = resolve(&new, &old, &RenameChoices::reverse_of(&up.renames));
    assert_eq!(down.renames.len(), 4, "{:?}", summary(&down));
    assert!(
        down.candidates
            .iter()
            .all(|c| c.confidence == Confidence::Certain)
    );
    assert!(crate::diff(&down.schema, &old).is_empty());
}

#[test]
fn a_table_renamed_with_a_column_renamed_matches_by_its_columns_types_in_order() {
    let old = schema(vec![table(
        "users",
        vec![col("id", "integer"), col("email", "text")],
    )]);
    let new = schema(vec![table(
        "users_v2",
        vec![col("id", "integer"), col("mail", "text")],
    )]);
    let r = detected(&old, &new);
    assert_eq!(summary(&r)[0], "table public.users → users_v2 Medium off");
    assert_eq!(
        r.candidates[0].reason,
        "Columns of the same types, in the same order; similar name."
    );
}

#[test]
fn detection_off_finds_nothing_beyond_the_known() {
    let old = schema(vec![table(
        "users",
        vec![col("id", "integer"), col("email", "text")],
    )]);
    let new = schema(vec![table(
        "users",
        vec![col("id", "integer"), col("mail", "text")],
    )]);
    assert!(
        resolve(&old, &new, &RenameChoices::exactly(Vec::new()))
            .candidates
            .is_empty()
    );
}

#[test]
fn keys_escape_their_separators() {
    let r = Rename {
        kind: RenameKind::Column,
        schema: "a/b".into(),
        parent: Some("t\\x".into()),
        from: "c".into(),
        to: "d".into(),
        to_schema: None,
    };
    assert_eq!(r.key(), "column/a\\/b/t\\\\x/c/d");
}

#[test]
fn same_shaped_data_with_unrelated_names_is_never_renamed_by_default() {
    let old = schema(vec![table(
        "users",
        vec![col("id", "integer"), col("password_hash", "text")],
    )]);
    let new = schema(vec![table(
        "users",
        vec![col("id", "integer"), col("bio", "text")],
    )]);
    assert_eq!(
        summary(&detected(&old, &new)),
        ["column public.users.password_hash → bio Medium off"]
    );
    // A table of the same shape under an unrelated name, nothing following it.
    let old = schema(vec![table(
        "tags",
        vec![col("id", "integer"), col("label", "text")],
    )]);
    let new = schema(vec![table(
        "colors",
        vec![col("id", "integer"), col("label", "text")],
    )]);
    assert_eq!(
        summary(&detected(&old, &new)),
        ["table public.tags → colors Medium off"]
    );
}

#[test]
fn a_swap_reversed_from_its_net_pairs_is_still_a_swap() {
    let old = schema(vec![table(
        "t",
        vec![col("a", "integer"), col("b", "text")],
    )]);
    let new = schema(vec![table(
        "t",
        vec![col("b", "integer"), col("a", "text")],
    )]);
    let known = vec![
        rename(RenameKind::Column, Some("t"), "a", "tmp"),
        rename(RenameKind::Column, Some("t"), "b", "a"),
        rename(RenameKind::Column, Some("t"), "tmp", "b"),
    ];
    let up = resolve(
        &old,
        &new,
        &RenameChoices {
            known,
            ..RenameChoices::default()
        },
    );
    assert_eq!(up.renames.len(), 2);
    // The down, and a version's stored renames, give the net pairs back.
    let down = resolve(&new, &old, &RenameChoices::reverse_of(&up.renames));
    assert_eq!(down.renames.len(), 2, "{:?}", summary(&down));
    assert!(crate::diff(&down.schema, &old).is_empty());
    let net = resolve(&old, &new, &RenameChoices::exactly(up.renames.clone()));
    assert_eq!(net.renames, up.renames);
}

#[test]
fn a_table_moved_to_another_schema_is_known_and_detected() {
    let mut users = table("users", vec![col("id", "integer"), col("email", "text")]);
    users.indexes = vec![index("users_email_idx", "users", "email")];
    let old = schema(vec![users.clone()]);
    let mut moved = users;
    moved.schema = "archive".into();
    moved.indexes[0].definition =
        "CREATE INDEX users_email_idx ON archive.users USING btree (email)".into();
    let new = schema(vec![moved]);
    let r = detected(&old, &new);
    assert_eq!(summary(&r), ["table public.users → archive.users High on"]);
    assert!(crate::diff(&r.schema, &new).is_empty());
    let mut known = rename(RenameKind::Table, None, "users", "users");
    known.to_schema = Some("archive".into());
    let r = resolve(&old, &new, &RenameChoices::exactly(vec![known]));
    assert_eq!(
        summary(&r),
        ["table public.users → archive.users Certain on"]
    );
    assert!(crate::diff(&r.schema, &new).is_empty());
}

/// A version's stored renames: exactly these, nothing detected.
fn stored(known: Vec<Rename>) -> RenameChoices {
    RenameChoices::exactly(known)
}

#[test]
fn versions_compose_batch_by_batch_so_a_later_rename_lands_on_the_swapped_column() {
    // v1 t(a integer, b text); v2 swaps them; v3 renames b (once a) to c.
    let v1 = schema(vec![table(
        "t",
        vec![col("a", "integer"), col("b", "text")],
    )]);
    let v3 = schema(vec![table(
        "t",
        vec![col("c", "integer"), col("a", "text")],
    )]);
    let range = RenameChoices::compose([
        stored(vec![
            rename(RenameKind::Column, Some("t"), "a", "b"),
            rename(RenameKind::Column, Some("t"), "b", "a"),
        ]),
        stored(vec![rename(RenameKind::Column, Some("t"), "b", "c")]),
    ]);
    assert_eq!(range.batches, [2]);
    let up = resolve(&v1, &v3, &range);
    assert!(
        crate::diff(&up.schema, &v3).is_empty(),
        "c is the integer column: {:?}",
        summary(&up)
    );
    let mut net: Vec<String> = up.renames.iter().map(Rename::describe).collect();
    net.sort();
    assert_eq!(net, ["public.t.a → c", "public.t.b → a"]);
    // And back, batch by batch, the last first.
    let back = range.reversed();
    assert_eq!(back.batches, [1]);
    let down = resolve(&v3, &v1, &back);
    assert!(
        crate::diff(&down.schema, &v1).is_empty(),
        "{:?}",
        summary(&down)
    );
    // One batch, as the range used to be read: the swap waits to the end
    // and the later rename takes the other column.
    let flat = RenameChoices {
        batches: Vec::new(),
        ..range
    };
    assert!(!crate::diff(&resolve(&v1, &v3, &flat).schema, &v3).is_empty());
}

#[test]
fn an_unrelated_held_rename_does_not_cancel_a_swap() {
    let old = schema(vec![table(
        "t",
        vec![
            col("a", "integer"),
            col("b", "text"),
            col("x", "integer"),
            col("y", "integer"),
        ],
    )]);
    let new = schema(vec![table(
        "t",
        vec![
            col("b", "integer"),
            col("a", "text"),
            col("x", "integer"),
            col("y", "integer"),
        ],
    )]);
    let known = vec![
        rename(RenameKind::Column, Some("t"), "a", "b"),
        rename(RenameKind::Column, Some("t"), "b", "a"),
        // y is not going anywhere: this one waits for nothing.
        rename(RenameKind::Column, Some("t"), "x", "y"),
    ];
    let r = resolve(&old, &new, &stored(known));
    assert!(crate::diff(&r.schema, &new).is_empty(), "{:?}", summary(&r));
    assert_eq!(r.renames.len(), 2);
}

#[test]
fn a_step_s_decisions_read_as_the_later_steps_renamed_things() {
    let off = |key: &str| RenameChoices {
        decisions: [(key.to_owned(), false)].into(),
        ..RenameChoices::default()
    };
    let later = RenameChoices {
        known: vec![
            rename(RenameKind::Table, None, "users", "accounts"),
            rename(RenameKind::Column, Some("accounts"), "referral", "promo"),
        ],
        ..RenameChoices::default()
    };
    let range = RenameChoices::compose([off("column/public/users/legacy/referral"), later]);
    assert_eq!(
        range.decisions,
        BTreeMap::from([("column/public/accounts/legacy/promo".to_owned(), false)])
    );
    // Two steps deciding one key differently: off (one turned it off on
    // purpose), whichever came last.
    let on = RenameChoices {
        decisions: [("column/public/t/a/b".to_owned(), true)].into(),
        ..RenameChoices::default()
    };
    for range in [
        RenameChoices::compose([off("column/public/t/a/b"), on.clone()]),
        RenameChoices::compose([on, off("column/public/t/a/b")]),
    ] {
        assert_eq!(
            range.decisions,
            BTreeMap::from([("column/public/t/a/b".to_owned(), false)])
        );
    }
}

#[test]
fn keys_parse_back_into_their_renames() {
    let mut r = rename(RenameKind::Column, Some("we/ird\\name"), "a/b", "c");
    assert_eq!(Rename::from_key(&r.key()), Some(r.clone()));
    r = rename(RenameKind::Table, None, "users", "users");
    r.to_schema = Some("archive".into());
    assert_eq!(Rename::from_key(&r.key()), Some(r));
    assert_eq!(Rename::from_key("nope"), None);
    assert_eq!(Rename::from_key("thing/public//a/b"), None);
}

#[test]
fn a_column_renamed_in_a_swapped_table_follows_the_swap() {
    // One version: cats and dogs swap names, and meow (cats', now dogs')
    // becomes purr. Stored as its net renames, by level, the column under
    // its table's final name.
    let old = schema(vec![
        table("cats", vec![col("id", "integer"), col("meow", "text")]),
        table("dogs", vec![col("id", "integer"), col("woof", "text")]),
    ]);
    let new = schema(vec![
        table("cats", vec![col("id", "integer"), col("woof", "text")]),
        table("dogs", vec![col("id", "integer"), col("purr", "text")]),
    ]);
    let net = vec![
        rename(RenameKind::Table, None, "cats", "dogs"),
        rename(RenameKind::Table, None, "dogs", "cats"),
        rename(RenameKind::Column, Some("dogs"), "meow", "purr"),
    ];
    let up = resolve(&old, &new, &stored(net));
    assert!(
        crate::diff(&up.schema, &new).is_empty(),
        "{:?}",
        summary(&up)
    );
    assert_eq!(up.renames.len(), 3, "{:?}", summary(&up));
    assert!(
        up.candidates.iter().all(|c| c.applied),
        "{:?}",
        summary(&up)
    );
    // Back, from the renames it applied, as the down and a range do.
    let down = resolve(&new, &old, &RenameChoices::reverse_of(&up.renames));
    assert!(
        crate::diff(&down.schema, &old).is_empty(),
        "{:?}",
        summary(&down)
    );
    let range = RenameChoices::compose([stored(up.renames.clone())]).reversed();
    let back = resolve(&new, &old, &range);
    assert!(
        crate::diff(&back.schema, &old).is_empty(),
        "{:?}",
        summary(&back)
    );
}
