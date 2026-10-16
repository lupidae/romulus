//! An import planned: the files read as migrations, matched against what
//! the database already imported, each new one analysed (what it skips).

use romulus_sandbox::import::{self, Migration, Plan, SourceFile};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use utoipa::ToSchema;
use uuid::Uuid;

use super::{IgnoredFile, ImportError, ImportFailure, ImportOptions, SkippedStatement};
use crate::{ApiError, ApiResult, compile::Diagnostic, domain::MigrationFormat, error::ErrorBody};

/// New migrations one import adds, at most: each is a model read in the
/// sandbox.
pub const MAX_NEW: usize = 500;
/// Migrations one import replays, already imported ones included.
pub const MAX_REPLAYED: usize = 2_000;

/// Where a migration stands against the database's imports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum MigrationStatus {
    /// Not imported yet: the import adds it.
    New,
    /// Imported already, unchanged: nothing to do.
    Imported,
    /// Imported already, but the file changed since: the import is refused.
    Changed,
    /// New, but sorted before migrations already imported: the history
    /// cannot be rebuilt around it, the import is refused.
    OutOfOrder,
}

/// One migration of the files, in order.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PlannedMigration {
    /// The tool's version (`20240101120000`, `1.2`, `blog.0002_post_slug`),
    /// or the path for plain folders: what later imports match on.
    pub id: String,
    /// The version's message: the file (or folder) name.
    pub name: String,
    pub path: String,
    /// The line of `path` where it starts (exported files hold several).
    pub line: u32,
    pub status: MigrationStatus,
    /// The version it became, when imported already.
    pub version: Option<i32>,
    /// What will not be replayed (new migrations only).
    pub skipped: Vec<SkippedStatement>,
    /// Syntax errors (new migrations only): the import would stop here.
    pub errors: Vec<ImportError>,
}

/// What importing the files would do; nothing run yet.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ImportPreview {
    /// The format the files are read as.
    pub format: MigrationFormat,
    /// The format they look like (`format` unless one was asked for).
    pub detected: MigrationFormat,
    pub migrations: Vec<PlannedMigration>,
    /// Files that are not migrations (down migrations, READMEs, the tool's
    /// own files).
    pub ignored: Vec<IgnoredFile>,
    /// How many versions the import would create.
    pub new: usize,
    /// The number the first of them would get.
    pub first_number: Option<i32>,
    /// The database has versions that were not imported: the import needs
    /// `append=true`, and the imported ones come after them.
    pub needs_append: bool,
    /// Why the import would be refused as it is (`migration_changed`,
    /// `migration_out_of_order`, `too_many_migrations`, `import_failed`
    /// for a syntax error); `null` when it can run.
    pub problem: Option<ErrorBody>,
}

/// A migration the database imported already.
#[derive(Debug, Clone)]
pub struct Existing {
    pub migration_id: String,
    pub checksum: Vec<u8>,
    pub number: i32,
    /// Its up SQL, as replayed then.
    pub source: String,
}

/// An import, planned.
pub struct Planned {
    pub preview: ImportPreview,
    pub format: MigrationFormat,
    pub migrations: Vec<Migration>,
    /// What the database imported, in version order: replayed first.
    pub history: Vec<Existing>,
    /// The new migrations, by index into `migrations`.
    pub new: Vec<usize>,
    /// The latest version (any origin): number and model.
    pub latest: Option<(i32, serde_json::Value)>,
}

pub fn checksum(sql: &str) -> Vec<u8> {
    Sha256::digest(sql.as_bytes()).to_vec()
}

/// A diagnostic in a migration's text, located in its file.
pub fn located(d: &Diagnostic, migration: &Migration) -> ImportError {
    let shift = migration.line.saturating_sub(1);
    ImportError {
        code: d.code,
        message: d.message.clone(),
        detail: d.detail.clone(),
        hint: d.hint.clone(),
        sqlstate: d.sqlstate.clone(),
        line: d.span.map(|s| s.from.line + shift),
        column: d.span.map(|s| s.from.column),
        end_line: d.span.map(|s| s.to.line + shift),
        end_column: d.span.map(|s| s.to.column),
    }
}

/// A skipped statement of a migration, located in its file.
pub fn skipped_in_file(s: &import::Skipped, migration: &Migration) -> SkippedStatement {
    SkippedStatement {
        line: s.line + migration.line.saturating_sub(1),
        kind: s.kind.clone(),
        reason: s.reason,
        text: s.text.clone(),
    }
}

/// Read `files` as migrations and match them with the database's imports.
pub async fn plan(
    db: &PgPool,
    database_id: Uuid,
    files: Vec<SourceFile>,
    options: &ImportOptions,
) -> ApiResult<Planned> {
    let format = options.format.map(Into::into);
    let planned = tokio::task::spawn_blocking(move || import::plan(files, format))
        .await
        .map_err(ApiError::internal)?
        .map_err(|err| ApiError::Unprocessable {
            code: "invalid_migrations",
            message: match &err {
                import::PlanError::Empty(why) => format!("No migration found: {why}"),
                other => format!("These files cannot be imported: {other}."),
            },
            details: Some(json!({ "error": err.to_string() })),
        })?;

    let history = sqlx::query_as!(
        Existing,
        r#"SELECT m.migration_id, m.checksum, v.number, v.source
           FROM version_migrations m JOIN versions v ON v.id = m.version_id
           WHERE m.database_id = $1 ORDER BY v.number"#,
        database_id,
    )
    .fetch_all(db)
    .await?;
    let others = sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM versions WHERE database_id = $1 AND origin <> 'import') AS "exists!""#,
        database_id,
    )
    .fetch_one(db)
    .await?;
    let latest = sqlx::query!(
        "SELECT number, model FROM versions WHERE database_id = $1 ORDER BY number DESC LIMIT 1",
        database_id,
    )
    .fetch_optional(db)
    .await?
    .map(|row| (row.number, row.model));

    // Analysing hundreds of migrations: off the async workers.
    let latest_number = latest.as_ref().map(|(n, _)| *n);
    let planned =
        tokio::task::spawn_blocking(move || build(planned, history, others, latest_number))
            .await
            .map_err(ApiError::internal)?;
    Ok(Planned { latest, ..planned })
}

fn build(plan: Plan, history: Vec<Existing>, others: bool, latest: Option<i32>) -> Planned {
    let Plan {
        format,
        detected,
        migrations,
        ignored,
    } = plan;
    let mut planned: Vec<PlannedMigration> = Vec::with_capacity(migrations.len());
    let mut new = Vec::new();
    let last_imported = migrations
        .iter()
        .rposition(|m| history.iter().any(|e| e.migration_id == m.id));
    for (k, migration) in migrations.iter().enumerate() {
        let existing = history.iter().find(|e| e.migration_id == migration.id);
        let (status, version) = match existing {
            Some(e) if e.checksum == checksum(&migration.sql) => {
                (MigrationStatus::Imported, Some(e.number))
            }
            Some(e) => (MigrationStatus::Changed, Some(e.number)),
            None if last_imported.is_some_and(|last| k < last) => {
                (MigrationStatus::OutOfOrder, None)
            }
            None => (MigrationStatus::New, None),
        };
        let (skipped, errors) =
            if matches!(status, MigrationStatus::New | MigrationStatus::OutOfOrder) {
                let analysis = import::analyze(&migration.sql);
                (
                    analysis
                        .skipped
                        .iter()
                        .map(|s| skipped_in_file(s, migration))
                        .collect(),
                    analysis
                        .errors
                        .iter()
                        .map(|d| located(d, migration))
                        .collect(),
                )
            } else {
                (Vec::new(), Vec::new())
            };
        if status == MigrationStatus::New {
            new.push(k);
        }
        planned.push(PlannedMigration {
            id: migration.id.clone(),
            name: migration.name.clone(),
            path: migration.path.clone(),
            line: migration.line,
            status,
            version,
            skipped,
            errors,
        });
    }
    let mut preview = ImportPreview {
        format: format.into(),
        detected: detected.into(),
        new: new.len(),
        first_number: (!new.is_empty()).then(|| latest.map_or(1, |n| n + 1)),
        needs_append: others && !new.is_empty(),
        migrations: planned,
        ignored: ignored.into_iter().map(Into::into).collect(),
        problem: None,
    };
    let replayed = history.len() + new.len();
    preview.problem = problem(&preview, replayed).map(|err| err.body());
    Planned {
        format: format.into(),
        preview,
        migrations,
        history,
        new,
        latest: None,
    }
}

/// Why the planned import cannot run (whatever `append` says).
fn problem(preview: &ImportPreview, replayed: usize) -> Option<ApiError> {
    let migrations = &preview.migrations;
    if let Some(changed) = migrations
        .iter()
        .find(|m| m.status == MigrationStatus::Changed)
    {
        return Some(ApiError::Conflict {
            code: "migration_changed",
            message: format!(
                "{} changed since it was imported as v{}. A migration must not change once applied: restore it, or add a new migration.",
                changed.path,
                changed.version.unwrap_or_default()
            ),
        });
    }
    if let Some(late) = migrations
        .iter()
        .find(|m| m.status == MigrationStatus::OutOfOrder)
    {
        return Some(ApiError::Conflict {
            code: "migration_out_of_order",
            message: format!(
                "{} sorts before migrations already imported: the history cannot be rebuilt around it. Give it a later version.",
                late.path
            ),
        });
    }
    if preview.new > MAX_NEW || replayed > MAX_REPLAYED {
        return Some(ApiError::Unprocessable {
            code: "too_many_migrations",
            message: format!(
                "That is {} new migrations ({replayed} to replay); an import adds at most {MAX_NEW} and replays at most {MAX_REPLAYED}. Import the oldest ones first.",
                preview.new
            ),
            details: None,
        });
    }
    let (index, broken) = migrations
        .iter()
        .enumerate()
        .find(|(_, m)| m.status == MigrationStatus::New && !m.errors.is_empty())?;
    Some(failed(index, broken, broken.errors.clone()))
}

/// 422 `import_failed`: the import stops at this migration.
pub fn failed(index: usize, migration: &PlannedMigration, errors: Vec<ImportError>) -> ApiError {
    let first = errors.first();
    let place = match first.and_then(|e| e.line) {
        Some(line) => format!("{}:{line}", migration.path),
        None => migration.path.clone(),
    };
    let message = format!(
        "{place}: {}. Nothing was imported.",
        first.map_or("it does not replay", |e| e.message.as_str())
    );
    let failure = ImportFailure {
        index,
        id: migration.id.clone(),
        name: migration.name.clone(),
        path: migration.path.clone(),
        errors,
    };
    ApiError::Unprocessable {
        code: "import_failed",
        message,
        details: serde_json::to_value(failure).ok(),
    }
}

/// Why `planned` cannot be imported with `options`; `None` when it can.
pub fn refusal(planned: &Planned, options: &ImportOptions) -> Option<ApiError> {
    if let Some(problem) = problem(&planned.preview, planned.history.len() + planned.new.len()) {
        return Some(problem);
    }
    (planned.preview.needs_append && !options.append).then(|| ApiError::Conflict {
        code: "has_versions",
        message: "This database has versions that were not imported. Import with append=true to add the migrations after them."
            .to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use romulus_sandbox::import::{Format, SourceFile, plan};

    use super::*;

    fn existing(id: &str, sql: &str, number: i32) -> Existing {
        Existing {
            migration_id: id.to_owned(),
            checksum: checksum(sql),
            number,
            source: sql.to_owned(),
        }
    }

    fn planned(files: &[(&str, &str)], history: Vec<Existing>, others: bool) -> Planned {
        let files = files.iter().map(|(p, t)| SourceFile::new(*p, *t)).collect();
        let latest = history.iter().map(|e| e.number).max();
        build(
            plan(files, Some(Format::Sqlx)).unwrap(),
            history,
            others,
            latest,
        )
    }

    fn statuses(planned: &Planned) -> Vec<MigrationStatus> {
        planned
            .preview
            .migrations
            .iter()
            .map(|m| m.status)
            .collect()
    }

    #[test]
    fn imported_migrations_are_matched_by_id_and_checksum() {
        use MigrationStatus::{Changed, Imported, New};
        let history = vec![
            existing("1", "CREATE TABLE a ();", 1),
            existing("2", "CREATE TABLE b ();", 2),
        ];
        let same = planned(
            &[
                ("1_a.sql", "CREATE TABLE a ();"),
                ("2_b.sql", "CREATE TABLE b ();"),
                ("3_c.sql", "CREATE TABLE c ();"),
            ],
            history.clone(),
            false,
        );
        assert_eq!(statuses(&same), [Imported, Imported, New]);
        assert_eq!(same.new, [2]);
        assert_eq!(same.preview.first_number, Some(3));
        assert!(same.preview.problem.is_none());

        let edited = planned(
            &[
                ("1_a.sql", "CREATE TABLE a (id int);"),
                ("2_b.sql", "CREATE TABLE b ();"),
            ],
            history,
            false,
        );
        assert_eq!(statuses(&edited), [Changed, Imported]);
        assert_eq!(edited.preview.problem.unwrap().code, "migration_changed");
    }

    #[test]
    fn a_new_migration_before_imported_ones_is_out_of_order() {
        let history = vec![existing("2", "CREATE TABLE b ();", 1)];
        let late = planned(
            &[
                ("1_a.sql", "CREATE TABLE a ();"),
                ("2_b.sql", "CREATE TABLE b ();"),
            ],
            history,
            false,
        );
        assert_eq!(
            statuses(&late),
            [MigrationStatus::OutOfOrder, MigrationStatus::Imported]
        );
        assert_eq!(late.preview.problem.unwrap().code, "migration_out_of_order");
    }

    #[test]
    fn other_versions_need_append_and_syntax_errors_stop_it() {
        let fresh = planned(&[("1_a.sql", "CREATE TABLE a ();")], Vec::new(), true);
        assert!(fresh.preview.needs_append);
        assert!(fresh.preview.problem.is_none());
        let options = ImportOptions::default();
        assert!(matches!(
            refusal(&fresh, &options),
            Some(ApiError::Conflict {
                code: "has_versions",
                ..
            })
        ));
        let append = ImportOptions {
            append: true,
            ..ImportOptions::default()
        };
        assert!(refusal(&fresh, &append).is_none());

        let broken = planned(
            &[
                ("1_a.sql", "CREATE TABLE a ();"),
                ("2_b.sql", "\nCREATE TABLE b (;"),
            ],
            Vec::new(),
            false,
        );
        let problem = broken.preview.problem.unwrap();
        assert_eq!(problem.code, "import_failed");
        assert!(
            problem.message.starts_with("2_b.sql:2: "),
            "{}",
            problem.message
        );
        assert_eq!(problem.details.unwrap()["index"], 1);
    }
}
