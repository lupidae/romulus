//! What a migration test runs: the base version (what its playground
//! holds), the target model, the up migration and the down one.
//!
//! - **A draft**, as reviewed (`sha256`, the migration tab's or the
//!   publish preview's): in schema mode the migration romulus-migration
//!   plans from the base version to the draft's model, and the one back; in
//!   migration mode the migration as written, and no down migration unless
//!   one is asked for (`down: generate`: planned from the draft's model back
//!   to the base).
//! - **Two versions** (`from`, `to`): planned both ways, as the diff page's
//!   migration view plans them.
//! - **A version's own migration** (`version`): the one it was written
//!   (migration mode) or imported as, from the live version before it; no
//!   down migration unless one is asked for.
//!
//! Renames are the migration view's: a draft's known ones and its editors'
//! decisions, the versions' stored ones, and `renames` (what is on screen)
//! over them; the down migration reverses the renames the up one applied
//! (`romulus_migration::down`). `concurrently` as the view has it. Planned
//! off the async workers, for the playground's Postgres ([`MAJOR`]).

use std::collections::BTreeMap;

use remus_core::Schema;
use romulus_migration::{Options, RenameChoices};
use romulus_migration_check::Migration;
use serde::Deserialize;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    ApiError, ApiResult, AppState,
    collab::{
        compile::{input_hash, sha256_hex},
        room::Content,
    },
    databases::DatabaseRef,
    domain::{
        DraftMode, DraftStatus, MigrationTestDown, MigrationTestSource, PlaygroundProfile,
        PlaygroundSize, SourceKind,
    },
    drafts::{
        publish::{compiled, hub_error, is_current},
        versions::{Version, by_id},
    },
    playgrounds::MAJOR,
    versions::migration::stored_renames,
};

/// What to test.
#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateMigrationTest {
    /// A draft's migration from its base version, as reviewed: the draft,
    /// and the SHA-256 of what was reviewed (the migration tab's, or the
    /// publish preview's). Or:
    pub draft_id: Option<Uuid>,
    pub sha256: Option<String>,
    /// the migration planned from version `from` to version `to`. Or:
    pub from: Option<i32>,
    pub to: Option<i32>,
    /// the migration version `version` was written or imported as, from
    /// the live version before it.
    pub version: Option<i32>,
    /// `generate`: a down migration for a migration written without one
    /// (planned from its target back to its base). `none`: the up
    /// migration only. By default: the generated one when the up migration
    /// is generated, none when it is written.
    #[serde(default)]
    pub down: Option<DownChoice>,
    /// Renames turned on or off over what is stored, as the migration view
    /// has them on screen (`SchemaMigration.renames[].key` → on or off).
    #[serde(default)]
    pub renames: Option<BTreeMap<String, bool>>,
    /// Build indexes on existing tables `CONCURRENTLY`, as the migration
    /// view has it.
    #[serde(default)]
    pub concurrently: bool,
    /// Test on the rows of this playground (a copy of them: it is never
    /// changed). It must hold the base version, as built (not migrated
    /// rows). Its size, profile and seed are the test's. Otherwise:
    #[serde(default)]
    pub playground: Option<Uuid>,
    /// `small` unless given (fresh generated rows).
    #[serde(default)]
    pub size: Option<PlaygroundSize>,
    #[serde(default)]
    pub profile: PlaygroundProfile,
    /// Hex, at most 15 digits; random when absent.
    #[serde(default)]
    pub seed: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DownChoice {
    Generate,
    None,
}

/// A test, planned.
pub struct Planned {
    pub source: MigrationTestSource,
    pub draft: Option<(Uuid, String)>,
    /// What the playground holds.
    pub base: Version,
    pub to: Option<(Uuid, i32)>,
    pub target: Schema,
    pub base_label: String,
    pub target_label: String,
    pub up: Migration,
    pub up_sql: String,
    pub down: Option<Migration>,
    pub down_sql: Option<String>,
    pub down_origin: MigrationTestDown,
    pub input_sha256: Option<Vec<u8>>,
    /// For a draft: the migration publishing it would run, as
    /// [`gate_key`] says it (what the publish gate compares).
    pub gate_sha256: Option<Vec<u8>>,
}

fn invalid(field: &str, message: &str) -> ApiError {
    ApiError::Validation {
        message: message.to_owned(),
        details: Some(serde_json::json!({ field: message })),
    }
}

fn unprocessable(code: &'static str, message: impl Into<String>) -> ApiError {
    ApiError::Unprocessable {
        code,
        message: message.into(),
        details: None,
    }
}

/// How a test plans: the renames and `concurrently` of the migration view.
#[derive(Clone, Default)]
struct Planning {
    renames: RenameChoices,
    concurrently: bool,
}

/// Up and down, as romulus-migration plans them for the playground's
/// Postgres (the down one reversing the renames the up one applied). The
/// one place a test plans.
async fn generated(
    old: Schema,
    new: Schema,
    planning: Planning,
) -> ApiResult<(Migration, Migration, String, String)> {
    tokio::task::spawn_blocking(move || {
        let options = Options {
            concurrently: planning.concurrently,
            major: Some(MAJOR.number()),
            renames: planning.renames,
        };
        let up = romulus_migration::migration(&old, &new, &options);
        let down = romulus_migration::down(&old, &new, &up);
        let (up_sql, down_sql) = (up.script(), down.script());
        (
            Migration::planned(&up),
            Migration::planned(&down),
            up_sql,
            down_sql,
        )
    })
    .await
    .map_err(ApiError::internal)
}

/// The exact migration publishing a draft would run, as a key: the SHA-256
/// of its base version's id, the up migration and the down one, planned
/// for the playgrounds' Postgres with `decisions` (the draft's renames, or
/// what was on screen) and `concurrently` as the test ran it (the gate
/// accepts either form: how indexes are built is the tool's choice when it
/// runs, but a test is of the statements it ran). In migration mode the up
/// migration is the text as written and the down one generated (reversing
/// its renames). `None`: nothing to run (the schema unchanged, a migration
/// of nothing but comments), so nothing to test.
pub async fn gate_key(
    base: &Version,
    content: &Content,
    decisions: &BTreeMap<String, bool>,
    target: &Schema,
    concurrently: bool,
) -> ApiResult<Option<Vec<u8>>> {
    use sha2::{Digest, Sha256};
    let written = match content.mode {
        DraftMode::Schema => None,
        DraftMode::Migration => {
            let statements =
                Migration::written(&content.migration).map_or(1, |m| m.statements.len());
            if statements == 0 {
                return Ok(None);
            }
            Some(content.migration.clone())
        }
    };
    let renames = content.rename_choices(Some(decisions));
    let (old, new) = (base.schema()?, target.clone());
    let (up_sql, down_sql) = tokio::task::spawn_blocking(move || {
        let options = Options {
            concurrently,
            major: Some(MAJOR.number()),
            renames,
        };
        let up = romulus_migration::migration(&old, &new, &options);
        let down = romulus_migration::down(&old, &new, &up);
        let up_sql = (!up.statements.is_empty()).then(|| up.script());
        (up_sql, down.script())
    })
    .await
    .map_err(ApiError::internal)?;
    let Some(up_sql) = written.or(up_sql) else {
        return Ok(None);
    };
    let mut hasher = Sha256::new();
    hasher.update(b"romulus-test-gate\0");
    hasher.update(base.id.as_bytes());
    for part in [up_sql.as_bytes(), b"\0", down_sql.as_bytes()] {
        hasher.update(part);
    }
    Ok(Some(hasher.finalize().to_vec()))
}

/// The decisions on screen: at most as many as a draft keeps, keys of a
/// sane length.
fn decisions(request: &CreateMigrationTest) -> ApiResult<BTreeMap<String, bool>> {
    let decisions = request.renames.clone().unwrap_or_default();
    if decisions.len() > crate::collab::doc::MAX_RENAME_DECISIONS
        || decisions.keys().any(|k| k.len() > 2048)
    {
        return Err(invalid("renames", "Too many renames, or keys too long."));
    }
    Ok(decisions)
}

/// A migration as written: its statements, or why it does not read.
fn written(text: &str) -> ApiResult<Migration> {
    Migration::written(text).map_err(|error| {
        unprocessable(
            "migration_unreadable",
            format!("The migration does not parse ({error}): fix it to test it."),
        )
    })
}

fn checked(planned: &Planned) -> ApiResult<()> {
    if planned.up.statements.is_empty() {
        return Err(unprocessable(
            "nothing_to_migrate",
            "This changes nothing: there is no migration to test.",
        ));
    }
    let limit = romulus_playground::protocol::limits::MIGRATION_STATEMENTS;
    let longest = planned
        .up
        .statements
        .len()
        .max(planned.down.as_ref().map_or(0, |d| d.statements.len()));
    if longest > limit {
        return Err(unprocessable(
            "migration_too_long",
            format!(
                "The migration has {longest} statements: a test runs at most {limit} each way."
            ),
        ));
    }
    Ok(())
}

/// The live version `number` of `database`.
pub(crate) async fn by_number(
    state: &AppState,
    database: &DatabaseRef,
    number: i32,
) -> ApiResult<Version> {
    let id = sqlx::query_scalar!(
        "SELECT id FROM versions WHERE database_id = $1 AND number = $2 AND removed_at IS NULL",
        database.id,
        number,
    )
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| ApiError::not_found(format!("No version {number}.")))?;
    by_id(&state.db, database.id, id)
        .await?
        .ok_or_else(|| ApiError::not_found(format!("No version {number}.")))
}

/// Plans what `request` asks to test.
pub async fn plan(
    state: &AppState,
    database: &DatabaseRef,
    request: &CreateMigrationTest,
) -> ApiResult<Planned> {
    let planned = match (request.draft_id, request.from, request.to, request.version) {
        (Some(draft), None, None, None) => {
            let sha256 = request.sha256.as_deref().ok_or_else(|| {
                invalid(
                    "sha256",
                    "What was reviewed (sha256) is required for a draft.",
                )
            })?;
            of_draft(state, database, draft, sha256, request).await?
        }
        (None, Some(from), Some(to), None) => {
            of_versions(state, database, from, to, request).await?
        }
        (None, None, None, Some(version)) => of_version(state, database, version, request).await?,
        _ => {
            return Err(invalid(
                "draft_id",
                "Give a draft (draft_id and sha256), two versions (from and to), or one version.",
            ));
        }
    };
    checked(&planned)?;
    Ok(planned)
}

/// An open draft of `database`, as it is, if `sha256` names it: its name
/// and content.
async fn open_draft(
    state: &AppState,
    database: &DatabaseRef,
    draft_id: Uuid,
    sha256: &str,
) -> ApiResult<(String, Content)> {
    let draft = sqlx::query!(
        r#"SELECT name, status AS "status: DraftStatus" FROM drafts
           WHERE id = $1 AND database_id = $2 AND status <> 'deleted'"#,
        draft_id,
        database.id,
    )
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| ApiError::not_found("No such draft."))?;
    if draft.status != DraftStatus::Open {
        return Err(ApiError::Conflict {
            code: "draft_not_open",
            message: "This draft was published: test its version's migration instead.".into(),
        });
    }
    let content = state.collab.content(draft_id).await.map_err(hub_error)?;
    // What was reviewed (the publish preview's), what the migration view
    // fetched, or the live compile's hash of the text on screen (a
    // migration written by hand: its tab has no other).
    if !is_current(&content, sha256) && sha256_hex(content.source()) != sha256 {
        return Err(ApiError::Conflict {
            code: "draft_changed",
            message: "The draft changed since you looked at its migration. Look at it again."
                .into(),
        });
    }
    Ok((draft.name, content))
}

async fn of_draft(
    state: &AppState,
    database: &DatabaseRef,
    draft_id: Uuid,
    sha256: &str,
    request: &CreateMigrationTest,
) -> ApiResult<Planned> {
    let down = request.down;
    let (name, content) = open_draft(state, database, draft_id, sha256).await?;
    let no_base = || {
        unprocessable(
            "no_version",
            "This draft has no version to start from: there are no rows to test on yet.",
        )
    };
    let base_id = content.base_version.ok_or_else(no_base)?;
    let base = by_id(&state.db, database.id, base_id)
        .await?
        .ok_or_else(no_base)?;
    let (model, _) = compiled(&state.collab, &content).await?;
    let target = model.ok_or_else(|| {
        unprocessable(
            "compile_failed",
            "The draft's SQL has errors: fix them to test its migration.",
        )
    })?;
    // Kept as the draft's input (its compile's hash), whichever of the
    // two was given: the publish dialog and the Migration tab then agree
    // on which tests are of the text as it is.
    let input_sha256 = decode_hex(&input_hash(
        content.mode,
        &content.base_sql,
        content.source(),
    ));
    let base_schema = base.schema()?;
    // The draft's decisions, then what is on screen (as its migration tab
    // plans it): in migration mode its text's renames are known, and the
    // generated down reverses them.
    let mut chosen = content.renames.clone();
    chosen.extend(decisions(request)?);
    let planning = Planning {
        renames: content.rename_choices(Some(&chosen)),
        concurrently: request.concurrently,
    };
    let gate_sha256 = gate_key(&base, &content, &chosen, &target, request.concurrently).await?;
    let (up, up_sql, down_migration, down_sql, down_origin) = match content.mode {
        DraftMode::Schema => {
            let (up, back, up_sql, down_sql) =
                generated(base_schema, target.clone(), planning).await?;
            if down == Some(DownChoice::None) {
                (up, up_sql, None, None, MigrationTestDown::None)
            } else {
                (
                    up,
                    up_sql,
                    Some(back),
                    Some(down_sql),
                    MigrationTestDown::Generated,
                )
            }
        }
        DraftMode::Migration => {
            let up = written(&content.migration)?;
            if down == Some(DownChoice::Generate) {
                let (_, back, _, down_sql) =
                    generated(base_schema, target.clone(), planning).await?;
                (
                    up,
                    content.migration.clone(),
                    Some(back),
                    Some(down_sql),
                    MigrationTestDown::Generated,
                )
            } else {
                (
                    up,
                    content.migration.clone(),
                    None,
                    None,
                    MigrationTestDown::None,
                )
            }
        }
    };
    Ok(Planned {
        source: MigrationTestSource::Draft,
        target_label: format!("the draft \"{name}\""),
        draft: Some((draft_id, name)),
        base_label: format!("v{}", base.number),
        base,
        to: None,
        target,
        up,
        up_sql,
        down: down_migration,
        down_sql,
        down_origin,
        input_sha256,
        gate_sha256,
    })
}

async fn of_versions(
    state: &AppState,
    database: &DatabaseRef,
    from: i32,
    to: i32,
    request: &CreateMigrationTest,
) -> ApiResult<Planned> {
    let down = request.down;
    if from == to {
        return Err(invalid(
            "to",
            "Two different versions: a migration from one to the other.",
        ));
    }
    let base = by_number(state, database, from).await?;
    let target_version = by_number(state, database, to).await?;
    let target = target_version.schema()?;
    let mut renames = stored_renames(&state.db, database.id, from, to).await?;
    renames.decisions.extend(decisions(request)?);
    let planning = Planning {
        renames,
        concurrently: request.concurrently,
    };
    let (up, back, up_sql, down_sql) = generated(base.schema()?, target.clone(), planning).await?;
    let (down_migration, down_sql, down_origin) = if down == Some(DownChoice::None) {
        (None, None, MigrationTestDown::None)
    } else {
        (Some(back), Some(down_sql), MigrationTestDown::Generated)
    };
    Ok(Planned {
        source: MigrationTestSource::Versions,
        draft: None,
        base_label: format!("v{from}"),
        target_label: format!("v{to}"),
        base,
        to: Some((target_version.id, to)),
        target,
        up,
        up_sql,
        down: down_migration,
        down_sql,
        down_origin,
        input_sha256: None,
        gate_sha256: None,
    })
}

async fn of_version(
    state: &AppState,
    database: &DatabaseRef,
    number: i32,
    request: &CreateMigrationTest,
) -> ApiResult<Planned> {
    let down = request.down;
    let version = by_number(state, database, number).await?;
    let written_sql = sqlx::query_scalar!(
        "SELECT migration_sql FROM versions WHERE id = $1",
        version.id
    )
    .fetch_one(&state.db)
    .await?;
    let text = match (written_sql, version.source_kind) {
        (Some(sql), _) => sql,
        (None, SourceKind::Migration) => version.source.clone(),
        _ => {
            return Err(unprocessable(
                "no_written_migration",
                format!(
                    "v{number} was not written or imported as a migration: test the migration from the version before it instead."
                ),
            ));
        }
    };
    let previous = sqlx::query_scalar!(
        "SELECT number FROM versions
         WHERE database_id = $1 AND number < $2 AND removed_at IS NULL
         ORDER BY number DESC LIMIT 1",
        database.id,
        number,
    )
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| {
        unprocessable(
            "no_previous_version",
            format!("v{number} is the first version: there is no version before it to test its migration on."),
        )
    })?;
    let base = by_number(state, database, previous).await?;
    let target = version.schema()?;
    let up = written(&text)?;
    let (down_migration, down_sql, down_origin) = if down == Some(DownChoice::Generate) {
        // Back from the renames the version was published with.
        let planning = Planning {
            renames: stored_renames(&state.db, database.id, previous, number).await?,
            concurrently: false,
        };
        let (_, back, _, down_sql) = generated(base.schema()?, target.clone(), planning).await?;
        (Some(back), Some(down_sql), MigrationTestDown::Generated)
    } else {
        (None, None, MigrationTestDown::None)
    };
    Ok(Planned {
        source: MigrationTestSource::Version,
        draft: None,
        base_label: format!("v{previous}"),
        target_label: format!("v{number}"),
        base,
        to: Some((version.id, number)),
        target,
        up,
        up_sql: text,
        down: down_migration,
        down_sql,
        down_origin,
        input_sha256: None,
        gate_sha256: None,
    })
}

/// Bytes of a hex string (`None` when it is not one).
fn decode_hex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(text.get(i..i + 2)?, 16).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_read_from_hex() {
        assert_eq!(decode_hex("00ff10"), Some(vec![0, 255, 16]));
        assert_eq!(decode_hex("0"), None);
        assert_eq!(decode_hex("zz"), None);
    }
}
