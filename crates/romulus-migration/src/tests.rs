//! Unit tests: one per kind of change, on small hand-made models, and the
//! file layouts. That the statements actually run and produce the new
//! schema is the oracle's job (`tests/oracle`, against the sandbox).

use remus_core::{
    Schema,
    fixtures::SHOWCASE,
    model::{
        Column, Constraint, ConstraintKind, Entity, EntityKind, EnumType, Extension, Identity,
        Index, Policy, PolicyCommand, RelationRef, TypeKind,
    },
};

use crate::{
    FileRole, Format, Hazard, Kind, Meta, Migration, Options, Section, Stage, Statement,
    WarningCode, archive_name, down, editable, files, migration,
};

fn schema(entities: Vec<Entity>) -> Schema {
    let mut s =
        Schema::from_json(r#"{"remus_version": "0.2", "database": "db", "server_version": "17"}"#)
            .expect("an empty model");
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

fn view(name: &str, over: &[&str], definition: &str) -> Entity {
    Entity {
        kind: EntityKind::View,
        view_definition: Some(definition.into()),
        depends_on: over
            .iter()
            .map(|t| RelationRef {
                schema: "public".into(),
                name: (*t).into(),
            })
            .collect(),
        ..table(name, Vec::new())
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

fn defaulting(mut c: Column, default: &str) -> Column {
    c.default = Some(default.into());
    c
}

fn pk(table: &str, column: &str) -> Constraint {
    Constraint {
        name: format!("{table}_pkey"),
        kind: ConstraintKind::PrimaryKey,
        columns: vec![column.into()],
        ref_schema: None,
        ref_table: None,
        ref_columns: Vec::new(),
        on_update: None,
        on_delete: None,
        deferrable: false,
        definition: format!("PRIMARY KEY ({column})"),
        comment: None,
    }
}

fn fk(name: &str, column: &str, target: &str) -> Constraint {
    Constraint {
        name: name.into(),
        kind: ConstraintKind::ForeignKey,
        columns: vec![column.into()],
        ref_schema: Some("public".into()),
        ref_table: Some(target.into()),
        ref_columns: vec!["id".into()],
        on_update: None,
        on_delete: None,
        deferrable: false,
        definition: format!("FOREIGN KEY ({column}) REFERENCES public.{target}(id)"),
        comment: None,
    }
}

fn plan(old: &Schema, new: &Schema) -> Migration {
    migration(old, new, &Options::default())
}

/// The migration with these renames turned on or off, by key.
fn plan_deciding(old: &Schema, new: &Schema, decisions: &[(&str, bool)]) -> Migration {
    let options = Options {
        renames: crate::RenameChoices {
            decisions: decisions
                .iter()
                .map(|(k, v)| ((*k).to_owned(), *v))
                .collect(),
            ..crate::RenameChoices::default()
        },
        ..Options::default()
    };
    migration(old, new, &options)
}

fn sqls(m: &Migration) -> Vec<&str> {
    m.statements.iter().map(|s| s.sql.as_str()).collect()
}

fn find<'m>(m: &'m Migration, prefix: &str) -> &'m Statement {
    m.statements
        .iter()
        .find(|s| s.sql.starts_with(prefix))
        .unwrap_or_else(|| panic!("no statement starting with {prefix:?} in {:#?}", sqls(m)))
}

fn position(m: &Migration, prefix: &str) -> usize {
    m.statements
        .iter()
        .position(|s| s.sql.starts_with(prefix))
        .unwrap_or_else(|| panic!("no statement starting with {prefix:?} in {:#?}", sqls(m)))
}

fn users() -> Entity {
    let mut users = table(
        "users",
        vec![not_null(col("id", "bigint")), col("email", "text")],
    );
    users.constraints.push(pk("users", "id"));
    users
}

#[test]
fn a_schema_migrates_to_itself_with_nothing() {
    let showcase = Schema::from_json(SHOWCASE).expect("the showcase");
    let mut shuffled = showcase.clone();
    shuffled.entities.reverse();
    shuffled.enums.reverse();
    assert!(plan(&showcase, &shuffled).is_empty());
    assert_eq!(plan(&showcase, &showcase), Migration::default());
}

#[test]
fn the_showcase_is_built_from_nothing_in_dependency_order() {
    let showcase = Schema::from_json(SHOWCASE).expect("the showcase");
    let m = plan(&schema(Vec::new()), &showcase);
    let first_table = position(&m, "CREATE TABLE");
    let first_fk = m
        .statements
        .iter()
        .position(|s| s.sql.contains(" FOREIGN KEY "))
        .expect("the showcase has foreign keys");
    let last_table = m
        .statements
        .iter()
        .rposition(|s| s.kind == Kind::CreateTable)
        .expect("tables");
    assert!(position(&m, "CREATE TYPE") < first_table);
    assert!(last_table < first_fk, "foreign keys after every table");
    assert_eq!(m.destructive(), 0, "creating destroys nothing");
}

#[test]
fn a_new_not_null_column_without_a_default_is_destructive() {
    let old = schema(vec![users()]);
    let mut new = old.clone();
    new.entities[0].columns.push(not_null(col("name", "text")));
    new.entities[0]
        .columns
        .push(not_null(defaulting(col("active", "boolean"), "true")));
    let m = plan(&old, &new);
    let name = find(&m, "ALTER TABLE public.users ADD COLUMN name");
    assert_eq!(
        name.sql,
        "ALTER TABLE public.users ADD COLUMN name text NOT NULL;"
    );
    assert!(name.destructive);
    assert!(!find(&m, "ALTER TABLE public.users ADD COLUMN active").destructive);
}

#[test]
fn dropped_columns_go_last_in_the_removals() {
    let old = schema(vec![users()]);
    let mut new = old.clone();
    new.entities[0].columns.retain(|c| c.name != "email");
    new.entities[0].columns.push(col("nickname", "varchar(20)"));
    let m = plan(&old, &new);
    let drop = m.statements.last().expect("statements");
    assert_eq!(drop.sql, "ALTER TABLE public.users DROP COLUMN email;");
    assert!(drop.destructive);
    assert_eq!(drop.section, Section::Removals);
    assert_eq!(drop.kind, Kind::DropColumn);
}

#[test]
fn widening_needs_no_using_and_narrowing_casts_and_is_flagged() {
    let old = schema(vec![table(
        "t",
        vec![col("a", "integer"), col("b", "bigint"), col("c", "uuid")],
    )]);
    let mut new = old.clone();
    new.entities[0].columns[0].r#type = "bigint".into();
    new.entities[0].columns[1].r#type = "integer".into();
    new.entities[0].columns[2].r#type = "bigint".into();
    let m = plan(&old, &new);
    let a = find(&m, "ALTER TABLE public.t ALTER COLUMN a");
    assert_eq!(a.sql, "ALTER TABLE public.t ALTER COLUMN a TYPE bigint;");
    assert!(!a.destructive);
    let b = find(&m, "ALTER TABLE public.t ALTER COLUMN b");
    assert_eq!(
        b.sql,
        "ALTER TABLE public.t ALTER COLUMN b TYPE integer USING b::integer;"
    );
    assert!(b.destructive);
    assert!(
        b.note
            .as_deref()
            .is_some_and(|n| n.contains("fails on a value"))
    );
    let c = find(&m, "ALTER TABLE public.t ALTER COLUMN c");
    assert!(c.sql.ends_with("USING c::text::bigint;"), "{}", c.sql);
    assert!(m.warnings.iter().any(|w| w.code == WarningCode::Cast));
}

#[test]
fn a_default_is_dropped_and_set_again_around_a_type_change() {
    let old = schema(vec![table(
        "t",
        vec![defaulting(col("price", "text"), "'0'::text")],
    )]);
    let mut new = old.clone();
    new.entities[0].columns[0] = defaulting(col("price", "numeric(10,2)"), "0");
    let m = plan(&old, &new);
    assert_eq!(
        sqls(&m),
        [
            "ALTER TABLE public.t\n    ALTER COLUMN price DROP DEFAULT,\n    ALTER COLUMN price TYPE numeric(10,2) USING price::numeric(10,2),\n    ALTER COLUMN price SET DEFAULT 0;"
        ]
    );
}

#[test]
fn nullability_identity_and_defaults_alter_in_place() {
    let old = schema(vec![table(
        "t",
        vec![
            col("a", "integer"),
            not_null(col("b", "bigint")),
            defaulting(col("c", "text"), "'x'::text"),
        ],
    )]);
    let mut new = old.clone();
    new.entities[0].columns[0].not_null = true;
    new.entities[0].columns[1].identity = Some(Identity::Always);
    new.entities[0].columns[2].default = None;
    let m = plan(&old, &new);
    let set_not_null = find(&m, "ALTER TABLE public.t ALTER COLUMN a SET NOT NULL");
    assert!(set_not_null.destructive, "SET NOT NULL fails on NULL rows");
    // The identity's sequence moved past the rows the column holds.
    assert!(
        position(
            &m,
            "ALTER TABLE public.t ALTER COLUMN b ADD GENERATED ALWAYS AS IDENTITY"
        ) < position(&m, "SELECT setval(pg_get_serial_sequence('public.t', 'b')")
    );
    find(&m, "ALTER TABLE public.t ALTER COLUMN c DROP DEFAULT");
}

fn mood(labels: &[&str]) -> EnumType {
    EnumType {
        schema: "public".into(),
        name: "mood".into(),
        labels: labels.iter().map(|l| (*l).into()).collect(),
        comment: None,
    }
}

fn mood_column() -> Column {
    Column {
        type_schema: "public".into(),
        type_name: "mood".into(),
        type_kind: TypeKind::Enum,
        ..col("mood", "public.mood")
    }
}

#[test]
fn enum_labels_are_added_where_they_go() {
    let mut old = schema(Vec::new());
    old.enums.push(mood(&["ok", "happy"]));
    let mut new = old.clone();
    new.enums[0] = mood(&["sad", "ok", "fine", "happy", "ecstatic"]);
    let m = plan(&old, &new);
    assert_eq!(
        sqls(&m),
        [
            "ALTER TYPE public.mood ADD VALUE 'sad' BEFORE 'ok';",
            "ALTER TYPE public.mood ADD VALUE 'fine' AFTER 'ok';",
            "ALTER TYPE public.mood ADD VALUE 'ecstatic';",
        ]
    );
    assert!(m.statements.iter().all(|s| s.stage == Stage::Transaction));
}

#[test]
fn an_enum_value_the_migration_uses_is_committed_first() {
    let mut old = schema(vec![table("people", vec![mood_column()])]);
    old.enums.push(mood(&["ok"]));
    let mut new = old.clone();
    new.enums[0] = mood(&["ok", "happy"]);
    new.entities[0].columns[0].default = Some("'happy'::public.mood".into());
    let m = plan(&old, &new);
    let add = find(&m, "ALTER TYPE public.mood ADD VALUE 'happy'");
    assert_eq!(add.stage, Stage::Commit);
    assert_eq!(
        m.statements[0].sql, add.sql,
        "commit-stage statements come first"
    );
}

#[test]
fn removing_an_enum_label_recreates_the_type_and_converts_its_columns() {
    let mut old = schema(vec![table("people", vec![mood_column()])]);
    old.enums.push(mood(&["sad", "ok", "happy"]));
    let mut new = old.clone();
    new.enums[0] = mood(&["sad", "fine", "happy"]);
    // One label out and one in at its place, names unalike: offered as a
    // rename, off unless turned on.
    let m = plan_deciding(&old, &new, &[("enum_value/public/mood/ok/fine", true)]);
    assert_eq!(
        sqls(&m),
        ["ALTER TYPE public.mood RENAME VALUE 'ok' TO 'fine';"]
    );
    let m = plan(&old, &new);
    let rename = find(&m, "ALTER TYPE public.mood RENAME TO mood_old");
    assert_eq!(
        rename.alternative.as_deref(),
        Some("ALTER TYPE public.mood RENAME VALUE 'ok' TO 'fine';"),
        "one label out, one in: probably a rename"
    );
    find(
        &m,
        "CREATE TYPE public.mood AS ENUM ('sad', 'fine', 'happy');",
    );
    let convert = find(
        &m,
        "ALTER TABLE public.people ALTER COLUMN mood TYPE public.mood",
    );
    assert!(convert.sql.ends_with("USING mood::text::public.mood;"));
    assert!(
        convert.destructive,
        "a row holding 'ok' fails the conversion"
    );
    assert_eq!(
        m.statements.last().expect("statements").sql,
        "DROP TYPE public.mood_old;"
    );
    assert!(
        m.warnings
            .iter()
            .any(|w| w.code == WarningCode::TypeRecreated)
    );
}

#[test]
fn a_dropped_and_an_added_column_alike_are_a_rename() {
    let old = schema(vec![users()]);
    let mut new = old.clone();
    new.entities[0].columns[1].name = "mail".into();
    new.entities[0].columns[1].r#type = "character varying(320)".into();
    // A type change makes it unsure: off until turned on.
    let m = plan(&old, &new);
    assert!(!m.renames[0].applied);
    assert_eq!(m.applied, []);
    let m = plan_deciding(&old, &new, &[("column/public/users/email/mail", true)]);
    // Renamed in place, then converted: never dropped and added.
    assert_eq!(
        sqls(&m),
        [
            "ALTER TABLE public.users RENAME COLUMN email TO mail;",
            "ALTER TABLE public.users ALTER COLUMN mail TYPE character varying(320);",
        ]
    );
    assert_eq!(m.statements[0].section, Section::Renames);
    assert_eq!(m.applied.len(), 1);
    assert_eq!(m.renames[0].key, "column/public/users/email/mail");
    assert!(m.renames[0].applied);
}

#[test]
fn a_rename_left_off_is_offered_on_its_drop_and_add() {
    let old = schema(vec![users()]);
    let mut new = old.clone();
    new.entities[0].columns[1].name = "mail".into();
    let m = plan_deciding(&old, &new, &[("column/public/users/email/mail", false)]);
    let rename = "ALTER TABLE public.users RENAME COLUMN email TO mail;";
    assert_eq!(
        find(&m, "ALTER TABLE public.users ADD COLUMN mail")
            .alternative
            .as_deref(),
        Some(rename)
    );
    assert_eq!(
        find(&m, "ALTER TABLE public.users DROP COLUMN email")
            .alternative
            .as_deref(),
        Some(rename)
    );
    assert!(
        m.warnings
            .iter()
            .any(|w| w.code == WarningCode::PossibleRename)
    );
}

#[test]
fn a_dropped_and_an_added_table_alike_suggest_a_rename() {
    let old = schema(vec![table("customers", vec![col("id", "bigint")])]);
    let new = schema(vec![table("clients", vec![col("id", "bigint")])]);
    let m = plan(&old, &new);
    let rename = Some("ALTER TABLE public.customers RENAME TO clients;");
    assert_eq!(
        find(&m, "CREATE TABLE public.clients")
            .alternative
            .as_deref(),
        rename
    );
    assert_eq!(
        find(&m, "DROP TABLE public.customers")
            .alternative
            .as_deref(),
        rename
    );
}

#[test]
fn a_table_moved_to_a_new_schema_creates_the_schema_then_moves_it() {
    let audit = || table("audit", vec![col("id", "bigint"), col("what", "text")]);
    let old = schema(vec![audit()]);
    let mut moved = audit();
    moved.schema = "archive".into();
    let new = schema(vec![moved]);
    let m = plan(&old, &new);
    assert_eq!(
        sqls(&m),
        [
            "CREATE SCHEMA IF NOT EXISTS archive;",
            "ALTER TABLE public.audit SET SCHEMA archive;"
        ]
    );
    let back = down(&old, &new, &m);
    assert_eq!(
        sqls(&back),
        ["ALTER TABLE archive.audit SET SCHEMA public;"]
    );
    let codes: Vec<_> = back.warnings.iter().map(|w| w.code).collect();
    assert_eq!(
        codes,
        [WarningCode::EmptySchema, WarningCode::FunctionBodies],
        "archive is left empty; a body naming archive.audit breaks"
    );
    assert!(
        m.warnings[0]
            .message
            .contains("public.audit → archive.audit (moved)")
    );
}

#[test]
fn renamed_and_converted_columns_warn_about_function_bodies() {
    let old = schema(vec![table(
        "people",
        vec![
            col("id", "bigint"),
            col("email", "text"),
            col("age", "integer"),
        ],
    )]);
    let new = schema(vec![table(
        "people",
        vec![
            col("id", "bigint"),
            col("email_address", "text"),
            col("age", "bigint"),
        ],
    )]);
    let m = plan(&old, &new);
    let w = m
        .warnings
        .iter()
        .find(|w| w.code == WarningCode::FunctionBodies)
        .expect("a warning about bodies");
    assert_eq!(
        w.message,
        "Functions and triggers are not in romulus's model, and Postgres does not rewrite their bodies: check any that use public.people.email → email_address (renamed), public.people.age (now bigint)."
    );
    let same = plan(&old, &old);
    assert_eq!(same.warnings, []);
}

#[test]
fn a_column_inserted_in_the_middle_warns_that_it_goes_last() {
    let old = schema(vec![users()]);
    let mut new = old.clone();
    new.entities[0].columns.insert(1, col("name", "text"));
    let m = plan(&old, &new);
    let warning = m
        .warnings
        .iter()
        .find(|w| w.code == WarningCode::ColumnOrder)
        .expect("an order warning");
    assert_eq!(warning.object.as_deref(), Some("public.users"));
    assert!(
        warning.message.contains("(id, email, name)"),
        "{}",
        warning.message
    );
}

#[test]
fn foreign_keys_between_new_tables_come_after_both() {
    let mut orders = table(
        "orders",
        vec![col("id", "bigint"), col("user_id", "bigint")],
    );
    orders.constraints.push(pk("orders", "id"));
    orders
        .constraints
        .push(fk("orders_user_id_fkey", "user_id", "users"));
    let new = schema(vec![orders, users()]);
    let m = plan(&schema(Vec::new()), &new);
    let fk = position(
        &m,
        "ALTER TABLE public.orders ADD CONSTRAINT orders_user_id_fkey",
    );
    assert!(position(&m, "CREATE TABLE public.users") < fk);
    assert!(position(&m, "CREATE TABLE public.orders") < fk);
}

#[test]
fn a_rebuilt_key_takes_the_foreign_keys_to_it_along() {
    let mut orders = table(
        "orders",
        vec![col("id", "bigint"), col("user_id", "bigint")],
    );
    orders
        .constraints
        .push(fk("orders_user_id_fkey", "user_id", "users"));
    let old = schema(vec![orders, users()]);
    let mut new = old.clone();
    new.entities[1].constraints[0].definition = "PRIMARY KEY (id, email)".into();
    let m = plan(&old, &new);
    let order = [
        "ALTER TABLE public.orders DROP CONSTRAINT orders_user_id_fkey",
        "ALTER TABLE public.users DROP CONSTRAINT users_pkey",
        "ALTER TABLE public.users ADD CONSTRAINT users_pkey",
        "ALTER TABLE public.orders ADD CONSTRAINT orders_user_id_fkey",
    ]
    .map(|prefix| position(&m, prefix));
    assert!(order.is_sorted(), "{order:?}");
}

#[test]
fn concurrently_builds_plain_indexes_on_existing_tables_after_the_transaction() {
    let old = schema(vec![users()]);
    let mut new = old.clone();
    new.entities[0].indexes.push(Index {
        name: "users_email_idx".into(),
        unique: false,
        partial: false,
        definition: "CREATE INDEX users_email_idx ON public.users USING btree (email)".into(),
    });
    new.entities[0].indexes.push(Index {
        name: "users_email_key".into(),
        unique: true,
        partial: false,
        definition: "CREATE UNIQUE INDEX users_email_key ON public.users USING btree (email)"
            .into(),
    });
    let m = migration(
        &old,
        &new,
        &Options {
            concurrently: true,
            ..Options::default()
        },
    );
    let plain = find(&m, "CREATE INDEX CONCURRENTLY users_email_idx");
    assert_eq!(plain.stage, Stage::Concurrent);
    let unique = find(&m, "CREATE UNIQUE INDEX users_email_key");
    assert_eq!(
        unique.stage,
        Stage::Transaction,
        "a foreign key may need it"
    );
    assert_eq!(m.statements.last().expect("statements").sql, plain.sql);
}

#[test]
fn views_drop_dependents_first_and_create_dependencies_first() {
    let base = table("t", vec![col("a", "integer")]);
    let v1 = view("v1", &["t"], " SELECT a FROM public.t;");
    let v2 = view("v2", &["v1"], " SELECT a FROM public.v1;");
    let old = schema(vec![base.clone(), v1.clone(), v2.clone()]);
    let mut new = old.clone();
    new.entities[0].columns[0].r#type = "bigint".into();
    let m = plan(&old, &new);
    assert!(position(&m, "DROP VIEW public.v2") < position(&m, "DROP VIEW public.v1"));
    assert!(
        position(&m, "DROP VIEW public.v1") < position(&m, "ALTER TABLE public.t ALTER COLUMN a")
    );
    assert!(
        position(&m, "ALTER TABLE public.t ALTER COLUMN a") < position(&m, "CREATE VIEW public.v1")
    );
    assert!(position(&m, "CREATE VIEW public.v1") < position(&m, "CREATE VIEW public.v2"));
}

#[test]
fn policies_alter_in_place_when_they_can() {
    let policy = Policy {
        name: "own".into(),
        command: PolicyCommand::Select,
        permissive: true,
        roles: vec!["app_user".into()],
        using: Some("(owner = CURRENT_USER)".into()),
        check: None,
    };
    let mut docs = table("docs", vec![col("owner", "text")]);
    docs.rls = true;
    docs.policies.push(policy);
    let old = schema(vec![docs]);
    let mut roles = old.clone();
    roles.entities[0].policies[0].roles = vec!["app_user".into(), "app_admin".into()];
    assert_eq!(
        sqls(&plan(&old, &roles)),
        ["ALTER POLICY own ON public.docs TO app_admin, app_user;"]
    );
    let mut command = old.clone();
    command.entities[0].policies[0].command = PolicyCommand::All;
    assert_eq!(
        sqls(&plan(&old, &command)),
        [
            "DROP POLICY own ON public.docs;",
            "CREATE POLICY own ON public.docs FOR ALL TO app_user USING (owner = CURRENT_USER);"
        ]
    );
}

#[test]
fn objects_an_extension_provides_are_left_to_it() {
    let mut postgis = schema(vec![table("spatial_ref_sys", vec![col("srid", "integer")])]);
    postgis.extensions.push(Extension {
        name: "postgis".into(),
        schema: "public".into(),
        version: "3.5.2".into(),
    });
    let m = plan(&schema(Vec::new()), &postgis);
    assert_eq!(
        sqls(&m),
        ["CREATE EXTENSION IF NOT EXISTS postgis WITH SCHEMA public;"]
    );
    let back = plan(&postgis, &schema(Vec::new()));
    assert_eq!(sqls(&back), ["DROP EXTENSION postgis;"]);
}

#[test]
fn extensions_are_created_after_and_dropped_before_what_they_require() {
    let mut both = schema(Vec::new());
    for name in ["earthdistance", "cube"] {
        both.extensions.push(Extension {
            name: name.into(),
            schema: "public".into(),
            version: "1.0".into(),
        });
    }
    let up = plan(&schema(Vec::new()), &both);
    assert!(
        position(&up, "CREATE EXTENSION IF NOT EXISTS cube")
            < position(&up, "CREATE EXTENSION IF NOT EXISTS earthdistance")
    );
    let down = plan(&both, &schema(Vec::new()));
    assert_eq!(
        sqls(&down),
        ["DROP EXTENSION earthdistance;", "DROP EXTENSION cube;"]
    );
}

// ---- Files -------------------------------------------------------------------

fn meta() -> Meta {
    Meta {
        database: "acme/app".into(),
        from: "v4".into(),
        to: "v5".into(),
        date: "2026-10-16".into(),
        version: "20261016093000".into(),
        name: "add_names".into(),
    }
}

fn with_index_concurrently() -> (Migration, Migration) {
    let old = schema(vec![users()]);
    let mut new = old.clone();
    new.entities[0].columns.push(not_null(col("name", "text")));
    new.entities[0].indexes.push(Index {
        name: "users_name_idx".into(),
        unique: false,
        partial: false,
        definition: "CREATE INDEX users_name_idx ON public.users USING btree (name)".into(),
    });
    let options = Options {
        concurrently: true,
        ..Options::default()
    };
    (
        migration(&old, &new, &options),
        migration(&new, &old, &Options::default()),
    )
}

fn meta_for(format: Format) -> Meta {
    Meta {
        version: if format == Format::Flyway {
            "5".into()
        } else {
            "20261016093000".into()
        },
        ..meta()
    }
}

fn paths(format: Format) -> Vec<String> {
    let (up, down) = with_index_concurrently();
    files(&up, &down, format, &meta_for(format))
        .into_iter()
        .map(|f| f.path)
        .collect()
}

#[test]
fn every_tool_gets_its_file_names() {
    assert_eq!(paths(Format::Plain), ["20261016093000_add_names.sql"]);
    assert_eq!(
        paths(Format::Sqlx),
        [
            "20261016093000_add_names.sql",
            "20261016093001_add_names_create_index_users_name_idx.sql"
        ]
    );
    assert_eq!(
        paths(Format::Flyway),
        [
            "V5__add_names.sql",
            "V5.1__add_names_create_index_users_name_idx.sql",
            "V5.1__add_names_create_index_users_name_idx.sql.conf"
        ]
    );
    assert_eq!(
        paths(Format::Prisma),
        [
            "20261016093000_add_names/migration.sql",
            "20261016093001_add_names_create_index_users_name_idx/migration.sql"
        ]
    );
    assert_eq!(
        paths(Format::Dbmate),
        [
            "20261016093000_add_names.sql",
            "20261016093001_add_names_create_index_users_name_idx.sql"
        ]
    );
    assert_eq!(
        paths(Format::GolangMigrate),
        [
            "20261016093000_add_names.up.sql",
            "20261016093000_add_names.down.sql",
            "20261016093001_add_names_create_index_users_name_idx.up.sql",
            "20261016093001_add_names_create_index_users_name_idx.down.sql"
        ]
    );
}

/// A migration written by hand: a column backfilled before NOT NULL, an
/// index built concurrently, then a constraint using it (order matters).
fn written() -> Migration {
    let statement = |sql: &str, stage: Stage| Statement {
        sql: sql.to_owned(),
        kind: Kind::Written,
        hazard: None,
        destructive: false,
        note: None,
        alternative: None,
        stage,
        section: Section::Written,
    };
    Migration {
        statements: vec![
            statement(
                "ALTER TABLE users ADD COLUMN name text;",
                Stage::Transaction,
            ),
            statement(
                "-- Backfill first.\nUPDATE users SET name = '';",
                Stage::Transaction,
            ),
            statement(
                "ALTER TABLE users ALTER COLUMN name SET NOT NULL;",
                Stage::Transaction,
            ),
            statement(
                "create unique index concurrently if not exists users_name on users (name);",
                Stage::Concurrent,
            ),
            statement(
                "ALTER TABLE users ADD CONSTRAINT users_name_key UNIQUE USING INDEX users_name;",
                Stage::Transaction,
            ),
        ],
        major: Some(17),
        written: true,
        ..Migration::default()
    }
}

#[test]
fn a_written_migration_keeps_its_order_split_around_what_cannot_run_in_a_transaction() {
    let (_, down) = with_index_concurrently();
    let up = written();
    let laid = |format: Format| files(&up, &down, format, &meta_for(format));
    let paths =
        |format: Format| -> Vec<String> { laid(format).into_iter().map(|f| f.path).collect() };
    assert_eq!(
        paths(Format::Sqlx),
        [
            "20261016093000_add_names.sql",
            "20261016093001_add_names_create_index_users_name.sql",
            "20261016093002_add_names_part_2.sql",
        ]
    );
    assert_eq!(
        paths(Format::Flyway),
        [
            "V5__add_names.sql",
            "V5.1__add_names_create_index_users_name.sql",
            "V5.1__add_names_create_index_users_name.sql.conf",
            "V5.2__add_names_part_2.sql",
        ]
    );
    assert_eq!(
        paths(Format::GolangMigrate),
        [
            "20261016093000_add_names.up.sql",
            "20261016093000_add_names.down.sql",
            "20261016093001_add_names_create_index_users_name.up.sql",
            "20261016093001_add_names_create_index_users_name.down.sql",
            "20261016093002_add_names_part_2.up.sql",
            "20261016093002_add_names_part_2.down.sql",
        ]
    );
    // The down migration, planned, goes with the first part.
    let golang = laid(Format::GolangMigrate);
    assert!(
        golang[1].content.contains("DROP COLUMN name"),
        "{}",
        golang[1].content
    );
    assert!(
        golang[5]
            .content
            .contains("Undone by the first part's down.")
    );
    let dbmate = laid(Format::Dbmate);
    assert!(
        dbmate[1]
            .content
            .contains("-- migrate:up transaction:false")
    );
    assert!(
        dbmate[2]
            .content
            .contains("Undone by the first part's down.")
    );

    // Plain SQL: each part in its transaction, in the order written.
    let plain = &laid(Format::Plain)[0].content;
    assert!(
        plain.starts_with(
            "-- Migration of acme/app from v4 to v5 for PostgreSQL 17, written by hand in\n\
             -- romulus; laid out on 2026-10-16.\n"
        ),
        "{plain}"
    );
    assert!(plain.contains("-- 5 statements.\n"), "{plain}");
    assert!(
        !plain.contains("-- Written by hand\n"),
        "no heading: {plain}"
    );
    let at = |needle: &str| {
        plain
            .find(needle)
            .unwrap_or_else(|| panic!("{needle}: {plain}"))
    };
    let order = [
        at("BEGIN;"),
        at("ADD COLUMN name"),
        at("-- Backfill first.\nUPDATE users"),
        at("SET NOT NULL"),
        at("COMMIT;"),
        at("concurrently if not exists"),
        plain.rfind("BEGIN;").unwrap(),
        at("USING INDEX"),
        plain.rfind("COMMIT;").unwrap(),
    ];
    assert!(order.is_sorted(), "{order:?}: {plain}");
    assert_eq!(plain.matches("BEGIN;").count(), 2, "{plain}");
    // Nothing flagged, nothing marked.
    assert!(laid(Format::Sqlx).iter().all(|f| f.marks.is_empty()));
}

#[test]
fn a_written_migration_of_concurrent_statements_only_still_has_its_down() {
    let (_, down) = with_index_concurrently();
    let mut up = written();
    up.statements.retain(|s| s.stage == Stage::Concurrent);
    let golang = files(&up, &down, Format::GolangMigrate, &meta());
    let paths: Vec<&str> = golang.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(
        paths,
        [
            "20261016093000_add_names.up.sql",
            "20261016093000_add_names.down.sql",
            "20261016093001_add_names_create_index_users_name.up.sql",
            "20261016093001_add_names_create_index_users_name.down.sql",
        ]
    );
    assert!(golang[0].content.contains("-- Nothing to do."));
    assert!(golang[1].content.contains("DROP COLUMN name"));
}

#[test]
fn plain_sql_wraps_the_transaction_and_runs_concurrent_indexes_after_it() {
    let (up, down) = with_index_concurrently();
    let file = &files(&up, &down, Format::Plain, &meta())[0];
    let content = &file.content;
    assert!(content.starts_with(
        "-- Migration of acme/app from v4 to v5, generated by romulus on 2026-10-16."
    ));
    let begin = content.find("BEGIN;").expect("BEGIN");
    let commit = content.find("COMMIT;").expect("COMMIT");
    let concurrent = content
        .find("CREATE INDEX CONCURRENTLY")
        .expect("the index");
    assert!(begin < commit && commit < concurrent);
}

#[test]
fn tools_with_their_own_transaction_get_none_in_the_file_and_opt_out_for_concurrent_ones() {
    let (up, down) = with_index_concurrently();
    let sqlx = files(&up, &down, Format::Sqlx, &meta());
    assert!(!sqlx[0].content.contains("BEGIN;"));
    assert!(sqlx[1].content.starts_with("-- no-transaction\n"));
    let flyway = files(&up, &down, Format::Flyway, &meta_for(Format::Flyway));
    assert_eq!(flyway[2].role, FileRole::Config);
    assert_eq!(flyway[2].content, "executeInTransaction=false\n");
    let dbmate = files(&up, &down, Format::Dbmate, &meta());
    assert!(
        dbmate[1]
            .content
            .contains("-- migrate:up transaction:false")
    );
}

#[test]
fn down_migrations_undo_the_up_one() {
    let (up, down) = with_index_concurrently();
    let dbmate = files(&up, &down, Format::Dbmate, &meta());
    let main = &dbmate[0].content;
    let (up_part, down_part) = main.split_once("-- migrate:down").expect("a down section");
    assert!(up_part.contains("-- migrate:up\n"));
    assert!(up_part.contains("ADD COLUMN name"));
    assert!(down_part.contains("ALTER TABLE public.users DROP COLUMN name;"));
    let golang = files(&up, &down, Format::GolangMigrate, &meta());
    assert_eq!(golang[1].role, FileRole::Down);
    assert!(golang[1].content.contains("BEGIN;"));
    assert!(golang[1].content.contains("DROP COLUMN name;"));
    assert!(golang[1].content.contains("Best effort"));
}

#[test]
fn marks_point_at_the_flagged_statements() {
    let (up, down) = with_index_concurrently();
    let file = &files(&up, &down, Format::Plain, &meta())[0];
    let lines: Vec<&str> = file.content.lines().collect();
    assert_eq!(file.marks.len(), 1);
    let mark = file.marks[0];
    assert_eq!(mark.hazard, Hazard::CanFail);
    let flagged = &lines[mark.from as usize - 1..mark.to as usize];
    assert!(flagged[0].starts_with("-- CAN FAIL:"), "{flagged:?}");
    assert_eq!(
        *flagged.last().expect("lines"),
        "ALTER TABLE public.users ADD COLUMN name text NOT NULL;"
    );
}

#[test]
fn an_empty_migration_says_so() {
    let same = schema(vec![users()]);
    let m = plan(&same, &same);
    let file = &files(&m, &m, Format::Plain, &meta())[0];
    assert!(file.content.contains("Nothing to migrate"));
    assert!(!file.content.contains("BEGIN;"));
}

// ---- Review fixes (MG-fix): each failed before its fix ----------------------

fn partition(parent: &str, child: &str, bound: &str) -> remus_core::model::Partition {
    remus_core::model::Partition {
        parent_schema: "public".into(),
        parent: parent.into(),
        child_schema: "public".into(),
        child: child.into(),
        bound: Some(bound.into()),
        partition_key: None,
    }
}

fn logs() -> Entity {
    let mut logs = table(
        "logs",
        vec![not_null(col("at", "date")), col("line", "text")],
    );
    logs.kind = EntityKind::PartitionedTable;
    logs.partition_key = Some("RANGE (at)".into());
    logs.indexes.push(Index {
        name: "logs_at_idx".into(),
        unique: false,
        partial: false,
        definition: "CREATE INDEX logs_at_idx ON ONLY public.logs USING btree (at)".into(),
    });
    logs
}

const BOUND_2025: &str = "FOR VALUES FROM ('2025-01-01') TO ('2026-01-01')";

#[test]
fn a_partition_that_becomes_a_table_is_detached_not_dropped() {
    let mut old = schema(vec![logs()]);
    old.partitions
        .push(partition("logs", "logs_2025", BOUND_2025));
    let mut kept = table(
        "logs_2025",
        vec![not_null(col("at", "date")), col("line", "text")],
    );
    kept.indexes.push(Index {
        name: "logs_2025_at_idx".into(),
        unique: false,
        partial: false,
        definition: "CREATE INDEX logs_2025_at_idx ON public.logs_2025 USING btree (at)".into(),
    });
    let new = schema(vec![logs(), kept]);
    let m = plan(&old, &new);
    assert_eq!(
        m.statements[0].sql,
        "ALTER TABLE public.logs DETACH PARTITION public.logs_2025;"
    );
    assert!(
        m.statements
            .iter()
            .all(|s| !s.sql.starts_with("DROP TABLE") && !s.sql.starts_with("CREATE TABLE")),
        "{:#?}",
        sqls(&m)
    );
    assert_eq!(m.destructive(), 0, "the rows stay");
}

#[test]
fn a_detached_partitions_names_are_guessed_out_loud() {
    // The model lists no partition's own indexes: its copy of `logs_at_idx`
    // is assumed to be `logs_2025_at_idx`, Postgres's name for it.
    let mut old = schema(vec![logs()]);
    old.partitions
        .push(partition("logs", "logs_2025", BOUND_2025));
    let mut kept = table(
        "logs_2025",
        vec![not_null(col("at", "date")), col("line", "text")],
    );
    kept.indexes.push(Index {
        name: "logs_2025_by_day".into(),
        unique: false,
        partial: false,
        definition: "CREATE INDEX logs_2025_by_day ON public.logs_2025 USING btree (at)".into(),
    });
    let new = schema(vec![logs(), kept]);
    let m = plan(&old, &new);
    let dropped = find(&m, "DROP INDEX IF EXISTS public.logs_2025_at_idx;");
    assert!(
        dropped
            .note
            .as_deref()
            .is_some_and(|n| n.contains("another name")),
        "{dropped:?}"
    );
    // The table may have been attached with its own index, kept as the
    // copy under its own name.
    find(
        &m,
        "CREATE INDEX IF NOT EXISTS logs_2025_by_day ON public.logs_2025",
    );
    let warning = m
        .warnings
        .iter()
        .find(|w| w.code == WarningCode::GuessedNames)
        .expect("a warning about the guessed names");
    assert_eq!(warning.object.as_deref(), Some("public.logs_2025"));
    assert!(
        warning.message.contains("`logs_2025_at_idx`"),
        "{warning:?}"
    );

    // Kept under the guessed name: created unless it is there.
    let mut same = new.clone();
    same.entities[1].indexes[0] = Index {
        name: "logs_2025_at_idx".into(),
        unique: false,
        partial: false,
        definition: "CREATE INDEX logs_2025_at_idx ON public.logs_2025 USING btree (at)".into(),
    };
    let m = plan(&old, &same);
    assert!(
        find(
            &m,
            "CREATE INDEX IF NOT EXISTS logs_2025_at_idx ON public.logs_2025"
        )
        .note
        .is_some()
    );
    assert!(sqls(&m).iter().all(|s| !s.starts_with("DROP INDEX")));
}

#[test]
fn a_table_that_becomes_a_partition_is_aligned_then_attached() {
    let mut archive = table(
        "logs_2025",
        vec![
            col("at", "date"),
            col("line", "text"),
            col("source", "text"),
        ],
    );
    archive.comment = Some("Imported".into());
    let old = schema(vec![logs(), archive]);
    let mut new = schema(vec![logs()]);
    new.partitions
        .push(partition("logs", "logs_2025", BOUND_2025));
    let m = plan(&old, &new);
    let order = [
        "ALTER TABLE public.logs_2025 ALTER COLUMN at SET NOT NULL;",
        "ALTER TABLE public.logs_2025 DROP COLUMN source;",
        "ALTER TABLE public.logs ATTACH PARTITION public.logs_2025 FOR VALUES",
    ]
    .map(|prefix| position(&m, prefix));
    assert!(order.is_sorted(), "{order:?}: {:#?}", sqls(&m));
    assert_eq!(
        find(&m, "ALTER TABLE public.logs ATTACH PARTITION").hazard,
        Some(Hazard::CanFail)
    );
    assert!(
        m.statements
            .iter()
            .all(|s| !s.sql.starts_with("DROP TABLE"))
    );
    assert!(
        m.warnings
            .iter()
            .all(|w| w.code != WarningCode::ColumnOrder),
        "a partition matches columns by name"
    );
}

#[test]
fn identity_to_serial_drops_the_identity_before_its_sequence_name_is_reused() {
    let mut identity = not_null(col("id", "bigint"));
    identity.identity = Some(Identity::ByDefault);
    let old = schema(vec![table("a", vec![identity])]);
    let new = schema(vec![table(
        "a",
        vec![not_null(defaulting(
            col("id", "bigint"),
            "nextval('public.a_id_seq'::regclass)",
        ))],
    )]);
    let m = plan(&old, &new);
    let order = [
        "ALTER TABLE public.a ALTER COLUMN id DROP IDENTITY;",
        "CREATE SEQUENCE IF NOT EXISTS public.a_id_seq;",
        "ALTER TABLE public.a ALTER COLUMN id SET DEFAULT nextval('public.a_id_seq'::regclass);",
        "SELECT setval('public.a_id_seq', coalesce(max(id), 0) + 1, false) FROM public.a;",
        "ALTER SEQUENCE public.a_id_seq OWNED BY public.a.id;",
    ]
    .map(|sql| position(&m, sql));
    assert!(order.is_sorted(), "{order:?}: {:#?}", sqls(&m));
    assert_eq!(find(&m, "SELECT setval").kind, Kind::SetSequence);

    // And back: the serial sequence goes, the identity catches up.
    let back = plan(&new, &old);
    let order = [
        "ALTER TABLE public.a ALTER COLUMN id DROP DEFAULT;",
        "DROP SEQUENCE public.a_id_seq;",
        "ALTER TABLE public.a ALTER COLUMN id ADD GENERATED BY DEFAULT AS IDENTITY;",
        "SELECT setval(pg_get_serial_sequence('public.a', 'id')",
    ]
    .map(|sql| position(&back, sql));
    assert!(order.is_sorted(), "{order:?}: {:#?}", sqls(&back));
}

#[test]
fn a_generated_column_loses_its_expression_before_the_column_it_reads_converts() {
    let mut g = col("g", "integer");
    g.generated = Some(remus_core::model::Generated::Stored);
    g.default = Some("(a * 2)".into());
    let old = schema(vec![table("t", vec![col("a", "integer"), g])]);
    let new = schema(vec![table(
        "t",
        vec![col("a", "bigint"), col("g", "integer")],
    )]);
    let m = plan(&old, &new);
    assert!(
        position(&m, "ALTER TABLE public.t ALTER COLUMN g DROP EXPRESSION;")
            < position(&m, "ALTER TABLE public.t ALTER COLUMN a TYPE bigint;")
    );
}

fn moods(labels: &[&str], default: &str) -> Schema {
    let mut column = mood_column();
    column.default = Some(format!("'{default}'::public.mood"));
    let mut s = schema(vec![table("days", vec![column])]);
    s.enums.push(mood(labels));
    s
}

#[test]
fn a_down_migration_commits_the_enum_values_it_uses_in_a_migration_of_its_own() {
    let (old, new) = (moods(&["ok", "sad"], "sad"), moods(&["ok"], "ok"));
    let (up, down) = (plan(&old, &new), plan(&new, &old));
    assert_eq!(
        find(&down, "ALTER TYPE public.mood ADD VALUE 'sad'").stage,
        Stage::Commit
    );
    for format in [Format::Dbmate, Format::GolangMigrate] {
        let laid = files(&up, &down, format, &meta());
        let main = laid
            .iter()
            .position(|f| f.path.contains("add_names.") || f.path.ends_with("add_names.sql"))
            .expect("the main file");
        let values = laid
            .iter()
            .position(|f| f.path.contains("_down_enum_values"))
            .unwrap_or_else(|| panic!("{format:?}: a migration for the down's enum values"));
        assert!(
            values > main,
            "{format:?}: after the main one, so its down runs first"
        );
        let down_text: String = laid
            .iter()
            .filter(|f| f.path.contains("_down_enum_values"))
            .map(|f| f.content.clone())
            .collect();
        assert!(down_text.contains("ADD VALUE 'sad'"), "{down_text}");
        let main_down: String = laid
            .iter()
            .filter(|f| !f.path.contains("_down_enum_values"))
            .map(|f| f.content.clone())
            .collect();
        assert!(!main_down.contains("ADD VALUE"), "{format:?}: {main_down}");
    }
}

#[test]
fn removing_a_domain_check_recreates_the_domain_rather_than_guess_its_name() {
    let domain = |checks: &[&str]| remus_core::model::DomainType {
        schema: "public".into(),
        name: "tag".into(),
        base_type: "text".into(),
        not_null: false,
        default: None,
        constraints: checks.iter().map(|c| (*c).into()).collect(),
        comment: None,
    };
    let mut old = schema(Vec::new());
    old.domains.push(domain(&[
        "CHECK ((VALUE <> ''::text))",
        "CHECK ((length(VALUE) < 40))",
    ]));
    let mut new = schema(Vec::new());
    new.domains.push(domain(&["CHECK ((VALUE <> ''::text))"]));
    let m = plan(&old, &new);
    assert!(
        m.statements
            .iter()
            .all(|s| !s.sql.contains("DROP CONSTRAINT")),
        "{:#?}",
        sqls(&m)
    );
    find(&m, "ALTER DOMAIN public.tag RENAME TO tag_old;");
    find(
        &m,
        "CREATE DOMAIN public.tag AS text CHECK ((VALUE <> ''::text));",
    );
}

#[test]
fn what_can_fail_on_existing_rows_is_flagged_apart_from_what_deletes_data() {
    let old = schema(vec![table(
        "t",
        vec![
            col("a", "integer"),
            col("n", "numeric(10,2)"),
            col("gone", "text"),
        ],
    )]);
    let mut new = schema(vec![table(
        "t",
        vec![col("a", "integer"), col("n", "integer")],
    )]);
    new.entities[0].constraints.push(Constraint {
        name: "t_a_check".into(),
        kind: ConstraintKind::Check,
        definition: "CHECK ((a > 0))".into(),
        columns: Vec::new(),
        ..pk("t", "a")
    });
    new.entities[0].indexes.push(Index {
        name: "t_a_key".into(),
        unique: true,
        partial: false,
        definition: "CREATE UNIQUE INDEX t_a_key ON public.t USING btree (a)".into(),
    });
    let m = plan(&old, &new);
    assert_eq!(
        find(&m, "ALTER TABLE public.t ADD CONSTRAINT t_a_check").hazard,
        Some(Hazard::CanFail)
    );
    assert_eq!(
        find(&m, "CREATE UNIQUE INDEX t_a_key").hazard,
        Some(Hazard::CanFail)
    );
    assert_eq!(
        find(&m, "ALTER TABLE public.t ALTER COLUMN n TYPE integer").hazard,
        Some(Hazard::DeletesData),
        "rounds"
    );
    assert_eq!(
        find(&m, "ALTER TABLE public.t DROP COLUMN gone").hazard,
        Some(Hazard::DeletesData)
    );
    assert!(
        m.statements
            .iter()
            .filter(|s| s.hazard.is_some())
            .all(|s| s.destructive)
    );

    // Created with its table: nothing to fail on.
    let fresh = plan(&schema(Vec::new()), &new);
    assert!(
        fresh.statements.iter().all(|s| s.hazard.is_none()),
        "{:#?}",
        sqls(&fresh)
    );
}

fn guarded(policies: Vec<Policy>, rls: bool, forced: bool) -> Schema {
    let mut docs = table("docs", vec![col("owner", "text")]);
    docs.rls = rls;
    docs.rls_forced = forced;
    docs.policies = policies;
    schema(vec![docs])
}

fn policy(name: &str, permissive: bool) -> Policy {
    Policy {
        name: name.into(),
        command: PolicyCommand::Select,
        permissive,
        roles: vec!["app_user".into()],
        using: Some("(owner = CURRENT_USER)".into()),
        check: None,
    }
}

#[test]
fn what_widens_access_is_flagged_and_warned() {
    let old = guarded(
        vec![policy("only_mine", false), policy("own", true)],
        true,
        true,
    );
    let new = guarded(
        vec![policy("own", true), policy("everyone", true)],
        false,
        false,
    );
    let m = plan(&old, &new);
    for sql in [
        "ALTER TABLE public.docs DISABLE ROW LEVEL SECURITY;",
        "ALTER TABLE public.docs NO FORCE ROW LEVEL SECURITY;",
        "DROP POLICY only_mine ON public.docs;",
        "CREATE POLICY everyone ON public.docs",
    ] {
        assert_eq!(find(&m, sql).hazard, Some(Hazard::WidensAccess), "{sql}");
        assert!(!find(&m, sql).destructive);
    }
    let warned = m
        .warnings
        .iter()
        .filter(|w| w.code == WarningCode::WidensAccess)
        .count();
    assert_eq!(warned, 4);

    // A permissive policy dropped narrows access: noted, not flagged.
    let narrower = guarded(Vec::new(), true, true);
    let m = plan(&guarded(vec![policy("own", true)], true, true), &narrower);
    let drop = find(&m, "DROP POLICY own ON public.docs;");
    assert_eq!(drop.hazard, None);
    assert!(
        drop.note
            .as_deref()
            .is_some_and(|n| n.starts_with("Narrows access"))
    );
}

#[test]
fn archives_and_versions_come_from_the_migration_not_its_first_file() {
    let flyway = meta_for(Format::Flyway);
    assert_eq!(archive_name(Format::Flyway, &flyway), "V5__add_names.zip");
    assert_eq!(
        archive_name(Format::Sqlx, &meta()),
        "20261016093000_add_names.zip"
    );
    // golang-migrate -seq numbering goes on with its padding.
    let seq = Meta {
        version: "000041".into(),
        ..meta()
    };
    let (up, down) = with_index_concurrently();
    let paths: Vec<String> = files(&up, &down, Format::GolangMigrate, &seq)
        .into_iter()
        .map(|f| f.path)
        .collect();
    assert_eq!(paths[0], "000041_add_names.up.sql");
    assert_eq!(
        paths[2],
        "000042_add_names_create_index_users_name_idx.up.sql"
    );
}

#[test]
fn a_prisma_concurrent_index_stands_alone_without_claiming_a_transaction() {
    let (up, down) = with_index_concurrently();
    let prisma = files(&up, &down, Format::Prisma, &meta());
    let concurrent = &prisma[1].content;
    assert!(
        !concurrent.contains("run this statement yourself"),
        "{concurrent}"
    );
    assert_eq!(
        concurrent.matches(';').count(),
        1,
        "one statement: {concurrent}"
    );
}

#[test]
fn the_editable_text_has_the_statements_and_no_transaction() {
    let old = schema(vec![users()]);
    let mut new = old.clone();
    new.entities[0].columns.push(not_null(col("name", "text")));
    let up = migration(&old, &new, &Options::default());
    let text = editable(&up, &meta());
    assert!(
        text.starts_with(
            "-- acme/app: migration from v4, written by hand.\n\
             -- Started from romulus's generated migration (2026-10-16):\n\
             -- edit it freely, the schema and the graph follow.\n"
        ),
        "{text}"
    );
    // Narrow: it is edited beside the graph.
    assert!(
        text.lines()
            .filter(|l| l.starts_with("--"))
            .all(|l| l.chars().count() <= 60),
        "{text}"
    );
    assert!(text.contains("-- Tables and columns\n"), "{text}");
    for statement in &up.statements {
        assert!(text.contains(&statement.sql), "{text}");
    }
    assert!(
        !text.contains("BEGIN;") && !text.contains("COMMIT;"),
        "{text}"
    );
    assert!(text.ends_with(";\n"), "{text:?}");
    // Nothing to migrate yet: an invitation to write it.
    let empty = editable(&migration(&old, &old, &Options::default()), &meta());
    assert_eq!(
        empty,
        "-- acme/app: migration from v4, written by hand.\n\
         -- Write what changes v4 here (ALTER TABLE …):\n\
         -- the schema and the graph follow from it.\n"
    );
}

// ---- Target majors (Options::major) -------------------------------------------

fn on(major: u32) -> Options {
    Options {
        major: Some(major),
        ..Options::default()
    }
}

fn generated(expression: &str) -> Column {
    let mut g = col("g", "integer");
    g.generated = Some(remus_core::model::Generated::Stored);
    g.default = Some(expression.into());
    g
}

#[test]
fn a_new_expression_is_set_in_place_from_17_and_readded_before() {
    let old = schema(vec![table(
        "t",
        vec![col("a", "integer"), generated("(a * 2)")],
    )]);
    let new = schema(vec![table(
        "t",
        vec![col("a", "integer"), generated("(a * 3)")],
    )]);

    for options in [Options::default(), on(17), on(18)] {
        let m = migration(&old, &new, &options);
        assert_eq!(
            sqls(&m),
            ["ALTER TABLE public.t ALTER COLUMN g SET EXPRESSION AS ((a * 3));"],
            "{options:?}"
        );
        assert_eq!(m.warnings, []);
    }
    for major in [15, 16] {
        let m = migration(&old, &new, &on(major));
        let drop = find(&m, "ALTER TABLE public.t DROP COLUMN g;");
        assert!(
            drop.note
                .as_deref()
                .is_some_and(|n| n.contains(&format!("PostgreSQL {major} has no SET EXPRESSION"))),
            "{:?}",
            drop.note
        );
        // Its values are generated: nothing is lost.
        assert_eq!(drop.hazard, None);
        assert!(
            position(&m, "ALTER TABLE public.t DROP COLUMN g;")
                < position(
                    &m,
                    "ALTER TABLE public.t ADD COLUMN g integer GENERATED ALWAYS AS ((a * 3)) STORED;"
                )
        );
        assert!(!sqls(&m).iter().any(|s| s.contains("SET EXPRESSION")));
        assert_eq!(m.warnings, []);
    }
}

fn with_constraint(name: &str, kind: ConstraintKind, definition: &str) -> Entity {
    let mut t = table("t", vec![col("a", "integer"), col("b", "integer")]);
    t.constraints.push(Constraint {
        name: name.into(),
        kind,
        columns: vec!["a".into()],
        definition: definition.into(),
        ..pk("t", "a")
    });
    t
}

#[test]
fn syntax_newer_than_the_target_is_kept_noted_and_warned_about() {
    let old = schema(vec![table(
        "t",
        vec![col("a", "integer"), col("b", "integer")],
    )]);
    let new = schema(vec![with_constraint(
        "t_a_key",
        ConstraintKind::Unique,
        "UNIQUE NULLS NOT DISTINCT (a)",
    )]);
    let add = "ALTER TABLE public.t ADD CONSTRAINT t_a_key UNIQUE NULLS NOT DISTINCT (a);";

    // 15 has it (and no target means the newest).
    for options in [Options::default(), on(15)] {
        let m = migration(&old, &new, &options);
        assert_eq!(m.warnings, []);
        assert!(
            !find(&m, add)
                .note
                .as_deref()
                .unwrap_or("")
                .contains("Fails on PostgreSQL")
        );
    }
    let m = migration(&old, &new, &on(14));
    let statement = find(&m, add);
    assert!(
        statement.note.as_deref().is_some_and(|n| n
            .ends_with(" Fails on PostgreSQL 14: it uses NULLS NOT DISTINCT (PostgreSQL 15+).")),
        "{:?}",
        statement.note
    );
    assert_eq!(m.warnings.len(), 1);
    assert_eq!(m.warnings[0].code, WarningCode::NewerPostgres);
    assert!(
        m.warnings[0]
            .message
            .contains("needs PostgreSQL 15 or later; the database is on 14"),
        "{}",
        m.warnings[0].message
    );
}

#[test]
fn a_virtual_column_and_unenforced_constraints_need_18() {
    let mut v = generated("(a * 2)");
    v.generated = Some(remus_core::model::Generated::Virtual);
    let old = schema(vec![table("t", vec![col("a", "integer")])]);
    let mut wanted = with_constraint(
        "t_a_check",
        ConstraintKind::Check,
        "CHECK ((a > 0)) NOT ENFORCED",
    );
    wanted.columns.push(v);
    let new = schema(vec![wanted]);

    let m = migration(&old, &new, &on(17));
    let add = find(
        &m,
        "ALTER TABLE public.t ADD COLUMN g integer GENERATED ALWAYS AS ((a * 2)) VIRTUAL;",
    );
    assert!(
        add.note
            .as_deref()
            .is_some_and(|n| n.contains("virtual generated columns (PostgreSQL 18+)"))
    );
    let check = find(&m, "ALTER TABLE public.t ADD CONSTRAINT t_a_check");
    assert!(
        check
            .note
            .as_deref()
            .is_some_and(|n| n.contains("NOT ENFORCED constraints"))
    );
    let codes: Vec<_> = m.warnings.iter().map(|w| w.code).collect();
    assert_eq!(
        codes,
        [WarningCode::NewerPostgres, WarningCode::NewerPostgres]
    );
    assert_eq!(migration(&old, &new, &on(18)).warnings, []);
}

#[test]
fn an_exclusion_constraint_on_a_partitioned_table_needs_17() {
    let mut partitioned = with_constraint(
        "t_a_excl",
        ConstraintKind::Exclusion,
        "EXCLUDE USING gist (a WITH =)",
    );
    partitioned.kind = EntityKind::PartitionedTable;
    partitioned.partition_key = Some("RANGE (a)".into());
    let new = schema(vec![partitioned.clone()]);

    // Created with the table, or added to it later.
    let created = migration(&schema(Vec::new()), &new, &on(16));
    assert!(
        find(&created, "CREATE TABLE public.t")
            .note
            .as_deref()
            .is_some_and(
                |n| n.contains("exclusion constraints on partitioned tables (PostgreSQL 17+)")
            )
    );
    assert_eq!(created.warnings[0].code, WarningCode::NewerPostgres);
    let mut bare = partitioned;
    bare.constraints.clear();
    let added = migration(&schema(vec![bare]), &new, &on(16));
    assert!(
        find(&added, "ALTER TABLE public.t ADD CONSTRAINT t_a_excl")
            .note
            .as_deref()
            .is_some_and(|n| n.contains("PostgreSQL 17+"))
    );
    assert_eq!(migration(&schema(Vec::new()), &new, &on(17)).warnings, []);

    // On a plain table, every major has them.
    let plain = schema(vec![with_constraint(
        "t_a_excl",
        ConstraintKind::Exclusion,
        "EXCLUDE USING gist (a WITH =)",
    )]);
    assert_eq!(migration(&schema(Vec::new()), &plain, &on(15)).warnings, []);
}
