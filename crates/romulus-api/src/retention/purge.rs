//! What expires on its own: deleted databases and invitations nobody
//! accepted, [`DAYS`] days on.
//!
//! A deleted database (`databases.deleted_at`, `DELETE /orgs/{org}/
//! databases/{db}`) has been hidden since, its tokens revoked, its drafts
//! closed and its playgrounds destroyed (`playgrounds::sweeper`); the purge
//! deletes its row and, with it, every version, draft, comment, token and
//! test it held (`ON DELETE CASCADE`: the history triggers let it go once
//! the row is gone). Its audit events stay the organization's history (who
//! published what, with which approvals): their `database_id` becomes NULL,
//! their `database_name` says which database it was, and a last one,
//! `database_purged`, says when it went.

use sqlx::PgPool;
use uuid::Uuid;

use crate::{audit, domain::AuditAction};

/// Days after a database's deletion, or an invitation's expiry, before it
/// is purged.
pub const DAYS: i32 = 30;

/// Purges the databases deleted more than [`DAYS`] days ago, a hundred at
/// most per call, each in its own transaction. How many.
pub async fn databases(db: &PgPool) -> Result<u64, sqlx::Error> {
    let due = sqlx::query_scalar!(
        "SELECT id FROM databases WHERE deleted_at < now() - make_interval(days => $1)
         ORDER BY deleted_at LIMIT 100",
        DAYS,
    )
    .fetch_all(db)
    .await?;
    let mut purged = 0;
    for id in due {
        match purge_database(db, id).await {
            Ok(true) => purged += 1,
            Ok(false) => {}
            Err(err) => {
                tracing::warn!(database = %id, error = %err, "purging a deleted database failed");
            }
        }
    }
    Ok(purged)
}

async fn purge_database(db: &PgPool, id: Uuid) -> Result<bool, sqlx::Error> {
    let mut tx = db.begin().await?;
    // Under the lock versions are created under; still due (deleted rows
    // are never undeleted, but read it again anyway).
    let Some(database) = sqlx::query!(
        r#"SELECT org_id, name, slug, (SELECT count(*) FROM versions v WHERE v.database_id = d.id) AS "versions!"
           FROM databases d WHERE id = $1 AND deleted_at < now() - make_interval(days => $2)
           FOR UPDATE"#,
        id,
        DAYS,
    )
    .fetch_optional(&mut *tx)
    .await?
    else {
        return Ok(false);
    };
    // About the database, before it goes: the purge sets its database_id
    // to NULL like the others', its name stays.
    audit::record(
        &mut *tx,
        &audit::Entry {
            org_id: database.org_id,
            database_id: Some(id),
            action: AuditAction::DatabasePurged,
            actor_id: None,
            actor_name: None,
            version_number: None,
            reason: None,
            details: audit::AuditDetails {
                database_name: Some(database.name),
                database_slug: Some(database.slug.clone()),
                ..Default::default()
            },
        },
    )
    .await?;
    sqlx::query!("DELETE FROM databases WHERE id = $1", id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    tracing::info!(database = %id, slug = %database.slug, versions = database.versions, "deleted database purged");
    Ok(true)
}

/// Deletes the invitations nobody accepted that expired more than [`DAYS`]
/// days ago. How many. (A revoked invitation is deleted when it is revoked;
/// an accepted one is the membership's record and stays.)
pub async fn invitations(db: &PgPool) -> Result<u64, sqlx::Error> {
    let mut purged = 0;
    loop {
        let batch = sqlx::query!(
            "DELETE FROM invitations WHERE id IN (
                 SELECT id FROM invitations
                 WHERE accepted_at IS NULL AND expires_at < now() - make_interval(days => $1)
                 LIMIT 1000
             )",
            DAYS,
        )
        .execute(db)
        .await?
        .rows_affected();
        purged += batch;
        if batch < 1000 {
            break;
        }
    }
    if purged > 0 {
        tracing::info!(invitations = purged, "expired invitations purged");
    }
    Ok(purged)
}
