//! Realistic schemas (tests/corpus): each compiles, and its model survives
//! the trip a draft opened from a pushed version takes: model → SQL
//! (`render_sql`) → compile → the same canonical model.

use remus_core::{
    Schema,
    fixtures::SHOWCASE,
    model::{EntityKind, Generated},
};
use romulus_sandbox::{Code, Compilation, Compiler, render_sql};

use crate::{compile_ok, explain, with_compiler};

/// Compile `sql`, then its rendered model, and require the same model.
pub async fn round_trip(compiler: &Compiler, name: &str, sql: &str) -> (Compilation, Schema) {
    let first = compile_ok(compiler, sql).await;
    let model = first.model.clone().expect("a model");
    let rendered = render_sql(&model);
    let second = compiler.compile(&rendered).await.expect("the sandbox runs");
    assert!(
        second.is_ok(),
        "{name}: the rendered model does not compile back:\n{}\n--- rendered SQL ---\n{rendered}",
        explain(&rendered, &second)
    );
    let again = second.model.clone().expect("a model");
    assert_same(name, &model, &again);
    (first, model)
}

/// Equal models, or a panic showing where they first differ.
pub fn assert_same(name: &str, expected: &Schema, actual: &Schema) {
    if expected == actual {
        return;
    }
    let pretty = |s: &Schema| serde_json::to_string_pretty(s).expect("serialisable");
    let (left, right) = (pretty(expected), pretty(actual));
    let (left, right): (Vec<&str>, Vec<&str>) = (left.lines().collect(), right.lines().collect());
    let first = left
        .iter()
        .zip(&right)
        .position(|(a, b)| a != b)
        .unwrap_or(left.len().min(right.len()));
    let from = first.saturating_sub(12);
    panic!(
        "{name}: the model changed after the round trip, from line {first}:\n--- compiled ---\n{}\n--- recompiled ---\n{}",
        left[from..(first + 6).min(left.len())].join("\n"),
        right[from..(first + 6).min(right.len())].join("\n"),
    );
}

fn entity<'a>(model: &'a Schema, schema: &str, name: &str) -> &'a remus_core::model::Entity {
    model
        .entities
        .iter()
        .find(|e| e.schema == schema && e.name == name)
        .unwrap_or_else(|| panic!("no entity {schema}.{name}"))
}

fn signatures(compilation: &Compilation) -> Vec<String> {
    compilation
        .stubs
        .iter()
        .map(romulus_sandbox::Stub::signature)
        .collect()
}

macro_rules! corpus {
    ($test:ident, $file:literal, |$compilation:ident, $model:ident| $check:block) => {
        #[test]
        fn $test() {
            with_compiler(|compiler| async move {
                let sql = include_str!(concat!("../corpus/", $file));
                let ($compilation, $model) = round_trip(compiler, $file, sql).await;
                $check
            });
        }
    };
}

corpus!(
    saas_with_rls_policies_to_roles,
    "01_saas_rls.sql",
    |compilation, model| {
        let orgs = entity(&model, "app", "organizations");
        assert!(orgs.rls);
        let admins = orgs
            .policies
            .iter()
            .find(|p| p.name == "org_admins")
            .unwrap();
        assert_eq!(admins.roles, ["app_user", "app_admin"]);
        let projects = entity(&model, "app", "projects");
        assert!(projects.rls_forced);
        let restrictive = projects
            .policies
            .iter()
            .find(|p| p.name == "project_no_archived")
            .unwrap();
        assert!(!restrictive.permissive);
        assert_eq!(restrictive.roles, ["app_user"]);
        assert_eq!(
            signatures(&compilation),
            [
                "app.is_org_admin(uuid) → boolean",
                "app.current_org_id() → uuid",
            ]
        );
    }
);

corpus!(
    ecommerce_with_enums_domains_and_composites,
    "02_ecommerce.sql",
    |_compilation, model| {
        assert_eq!(model.enums[0].labels.len(), 6);
        let domains: Vec<&str> = model.domains.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(domains, ["cents", "email", "sku"]);
        // NOT NULL and a CHECK: read once each, whatever the Postgres major.
        let cents = &model.domains[0];
        assert!(cents.not_null);
        assert_eq!(cents.constraints, ["CHECK ((VALUE >= 0))"]);
        assert_eq!(model.composites[0].name, "address");
        let items = entity(&model, "public", "order_items");
        let total = items.column("line_total").unwrap();
        assert_eq!(total.generated, Some(Generated::Stored));
        assert!(model.extensions.iter().any(|e| e.name == "citext"));
    }
);

corpus!(
    partitioned_tables_three_ways,
    "03_partitioned_events.sql",
    |_compilation, model| {
        // 3 + 3 + 4, plus the multi-level tree: 2 regions, 2 + 1 months under them.
        assert_eq!(model.partitions.len(), 15);
        let eu = model
            .partitions
            .iter()
            .find(|p| p.child == "orders_eu")
            .unwrap();
        assert_eq!(eu.partition_key.as_deref(), Some("RANGE (placed_on)"));
        let eu_2026 = model
            .partitions
            .iter()
            .find(|p| p.child == "orders_eu_2026")
            .unwrap();
        assert_eq!(eu_2026.parent, "orders_eu");
        assert_eq!(eu_2026.partition_key, None);
        let events = entity(&model, "analytics", "events");
        assert_eq!(events.kind, EntityKind::PartitionedTable);
        assert_eq!(events.partition_key.as_deref(), Some("RANGE (occurred_at)"));
        // Partitions are listed under the parent, not as entities.
        assert!(
            model
                .entities
                .iter()
                .all(|e| !e.name.starts_with("events_"))
        );
    }
);

corpus!(
    identity_serial_and_generated_columns,
    "04_generated_identity.sql",
    |_compilation, model| {
        let invoices = entity(&model, "public", "invoices");
        assert!(invoices.column("id").unwrap().identity.is_some());
        assert_eq!(
            invoices.column("number").unwrap().default.as_deref(),
            Some("nextval('public.invoices_number_seq'::regclass)")
        );
        assert_eq!(
            invoices.column("search").unwrap().generated,
            Some(Generated::Stored)
        );
    }
);

corpus!(
    views_recursive_views_and_materialized_views,
    "05_views.sql",
    |_compilation, model| {
        let views = model.entities.iter().filter(|e| e.kind.is_view()).count();
        assert_eq!(views, 6);
        let payroll = entity(&model, "public", "department_payroll");
        assert_eq!(payroll.kind, EntityKind::MaterializedView);
        assert_eq!(payroll.comment.as_deref(), Some("Refreshed nightly"));
        assert_eq!(
            entity(&model, "public", "department_heads")
                .depends_on
                .len(),
            1
        );
    }
);

corpus!(
    postgis_columns_and_indexes,
    "06_postgis.sql",
    |_compilation, model| {
        let places = entity(&model, "public", "places");
        // Types outside pg_catalog are schema-qualified, as remus prints them.
        assert_eq!(
            places.column("location").unwrap().r#type,
            "public.geometry(Point,4326)"
        );
        // remus lists what PostGIS creates, like it would on a real database;
        // the round trip skipped re-creating them.
        assert!(model.entities.iter().any(|e| e.name == "spatial_ref_sys"));
        assert!(model.extensions.iter().any(|e| e.name == "postgis"));
    }
);

corpus!(
    pgvector_columns_and_hnsw_indexes,
    "07_pgvector.sql",
    |_compilation, model| {
        let documents = entity(&model, "public", "documents");
        assert_eq!(
            documents.column("embedding").unwrap().r#type,
            "public.vector(1536)"
        );
        assert!(
            documents
                .indexes
                .iter()
                .any(|i| i.definition.contains("USING hnsw"))
        );
    }
);

corpus!(
    functions_the_schema_does_not_define,
    "08_custom_functions.sql",
    |compilation, model| {
        let articles = entity(&model, "public", "articles");
        let default = |c: &str| articles.column(c).unwrap().default.clone();
        assert_eq!(default("id").as_deref(), Some("util.next_id()"));
        // Unqualified calls print as on a real database where the function
        // lives in public.
        assert_eq!(
            default("public_id").as_deref(),
            Some("public.gen_short_id(10)")
        );
        assert_eq!(default("slug").as_deref(), Some("util.slugify(title)"));
        assert_eq!(
            default("reading_minutes").as_deref(),
            Some("util.reading_time(''::text)")
        );
        let tenant = articles
            .policies
            .iter()
            .find(|p| p.name == "tenant_isolation")
            .unwrap();
        assert_eq!(
            tenant.using.as_deref(),
            Some("(tenant_id = util.current_tenant())")
        );
        assert_eq!(
            signatures(&compilation),
            [
                "util.next_id() → bigint",
                "public.gen_short_id(integer) → text",
                "util.slugify(text) → text",
                "util.current_user_id() → uuid",
                "util.current_tenant() → integer",
                "util.is_valid_slug(text) → boolean",
                "public.is_email(text) → boolean",
                "util.normalize_email(text) → text",
                "util.unaccent_lower(character varying) → text",
                "util.has_role(text) → boolean",
                "util.excerpt(text, integer) → text",
                "util.reading_time(text) → integer",
            ]
        );
    }
);

corpus!(
    circular_foreign_keys_added_by_alter,
    "09_circular_fks.sql",
    |_compilation, model| {
        let departments = entity(&model, "public", "departments");
        let head = departments.foreign_keys().next().unwrap();
        assert!(head.deferrable);
        assert_eq!(head.ref_table.as_deref(), Some("employees"));
        let notes = entity(&model, "public", "assignment_notes");
        assert_eq!(
            notes.foreign_keys().next().unwrap().ref_columns,
            ["employee_id", "department_id"]
        );
    }
);

corpus!(
    several_schemas_quoted_names_and_comments,
    "10_multi_schema.sql",
    |_compilation, model| {
        let invoice = entity(&model, "billing", "Invoice");
        assert_eq!(
            invoice
                .constraints
                .iter()
                .find(|c| c.kind == remus_core::model::ConstraintKind::Check)
                .unwrap()
                .comment
                .as_deref(),
            Some("No credit notes here")
        );
        let accounts = entity(&model, "auth", "accounts");
        assert!(accounts.column("user").unwrap().comment.is_some());
        assert_eq!(
            entity(&model, "Reporting", "Revenue").comment.as_deref(),
            Some("Per currency")
        );
    }
);

corpus!(
    common_extensions,
    "11_extensions.sql",
    |compilation, model| {
        // Extension functions (uuid_generate_v4, digest, ll_to_earth) are not
        // stubbed: the extensions provide them.
        assert_eq!(signatures(&compilation), Vec::<String>::new());
        let names: Vec<&str> = model.extensions.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "btree_gist",
                "citext",
                "cube",
                "earthdistance",
                "fuzzystrmatch",
                "hstore",
                "ltree",
                "pg_trgm",
                "pgcrypto",
                "uuid-ossp",
            ]
        );
    }
);

corpus!(
    supabase_style_auth_and_policies,
    "12_supabase_like.sql",
    |compilation, model| {
        assert_eq!(compilation.created_schemas, ["auth"]);
        let profiles = entity(&model, "public", "profiles");
        let public = profiles
            .policies
            .iter()
            .find(|p| p.name.starts_with("Public"))
            .unwrap();
        assert_eq!(public.roles, ["anon", "authenticated"]);
        let buckets = entity(&model, "public", "buckets");
        let admins = buckets
            .policies
            .iter()
            .find(|p| p.name == "Admins manage buckets")
            .unwrap();
        assert_eq!(
            admins.using.as_deref(),
            Some("((auth.jwt() ->> 'role'::text) = 'admin'::text)")
        );
        assert_eq!(
            signatures(&compilation),
            ["auth.uid() → uuid", "auth.jwt() → jsonb"]
        );
        let objects = entity(&model, "public", "objects");
        assert_eq!(
            objects.column("id").unwrap().default.as_deref(),
            Some("extensions.uuid_generate_v4()")
        );
    }
);

corpus!(
    graph_edits_appended_after_the_tables,
    "13_graph_edits.sql",
    |_compilation, model| {
        // Policies keep their roles through table and policy renames.
        let members = entity(&model, "public", "members");
        assert_eq!(members.policies[0].roles, ["app_user"]);
        assert!(members.column("login_email").is_some());
        let posts = entity(&model, "public", "posts");
        assert_eq!(posts.policies[0].name, "posts_by_author");
        assert_eq!(posts.policies[0].roles, ["app_user", "moderator"]);
        assert!(posts.column("draft").is_none());
        assert_eq!(
            posts.foreign_keys().next().unwrap().ref_table.as_deref(),
            Some("members")
        );
        assert!(model.entities.iter().all(|e| e.name != "tmp_import"));
    }
);

corpus!(
    enums_grown_and_used_in_one_text,
    "14_enum_evolution.sql",
    |_compilation, model| {
        let state = model
            .enums
            .iter()
            .find(|e| e.name == "ticket_state")
            .unwrap();
        assert_eq!(state.labels, ["open", "escalated", "pending", "resolved"]);
    }
);

corpus!(
    less_common_types_storage_and_indexes,
    "15_types_and_storage.sql",
    |_compilation, model| {
        let cache = entity(&model, "public", "cache_entries");
        assert!(cache.unlogged);
        let measurements = entity(&model, "public", "measurements");
        assert!(
            measurements
                .indexes
                .iter()
                .any(|i| i.definition.contains("USING brin"))
        );
        assert!(
            measurements
                .constraints
                .iter()
                .any(|c| c.definition.starts_with("UNIQUE NULLS NOT DISTINCT"))
        );
    }
);

/// A version pushed by remus as JSON: a draft starts from its rendered SQL,
/// which names roles the sandbox lacks (`TO showcase_reader`) and was
/// introspected from a real database. It must compile, to the same model.
#[test]
fn a_pushed_version_compiles_back_to_its_model() {
    with_compiler(|compiler| async move {
        let pushed = Schema::from_json(SHOWCASE)
            .expect("the showcase fixture")
            .canonical();
        let sql = render_sql(&pushed);
        let compiled = compile_ok(compiler, &sql).await;
        let mut model = compiled.model.expect("a model");
        // What differs between any two databases: their name, server and
        // extension versions.
        let mut expected = pushed.clone();
        expected.database = model.database.clone();
        expected.server_version = model.server_version.clone();
        for (e, m) in expected.extensions.iter_mut().zip(&model.extensions) {
            e.version.clone_from(&m.version);
        }
        assert_same("showcase", &expected, &model);
        let orders = entity(&model, "shop", "orders");
        let reader = orders
            .policies
            .iter()
            .find(|p| p.name == "orders_no_cancelled")
            .unwrap();
        assert_eq!(reader.roles, ["showcase_reader"]);
        model.database = "kept".to_owned();
    });
}

/// The same text twice is one sandbox run.
#[test]
fn identical_text_is_served_from_the_cache() {
    with_compiler(|compiler| async move {
        let sql = "CREATE TABLE cached_once (id int PRIMARY KEY); -- cache probe";
        let first = compile_ok(compiler, sql).await;
        let second = compile_ok(compiler, sql).await;
        assert!(!first.cached);
        assert!(second.cached);
        assert_eq!(first.model, second.model);
        assert_eq!(first.sha256, second.sha256);
    });
}

/// Statements an extension made redundant are reported, not hidden.
#[test]
fn skipped_extension_objects_are_reported_as_notes() {
    with_compiler(|compiler| async move {
        let sql = "CREATE EXTENSION IF NOT EXISTS postgis;\n\
                   CREATE TABLE public.spatial_ref_sys (srid integer PRIMARY KEY);\n\
                   CREATE TABLE spots (id int, at geometry(Point, 4326));";
        let compiled = compile_ok(compiler, sql).await;
        let note = &compiled.diagnostics[0];
        assert_eq!(note.code, Code::ProvidedByExtension);
        assert_eq!(note.statement, Some(1));
        assert_eq!(note.span.unwrap().from.line, 2);
    });
}
