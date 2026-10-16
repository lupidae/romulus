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
    FileRole, Format, Kind, Meta, Migration, Options, Section, Stage, Statement, WarningCode,
    files, migration,
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
    let identity = find(
        &m,
        "ALTER TABLE public.t ALTER COLUMN b ADD GENERATED ALWAYS AS IDENTITY",
    );
    assert!(
        identity
            .note
            .as_deref()
            .is_some_and(|n| n.contains("setval"))
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
fn a_dropped_and_an_added_column_alike_suggest_a_rename() {
    let old = schema(vec![users()]);
    let mut new = old.clone();
    new.entities[0].columns[1].name = "mail".into();
    let m = plan(&old, &new);
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
    let m = migration(&old, &new, &Options { concurrently: true });
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
        timestamp: "20261016093000".into(),
        version: 5,
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
    let options = Options { concurrently: true };
    (
        migration(&old, &new, &options),
        migration(&new, &old, &Options::default()),
    )
}

fn paths(format: Format) -> Vec<String> {
    let (up, down) = with_index_concurrently();
    files(&up, &down, format, &meta())
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
    let flyway = files(&up, &down, Format::Flyway, &meta());
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
fn destructive_lines_point_at_the_flagged_statements() {
    let (up, down) = with_index_concurrently();
    let file = &files(&up, &down, Format::Plain, &meta())[0];
    let lines: Vec<&str> = file.content.lines().collect();
    assert_eq!(file.destructive.len(), 1);
    let range = file.destructive[0];
    let flagged = &lines[range.from as usize - 1..range.to as usize];
    assert!(flagged[0].starts_with("-- DESTRUCTIVE:"), "{flagged:?}");
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
