//! The fixture histories in tests/migrations, one per format: each is
//! planned, replayed migration by migration, and checked against the same
//! migrations replayed as one text.

use std::path::Path;

use remus_core::Schema;
use romulus_sandbox::import::{Format, IgnoreReason, ReadOnly, SkipReason, SourceFile, Tool, plan};

use crate::{import::replay, with_compiler};

/// Every file under `tests/migrations/<name>`, paths relative to it.
fn load(name: &str) -> Vec<SourceFile> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/migrations")
        .join(name);
    let mut files = Vec::new();
    let mut dirs = vec![root.clone()];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(&dir).expect("a fixture folder") {
            let path = entry.expect("an entry").path();
            if path.is_dir() {
                dirs.push(path);
            } else {
                let relative = path
                    .strip_prefix(&root)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned();
                files.push(SourceFile::new(
                    relative,
                    std::fs::read_to_string(&path).unwrap(),
                ));
            }
        }
    }
    files
}

fn json(model: &Schema) -> serde_json::Value {
    serde_json::to_value(model).unwrap()
}

/// Plan and replay a fixture; check what holds for every format.
async fn import(compiler: &romulus_sandbox::Compiler, name: &str, format: Format) -> Imported {
    let plan = plan(load(name), None).unwrap_or_else(|e| panic!("{name}: {e}"));
    assert_eq!(plan.format, format, "{name}: detected format");
    let texts: Vec<&str> = plan.migrations.iter().map(|m| m.sql.as_str()).collect();
    let (stepped, models) = replay(compiler, &texts, 0).await;
    if let Some(failure) = &stepped.failure {
        let migration = &plan.migrations[failure.index];
        panic!(
            "{name}: {} ({}) failed: {:#?}",
            migration.name, migration.path, failure.diagnostics
        );
    }
    assert_eq!(
        models.len(),
        plan.migrations.len(),
        "{name}: one model per migration"
    );
    assert!(
        models.iter().enumerate().all(|(k, (i, _))| k == *i),
        "{name}: models in order"
    );

    // The history in one go ends where the migrations one by one do.
    let whole = texts.join("\n;\n");
    let (single, last) = replay(compiler, &[&whole], 0).await;
    assert!(single.failure.is_none(), "{name}: {:?}", single.failure);
    assert_eq!(
        json(&last[0].1),
        json(&models.last().unwrap().1),
        "{name}: the last model is the whole history's"
    );
    Imported {
        plan,
        skipped: stepped.skipped,
        models: models.into_iter().map(|(_, m)| m).collect(),
    }
}

struct Imported {
    plan: romulus_sandbox::import::Plan,
    skipped: Vec<Vec<romulus_sandbox::import::Skipped>>,
    models: Vec<Schema>,
}

impl Imported {
    fn names(&self) -> Vec<&str> {
        self.plan
            .migrations
            .iter()
            .map(|m| m.name.as_str())
            .collect()
    }

    fn final_tables(&self) -> Vec<String> {
        crate::import::tables(self.models.last().unwrap())
    }

    /// (migration name, kind, reason) of every skipped statement.
    fn skipped(&self) -> Vec<(&str, &str, SkipReason)> {
        self.skipped
            .iter()
            .zip(&self.plan.migrations)
            .flat_map(|(list, m)| {
                list.iter()
                    .map(move |s| (m.name.as_str(), s.kind.as_str(), s.reason))
            })
            .collect()
    }
}

const CANONICAL_TABLES: &[&str] = &[
    "public.events",
    "public.issues",
    "public.open_issues",
    "public.organizations",
    "public.projects",
    "public.users",
];

/// The canonical history's checks: the tables at the end, the history
/// steps that must show in the models, and what was skipped.
fn check_canonical(imported: &Imported, name: &str) {
    assert_eq!(imported.models.len(), 12, "{name}");
    assert_eq!(imported.final_tables(), CANONICAL_TABLES, "{name}");
    let m = &imported.models;
    // Renamed in the fifth migration.
    assert!(crate::import::tables(&m[3]).contains(&"public.tasks".to_owned()));
    assert!(crate::import::tables(&m[4]).contains(&"public.issues".to_owned()));
    // Partitions hang under their parent.
    assert_eq!(m[8].partitions.len(), 2, "{name}: {:?}", m[8].partitions);
    // The enum gained a value and a name.
    let status = m[7]
        .enums
        .iter()
        .find(|e| e.name == "issue_status")
        .expect("renamed enum");
    assert_eq!(status.labels, ["todo", "doing", "blocked", "done"]);
    // The concurrent indexes are there.
    let issues = m[9].entities.iter().find(|e| e.name == "issues").unwrap();
    assert!(
        issues.indexes.iter().any(|i| i.name == "issues_open_idx"),
        "{name}"
    );
    // The seed and the backfills are reported, nothing else of them ran.
    let data: Vec<_> = imported
        .skipped()
        .into_iter()
        .filter(|(_, _, reason)| *reason == SkipReason::Data)
        .map(|(migration, kind, _)| (migration, kind))
        .collect();
    assert_eq!(data.len(), 4, "{name}: {data:?}");
    assert!(data[0].0.contains("seed"), "{name}: {data:?}");
    assert!(
        imported
            .skipped()
            .iter()
            .any(|(_, kind, r)| *kind == "GRANT" && *r == SkipReason::Privileges),
        "{name}"
    );
}

#[test]
fn every_file_based_format_imports_the_same_history() {
    with_compiler(|compiler| async move {
        let mut finals = Vec::new();
        for (name, format) in [
            ("sqlx", Format::Tool(Tool::Sqlx)),
            ("golang_migrate", Format::Tool(Tool::GolangMigrate)),
            ("dbmate", Format::Tool(Tool::Dbmate)),
            ("node_pg_migrate", Format::Read(ReadOnly::NodePgMigrate)),
            ("flyway", Format::Tool(Tool::Flyway)),
            ("plain", Format::Tool(Tool::Plain)),
        ] {
            let imported = import(compiler, name, format).await;
            check_canonical(&imported, name);
            finals.push((name, json(imported.models.last().unwrap())));
        }
        let (first, model) = &finals[0];
        for (name, other) in &finals[1..] {
            assert_eq!(model, other, "{first} and {name} end on different models");
        }
    });
}

#[test]
fn file_names_are_the_messages_and_tool_files_are_left_out() {
    with_compiler(|compiler| async move {
        let sqlx = import(compiler, "sqlx", Format::Tool(Tool::Sqlx)).await;
        assert_eq!(sqlx.names()[0], "20240105093000_init.sql");
        assert_eq!(sqlx.plan.migrations[0].id, "20240105093000");

        let golang = import(
            compiler,
            "golang_migrate",
            Format::Tool(Tool::GolangMigrate),
        )
        .await;
        assert_eq!(golang.names()[11], "000012_projects_archived_at.up.sql");
        assert_eq!(golang.plan.ignored.len(), 12, "the down files");
        assert!(
            golang
                .plan
                .ignored
                .iter()
                .all(|i| i.reason == IgnoreReason::Down)
        );
        // Their BEGIN and COMMIT are stripped, and said so.
        assert!(
            golang
                .skipped()
                .iter()
                .any(|(_, kind, r)| *kind == "COMMIT" && *r == SkipReason::Transaction)
        );

        let flyway = import(compiler, "flyway", Format::Tool(Tool::Flyway)).await;
        let ids: Vec<&str> = flyway
            .plan
            .migrations
            .iter()
            .map(|m| m.id.as_str())
            .collect();
        assert_eq!(
            ids,
            [
                "1", "1.1", "2", "2.1", "3", "3.1", "4", "4.1", "5", "5.1", "6", "10"
            ]
        );
        let reasons: Vec<IgnoreReason> = flyway.plan.ignored.iter().map(|i| i.reason).collect();
        assert!(reasons.contains(&IgnoreReason::Repeatable));
        assert!(reasons.contains(&IgnoreReason::Down));
        assert!(reasons.contains(&IgnoreReason::NotSql), "the .sql.conf");

        let plain = import(compiler, "plain", Format::Tool(Tool::Plain)).await;
        assert_eq!(plain.names()[9], "10-concurrent-issue-indexes.sql");
        assert_eq!(plain.plan.ignored[0].reason, IgnoreReason::NotSql);
    });
}

#[test]
fn prisma_migrations_import_with_their_hand_edits() {
    with_compiler(|compiler| async move {
        let prisma = import(compiler, "prisma", Format::Tool(Tool::Prisma)).await;
        assert_eq!(prisma.models.len(), 11);
        assert_eq!(prisma.names()[0], "20240105093000_init");
        assert_eq!(prisma.plan.ignored[0].reason, IgnoreReason::Tool);
        assert_eq!(
            prisma.final_tables(),
            [
                "public.Event",
                "public.Issue",
                "public.Organization",
                "public.Project",
                "public.User"
            ]
        );
        let status = prisma.models[10]
            .enums
            .iter()
            .find(|e| e.name == "IssueStatus")
            .unwrap();
        assert_eq!(status.labels, ["TODO", "DOING", "DONE", "BLOCKED"]);
        let data = prisma
            .skipped()
            .into_iter()
            .filter(|(_, _, r)| *r == SkipReason::Data)
            .count();
        assert_eq!(data, 4);
    });
}

#[test]
fn alembic_output_imports_revision_by_revision() {
    with_compiler(|compiler| async move {
        let alembic = import(compiler, "alembic", Format::Read(ReadOnly::Alembic)).await;
        check_canonical(&alembic, "alembic");
        assert_eq!(
            alembic.names()[0],
            "1975ea83b712 create organizations and users"
        );
        assert_eq!(alembic.plan.migrations[1].id, "ae1027a6acf0");
        // Alembic's version table is its own, not the project's.
        assert_eq!(alembic.plan.ignored[0].reason, IgnoreReason::Tool);
        assert!(
            !alembic
                .final_tables()
                .iter()
                .any(|t| t.contains("alembic_version"))
        );
        // The autocommit block around CONCURRENTLY (COMMIT; … BEGIN;) is stripped.
        assert!(
            alembic
                .skipped()
                .iter()
                .any(|(m, kind, _)| m.contains("concurrent") && *kind == "COMMIT")
        );
    });
}

#[test]
fn django_sqlmigrate_output_imports_every_app_in_plan_order() {
    with_compiler(|compiler| async move {
        let django = import(compiler, "django", Format::Read(ReadOnly::Django)).await;
        assert_eq!(django.models.len(), 30);
        assert_eq!(django.names()[0], "contenttypes.0001_initial");
        assert_eq!(django.names()[29], "tracker.0012_project_archived_at");
        let tables = django.final_tables();
        for table in [
            "public.auth_user",
            "public.django_session",
            "public.tracker_issue",
            "public.tracker_event",
        ] {
            assert!(tables.contains(&table.to_owned()), "{table}: {tables:?}");
        }
        // RunPython operations are named in the report.
        let python: Vec<&str> = django
            .skipped()
            .into_iter()
            .filter(|(m, kind, _)| *kind == "RunPython" && m.starts_with("tracker."))
            .map(|(m, _, _)| m)
            .collect();
        assert_eq!(
            python,
            [
                "tracker.0004_seed_acme",
                "tracker.0006_issue_number",
                "tracker.0012_project_archived_at"
            ]
        );
    });
}

#[test]
fn rails_migrations_import_with_their_enum_blocks() {
    with_compiler(|compiler| async move {
        let rails = import(compiler, "rails", Format::Read(ReadOnly::Rails)).await;
        assert_eq!(rails.models.len(), 12);
        assert_eq!(
            rails.names()[0],
            "20240105093000_create_organizations_and_users.rb"
        );
        // create_enum's DO blocks are unwrapped: the enums exist.
        let enums: Vec<&str> = rails.models[2]
            .enums
            .iter()
            .map(|e| e.name.as_str())
            .collect();
        assert_eq!(enums, ["task_status", "user_role"]);
        assert_eq!(
            rails.final_tables(),
            [
                "public.events",
                "public.issues",
                "public.open_issues",
                "public.organizations",
                "public.projects",
                "public.users"
            ]
        );
        // The seed's INSERTs (one through a model, with binds) are reported.
        let data: Vec<_> = rails
            .skipped()
            .into_iter()
            .filter(|(_, _, r)| *r == SkipReason::Data)
            .collect();
        assert_eq!(data.len(), 4, "{data:?}");
    });
}
