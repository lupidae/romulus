//! Creating a playground: its row (`building`), then its build on the host,
//! in a task of its own (the person leaving the page does not stop it). The
//! row says where it is (`progress`, in memory, while building), then
//! `ready` with its row counts and the generator's report, or `failed` with
//! why; a failed build leaves nothing on the host.
//!
//! The caps ([`super::MAX_TOTAL`], per organization, database and person,
//! one per draft or version) are checked early, then counted again in the
//! insert's transaction under [`CREATE_LOCK`]: two creates at once cannot
//! both pass a cap. Disk: the quotas of every live playground plus the new
//! one must fit in the volume less [`super::DISK_RESERVE_BYTES`] (counted
//! with the caps, so quotas are never promised twice), and what is free now
//! must hold the new one's quota plus the reserve. A host that cannot say
//! how much disk it has gets no new playground (fail closed).

use romulus_playground::protocol::{
    Build, BuildEvent, Extension, Health, Profile, Size, Stage, TableRows,
};
use serde_json::Value;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::{
    BuildProgress, CreatePlayground, DISK_RESERVE_BYTES, MAJOR, MAX_PER_DATABASE, MAX_PER_ORG,
    MAX_PER_PERSON, MAX_TOTAL, details, find, host::Caller,
};
use crate::{
    ApiError, ApiResult, AppState, audit,
    auth::{Member, min},
    databases::DatabaseRef,
    db,
    domain::{AuditAction, DraftStatus, PlaygroundProfile, PlaygroundSize},
    drafts::versions::{Version, by_id, latest},
};

fn no_version() -> ApiError {
    ApiError::Unprocessable {
        code: "no_version",
        message:
            "This database has no version yet: a playground holds one. Upload or publish one first."
                .into(),
        details: None,
    }
}

fn limit(message: String) -> ApiError {
    ApiError::Conflict {
        code: "playground_limit",
        message,
    }
}

/// The seed from its hex form, or a random one (24 bits: six digits are
/// easy to read out and type).
pub(crate) fn seed(given: Option<&str>) -> ApiResult<i64> {
    match given.map(str::trim) {
        Some(hex) if !hex.is_empty() => {
            let valid = hex.len() <= 15 && hex.bytes().all(|b| b.is_ascii_hexdigit());
            if !valid {
                return Err(ApiError::Validation {
                    message: "The seed is at most 15 hexadecimal digits.".into(),
                    details: Some(serde_json::json!({ "seed": "at most 15 hexadecimal digits" })),
                });
            }
            i64::from_str_radix(hex, 16).map_err(|_| ApiError::BadRequest("seed".into()))
        }
        _ => {
            let mut bytes = [0u8; 3];
            getrandom::fill(&mut bytes)
                .map_err(|e| ApiError::internal(std::io::Error::other(e.to_string())))?;
            Ok(i64::from(bytes[0]) << 16 | i64::from(bytes[1]) << 8 | i64::from(bytes[2]))
        }
    }
}

/// What the playground holds: the draft's base version, the version asked
/// for, or the latest.
async fn source(
    state: &AppState,
    database: &DatabaseRef,
    request: &CreatePlayground,
) -> ApiResult<(Option<(Uuid, String)>, Version)> {
    if let Some(draft_id) = request.draft_id {
        let draft = sqlx::query!(
            r#"SELECT id, name, status AS "status: DraftStatus", base_version_id
               FROM drafts WHERE id = $1 AND database_id = $2"#,
            draft_id,
            database.id,
        )
        .fetch_optional(&state.db)
        .await?
        .filter(|d| d.status != DraftStatus::Deleted)
        .ok_or_else(|| ApiError::not_found("No such draft."))?;
        if draft.status != DraftStatus::Open {
            return Err(ApiError::Conflict {
                code: "draft_not_open",
                message: "This draft was published: open a playground from its version instead."
                    .into(),
            });
        }
        let base = draft.base_version_id.ok_or_else(no_version)?;
        let version = by_id(&state.db, database.id, base)
            .await?
            .ok_or_else(no_version)?;
        return Ok((Some((draft.id, draft.name)), version));
    }
    let version = match request.version {
        Some(number) => {
            let id = sqlx::query_scalar!(
                "SELECT id FROM versions WHERE database_id = $1 AND number = $2 AND removed_at IS NULL",
                database.id,
                number,
            )
            .fetch_optional(&state.db)
            .await?
            .ok_or_else(|| ApiError::not_found(format!("No version {number}.")))?;
            by_id(&state.db, database.id, id).await?
        }
        None => latest(&state.db, database.id).await?,
    };
    Ok((None, version.ok_or_else(no_version)?))
}

/// The advisory lock creates take in their transaction (one at a time, so
/// the counts they check hold until they commit). Global: creates are rare.
pub(crate) const CREATE_LOCK: i64 = 0x726f_6d75_6c75_7301;

fn host_full(message: String) -> ApiError {
    ApiError::Down {
        code: "playground_host_full",
        message,
    }
}

/// The host's playground volume, as it said before the transaction.
pub(crate) struct Room {
    pub(crate) free: u64,
    pub(crate) total: u64,
}

/// What a new playground is checked against.
struct Wanted<'a> {
    member: &'a Member<min::Developer>,
    database_id: Uuid,
    draft_id: Option<Uuid>,
    version_id: Uuid,
    size: Size,
}

/// One live playground per draft (or version), the caps (the host's, the
/// organization's, the database's, the person's), and the disk the live
/// playgrounds' quotas already hold.
async fn check_caps(
    db: impl sqlx::PgExecutor<'_>,
    wanted: &Wanted<'_>,
    room: &Room,
) -> ApiResult<()> {
    // Migration tests' playgrounds take room on the host too (not a
    // place among the organization's playgrounds: they go within minutes).
    let counts = sqlx::query!(
        r#"WITH tests AS (
               SELECT size FROM migration_tests WHERE status = 'running' AND playground_id IS NOT NULL
           )
           SELECT count(*) + (SELECT count(*) FROM tests) AS "total!",
                  count(*) FILTER (WHERE org_id = $1) AS "org!",
                  count(*) FILTER (WHERE database_id = $2) AS "database!",
                  count(*) FILTER (WHERE created_by = $3) AS "person!",
                  count(*) FILTER (WHERE (draft_id IS NOT NULL AND draft_id = $4)
                                      OR (draft_id IS NULL AND $4::uuid IS NULL AND version_id = $5)) AS "same!",
                  count(*) FILTER (WHERE size = 'small')
                      + (SELECT count(*) FROM tests WHERE size = 'small') AS "small!",
                  count(*) FILTER (WHERE size = 'medium')
                      + (SELECT count(*) FROM tests WHERE size = 'medium') AS "medium!"
           FROM playgrounds WHERE status IN ('building', 'ready')"#,
        wanted.member.org.id,
        wanted.database_id,
        wanted.member.user.id,
        wanted.draft_id,
        wanted.version_id,
    )
    .fetch_one(db)
    .await?;
    if counts.same > 0 {
        return Err(exists());
    }
    if counts.total >= MAX_TOTAL {
        return Err(host_full(
            "The playground host holds as many playgrounds as it can. Try again when some are deleted or expire."
                .into(),
        ));
    }
    if counts.org >= MAX_PER_ORG {
        return Err(limit(format!(
            "Your organization has {MAX_PER_ORG} playgrounds, the most it can have. Delete one first."
        )));
    }
    if counts.database >= MAX_PER_DATABASE {
        return Err(limit(format!(
            "This database has {MAX_PER_DATABASE} playgrounds, the most it can have. Delete one first."
        )));
    }
    if counts.person >= MAX_PER_PERSON {
        return Err(limit(format!(
            "You made {MAX_PER_PERSON} playgrounds that are still here, the most one person can have. Delete one first."
        )));
    }
    let quota = |n: i64, size: Size| u64::try_from(n).unwrap_or(0) * size.disk_bytes();
    let promised = quota(counts.small, Size::Small) + quota(counts.medium, Size::Medium);
    let usable = room.total.saturating_sub(DISK_RESERVE_BYTES);
    if promised + wanted.size.disk_bytes() > usable
        || room.free < wanted.size.disk_bytes() + DISK_RESERVE_BYTES
    {
        return Err(host_full(
            "The playground host has no disk left for another playground right now. Delete one you no longer use, or try again later."
                .into(),
        ));
    }
    Ok(())
}

/// The host's volume: free and total bytes. A host that cannot say gets no
/// new playground (its disk is shared with the app).
pub(crate) async fn room(state: &AppState) -> ApiResult<Room> {
    let backend = state.playgrounds.host.backend()?;
    let health = tokio::time::timeout(std::time::Duration::from_secs(5), backend.health()).await;
    match health {
        Ok(Ok(Health {
            free_bytes: Some(free),
            total_bytes: Some(total),
            ..
        })) => Ok(Room { free, total }),
        other => {
            tracing::warn!(answer = ?other.map(|h| h.map(|h| (h.free_bytes, h.total_bytes))), "the playground host did not say its disk space");
            Err(ApiError::Down {
                code: "host_unavailable",
                message: "The playground host cannot say how much disk it has left, so no playground can be made right now. Try again in a moment.".into(),
            })
        }
    }
}

/// Playgrounds run Postgres [`MAJOR`] (their image): another major's
/// migration would show 17's locks and rewrites.
pub(crate) fn check_major(database: &DatabaseRef) -> ApiResult<()> {
    if database.pg_major == MAJOR {
        return Ok(());
    }
    Err(ApiError::Unprocessable {
        code: "major_unsupported",
        message: format!(
            "Playgrounds run Postgres {MAJOR} for now, and this database targets Postgres {}.",
            database.pg_major
        ),
        details: None,
    })
}

/// Checks the caps, stores the playground, starts its build: its id.
pub(super) async fn create(
    state: &AppState,
    member: &Member<min::Developer>,
    database: &DatabaseRef,
    request: CreatePlayground,
) -> ApiResult<Uuid> {
    state.limits.playgrounds.check(member.user.id)?;
    check_major(database)?;
    // Off: say so before anything is stored.
    state.playgrounds.host.backend()?;
    let (draft, version) = source(state, database, &request).await?;
    let draft_id = draft.as_ref().map(|(id, _)| *id);
    let size = request.size.unwrap_or(PlaygroundSize::Small);
    let host_size = match size {
        PlaygroundSize::Small => Size::Small,
        PlaygroundSize::Medium => Size::Medium,
    };

    let wanted = Wanted {
        member,
        database_id: database.id,
        draft_id,
        version_id: version.id,
        size: host_size,
    };
    // Early, before the compile: the common refusals cost nothing.
    let room = room(state).await?;
    check_caps(&state.db, &wanted, &room).await?;

    let schema = version.schema()?;
    let sql = version.sql()?;
    let script = state
        .compiler
        .playground_script(MAJOR, &sql)
        .await?
        .ok_or_else(|| ApiError::Unprocessable {
            code: "compile_failed",
            message: format!(
                "v{}'s SQL no longer compiles here, so no playground can hold it.",
                version.number
            ),
            details: None,
        })?;
    let seed = seed(request.seed.as_deref())?;
    let model_sha256 =
        Sha256::digest(serde_json::to_vec(&version.model).unwrap_or_default()).to_vec();

    let mut tx = state.db.begin().await?;
    // Counted again, one create at a time, until this one commits.
    sqlx::query!(
        r#"SELECT 1 AS "locked!" FROM pg_advisory_xact_lock($1)"#,
        CREATE_LOCK
    )
    .fetch_one(&mut *tx)
    .await?;
    check_caps(&mut *tx, &wanted, &room).await?;
    let inserted = sqlx::query_scalar!(
        "INSERT INTO playgrounds (org_id, database_id, draft_id, version_id, model_sha256, size, profile, seed, created_by)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) RETURNING id",
        member.org.id,
        database.id,
        draft_id,
        version.id,
        model_sha256,
        size as PlaygroundSize,
        request.profile as PlaygroundProfile,
        seed,
        member.user.id,
    )
    .fetch_one(&mut *tx)
    .await;
    let id = match inserted {
        Ok(id) => id,
        // Someone created one for the same draft or version meanwhile.
        Err(err) if db::is_unique_violation(&err) => return Err(exists()),
        Err(err) => return Err(err.into()),
    };
    let row = find(&mut *tx, database, id).await?;
    audit::record(
        &mut *tx,
        &audit::Entry {
            org_id: member.org.id,
            database_id: Some(database.id),
            action: AuditAction::PlaygroundCreated,
            actor_id: Some(member.user.id),
            actor_name: Some(member.user.name.clone()),
            version_number: Some(version.number),
            reason: None,
            details: details(&row),
        },
    )
    .await?;
    tx.commit().await?;

    let build = Build {
        size: host_size,
        profile: match request.profile {
            PlaygroundProfile::Realistic => Profile::Realistic,
            PlaygroundProfile::EdgeCases => Profile::EdgeCases,
        },
        seed: u64::try_from(seed).unwrap_or(0),
        schema_sql: script,
        extensions: extensions(&schema),
        model: version.model.clone(),
    };
    let tables = tables(&schema);
    tokio::spawn(job(state.clone(), id, row.caller(member), build, tables));
    Ok(id)
}

/// The extensions the model installs, as the host takes them.
pub(crate) fn extensions(schema: &remus_core::Schema) -> Vec<Extension> {
    schema
        .extensions
        .iter()
        .map(|e| Extension {
            name: e.name.clone(),
            schema: e.schema.clone(),
        })
        .collect()
}

/// The tables a build fills (what its progress counts).
fn tables(schema: &remus_core::Schema) -> usize {
    schema
        .entities
        .iter()
        .filter(|e| {
            matches!(
                e.kind,
                remus_core::model::EntityKind::Table
                    | remus_core::model::EntityKind::PartitionedTable
            )
        })
        .count()
}

fn exists() -> ApiError {
    ApiError::Conflict {
        code: "playground_exists",
        message: "A playground exists for this already: open it.".into(),
    }
}

/// The build, on the host, until it is ready or failed.
async fn job(state: AppState, id: Uuid, caller: Caller, build: Build, tables: usize) {
    let playgrounds = &state.playgrounds;
    let mut progress = BuildProgress {
        stage: Stage::Starting,
        tables_done: 0,
        tables_total: tables,
        table: None,
    };
    playgrounds.set_progress(id, Some(progress.clone()));
    let outcome = match playgrounds.host.backend() {
        Ok(backend) => match backend.build(id, caller, build).await {
            Ok(mut events) => {
                let mut outcome = Err("The playground host stopped during the build.".to_owned());
                while let Some(event) = events.recv().await {
                    match event {
                        BuildEvent::Stage { stage } => progress.stage = stage,
                        BuildEvent::Table {
                            schema,
                            table,
                            done,
                            total,
                            ..
                        } => {
                            progress.tables_done = done;
                            progress.tables_total = total.max(done);
                            progress.table = Some(format!("{schema}.{table}"));
                        }
                        BuildEvent::Ready {
                            tables,
                            report,
                            generator,
                            ..
                        } => {
                            outcome = Ok((tables, report, generator));
                            break;
                        }
                        BuildEvent::Failed { message, .. } => {
                            outcome = Err(message);
                            break;
                        }
                    }
                    playgrounds.set_progress(id, Some(progress.clone()));
                }
                outcome
            }
            Err(err) => Err(match err {
                super::host::HostError::Refused { message, .. } => message,
                other => other.to_string(),
            }),
        },
        Err(err) => Err(err.to_string()),
    };
    playgrounds.set_progress(id, None);
    if let Err(err) = record(&state, id, outcome).await {
        tracing::error!(playground = %id, error = %err, "cannot record a build's outcome");
    }
}

async fn record(
    state: &AppState,
    id: Uuid,
    outcome: Result<(Vec<TableRows>, Value, String), String>,
) -> Result<(), sqlx::Error> {
    match outcome {
        Ok((tables, report, generator)) => {
            let updated = sqlx::query!(
                "UPDATE playgrounds SET status = 'ready', ready_at = now(), row_counts = $2,
                        report = $3, generator_version = $4
                 WHERE id = $1 AND status = 'building'",
                id,
                serde_json::to_value(&tables).unwrap_or_default(),
                report,
                generator.chars().take(100).collect::<String>(),
            )
            .execute(&state.db)
            .await?;
            if updated.rows_affected() == 0 {
                // Deleted while it was building: nothing must stay behind.
                if let Ok(backend) = state.playgrounds.host.backend() {
                    let caller = Caller {
                        org: Uuid::nil(),
                        user: None,
                        access: romulus_playground::protocol::Access::Develop,
                    };
                    let _ = backend.destroy(id, caller).await;
                }
            }
        }
        Err(message) => {
            tracing::warn!(playground = %id, %message, "playground build failed");
            sqlx::query!(
                "UPDATE playgrounds SET status = 'failed', failure = $2 WHERE id = $1 AND status = 'building'",
                id,
                message.chars().take(2000).collect::<String>(),
            )
            .execute(&state.db)
            .await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seeds_are_hex_or_random_and_short() {
        assert_eq!(seed(Some("7f3a91")).unwrap(), 0x7f_3a91);
        assert_eq!(seed(Some(" ff ")).unwrap(), 255);
        assert!(seed(Some("xyz")).is_err());
        assert!(seed(Some("1234567890abcdef")).is_err());
        let random = seed(None).unwrap();
        assert!((0..=0xff_ffff).contains(&random));
    }
}
