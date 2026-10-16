//! Invariants the database enforces itself, whatever code path writes.

use std::time::Duration;

use romulus_api::{ApiError, db, domain::Role};
use sqlx::{PgExecutor, PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::harness::{database, member, org, user};

async fn set_role(
    db: impl PgExecutor<'_>,
    org: Uuid,
    user: Uuid,
    role: Role,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "UPDATE memberships SET role = $3 WHERE org_id = $1 AND user_id = $2",
        org,
        user,
        role as Role,
    )
    .execute(db)
    .await
    .map(drop)
}

async fn remove(db: &PgPool, org: Uuid, user: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "DELETE FROM memberships WHERE org_id = $1 AND user_id = $2",
        org,
        user
    )
    .execute(db)
    .await
    .map(drop)
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn last_admin_cannot_be_demoted(db: PgPool) {
    let (acme, alice, bob) = (
        org(&db, "acme").await,
        user(&db, "Alice").await,
        user(&db, "Bob").await,
    );
    member(&db, acme, alice, Role::Admin).await;
    member(&db, acme, bob, Role::Developer).await;

    let err = set_role(&db, acme, alice, Role::Developer)
        .await
        .unwrap_err();
    assert_eq!(db::constraint(&err), Some(db::ORGANIZATION_NEEDS_AN_ADMIN));
    let api = ApiError::from(err);
    assert_eq!((api.status().as_u16(), api.code()), (409, "last_admin"));
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn last_admin_cannot_be_removed_or_deleted(db: PgPool) {
    let (acme, alice) = (org(&db, "acme").await, user(&db, "Alice").await);
    member(&db, acme, alice, Role::Admin).await;

    let err = remove(&db, acme, alice).await.unwrap_err();
    assert_eq!(db::constraint(&err), Some(db::ORGANIZATION_NEEDS_AN_ADMIN));
    let err = sqlx::query!("DELETE FROM users WHERE id = $1", alice)
        .execute(&db)
        .await
        .unwrap_err();
    assert_eq!(db::constraint(&err), Some(db::ORGANIZATION_NEEDS_AN_ADMIN));
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn an_admin_can_step_down_when_another_remains(db: PgPool) {
    let (acme, alice, bob) = (
        org(&db, "acme").await,
        user(&db, "Alice").await,
        user(&db, "Bob").await,
    );
    member(&db, acme, alice, Role::Admin).await;
    member(&db, acme, bob, Role::Admin).await;

    set_role(&db, acme, alice, Role::Reviewer).await.unwrap();
    remove(&db, acme, alice).await.unwrap();
    let err = set_role(&db, acme, bob, Role::Developer).await.unwrap_err();
    assert_eq!(db::constraint(&err), Some(db::ORGANIZATION_NEEDS_AN_ADMIN));
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn concurrent_demotions_cannot_strand_an_organization(db: PgPool) {
    let (acme, alice, bob) = (
        org(&db, "acme").await,
        user(&db, "Alice").await,
        user(&db, "Bob").await,
    );
    member(&db, acme, alice, Role::Admin).await;
    member(&db, acme, bob, Role::Admin).await;

    // Each transaction alone would see the other admin and succeed.
    let mut first = db.begin().await.unwrap();
    set_role(&mut *first, acme, alice, Role::Developer)
        .await
        .unwrap();
    let second = tokio::spawn({
        let db = db.clone();
        async move {
            let mut second = db.begin().await?;
            set_role(&mut *second, acme, bob, Role::Developer).await?;
            second.commit().await
        }
    });
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        !second.is_finished(),
        "the second demotion waits for the first"
    );
    first.commit().await.unwrap();

    let err = second.await.unwrap().unwrap_err();
    assert_eq!(db::constraint(&err), Some(db::ORGANIZATION_NEEDS_AN_ADMIN));
}

/// A transaction at `isolation` (a `SET TRANSACTION …` statement) whose
/// snapshot is already taken, as an application transaction would have after
/// its first read.
async fn begin_at(db: &PgPool, isolation: &'static str) -> Transaction<'static, Postgres> {
    let mut tx = db.begin().await.unwrap();
    sqlx::query(isolation).execute(&mut *tx).await.unwrap();
    sqlx::query("SELECT count(*) FROM memberships")
        .execute(&mut *tx)
        .await
        .unwrap();
    tx
}

async fn admins(db: &PgPool, org: Uuid) -> i64 {
    sqlx::query_scalar!(
        r#"SELECT count(*) AS "n!" FROM memberships WHERE org_id = $1 AND role = 'admin'"#,
        org
    )
    .fetch_one(db)
    .await
    .unwrap()
}

/// With a fixed snapshot, the second demotion would still count the admin the
/// first one demoted. The trigger writes the organization row, so Postgres
/// aborts the second transaction instead of letting it strand the org.
async fn stale_snapshot_demotion_is_aborted(db: PgPool, isolation: &'static str) {
    let (acme, alice, bob) = (
        org(&db, "acme").await,
        user(&db, "Alice").await,
        user(&db, "Bob").await,
    );
    member(&db, acme, alice, Role::Admin).await;
    member(&db, acme, bob, Role::Admin).await;

    let mut first = begin_at(&db, isolation).await;
    let mut second = begin_at(&db, isolation).await;
    set_role(&mut *first, acme, alice, Role::Developer)
        .await
        .unwrap();
    first.commit().await.unwrap();

    let err = set_role(&mut *second, acme, bob, Role::Developer)
        .await
        .unwrap_err();
    assert!(db::is_serialization_failure(&err), "{err}");
    assert_eq!(ApiError::from(err).code(), "concurrent_change");
    second.rollback().await.unwrap();
    assert_eq!(admins(&db, acme).await, 1);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn repeatable_read_demotions_cannot_strand_an_organization(db: PgPool) {
    stale_snapshot_demotion_is_aborted(db, "SET TRANSACTION ISOLATION LEVEL REPEATABLE READ").await;
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn serializable_demotions_cannot_strand_an_organization(db: PgPool) {
    stale_snapshot_demotion_is_aborted(db, "SET TRANSACTION ISOLATION LEVEL SERIALIZABLE").await;
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn membership_changes_do_not_touch_the_organization_timestamp(db: PgPool) {
    let (acme, alice, bob) = (
        org(&db, "acme").await,
        user(&db, "Alice").await,
        user(&db, "Bob").await,
    );
    member(&db, acme, alice, Role::Admin).await;
    member(&db, acme, bob, Role::Admin).await;
    let before = sqlx::query_scalar!("SELECT updated_at FROM organizations WHERE id = $1", acme)
        .fetch_one(&db)
        .await
        .unwrap();

    set_role(&db, acme, bob, Role::Developer).await.unwrap();
    let after = sqlx::query_scalar!("SELECT updated_at FROM organizations WHERE id = $1", acme)
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(before, after);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn deleting_an_organization_takes_its_last_admin_along(db: PgPool) {
    let (acme, alice) = (org(&db, "acme").await, user(&db, "Alice").await);
    member(&db, acme, alice, Role::Admin).await;

    sqlx::query!("DELETE FROM organizations WHERE id = $1", acme)
        .execute(&db)
        .await
        .unwrap();
    let left = sqlx::query_scalar!(r#"SELECT count(*) AS "n!" FROM memberships"#)
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(left, 0);
}

async fn version(db: &PgPool, database: Uuid, number: i32) -> Result<Uuid, sqlx::Error> {
    sqlx::query_scalar!(
        "INSERT INTO versions (database_id, number, model, source, source_kind, origin)
         VALUES ($1, $2, '{}', 'CREATE TABLE t ();', 'sql', 'upload') RETURNING id",
        database,
        number,
    )
    .fetch_one(db)
    .await
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn versions_are_immutable(db: PgPool) {
    let acme = org(&db, "acme").await;
    let shop = database(&db, acme, "shop").await;
    let v1 = version(&db, shop, 1).await.unwrap();

    let err = sqlx::query!(
        "UPDATE versions SET message = 'rewritten' WHERE id = $1",
        v1
    )
    .execute(&db)
    .await
    .unwrap_err();
    assert_eq!(db::constraint(&err), Some("versions_are_immutable"));
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn versions_go_only_with_their_database(db: PgPool) {
    let acme = org(&db, "acme").await;
    let shop = database(&db, acme, "shop").await;
    let v1 = version(&db, shop, 1).await.unwrap();
    sqlx::query!(
        "INSERT INTO drafts (database_id, name, base_version_id) VALUES ($1, 'Draft from v1', $2)",
        shop,
        v1
    )
    .execute(&db)
    .await
    .unwrap();

    let err = sqlx::query!("DELETE FROM versions WHERE id = $1", v1)
        .execute(&db)
        .await
        .unwrap_err();
    assert_eq!(db::constraint(&err), Some(db::VERSIONS_ARE_PERMANENT));
    assert_eq!(ApiError::from(err).code(), "version_immutable");

    sqlx::query!("DELETE FROM databases WHERE id = $1", shop)
        .execute(&db)
        .await
        .unwrap();
    let blog = database(&db, acme, "blog").await;
    version(&db, blog, 1).await.unwrap();
    sqlx::query!("DELETE FROM organizations WHERE id = $1", acme)
        .execute(&db)
        .await
        .unwrap();
    let left = sqlx::query_scalar!(r#"SELECT count(*) AS "n!" FROM versions"#)
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(left, 0);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn deleting_an_author_keeps_their_versions(db: PgPool) {
    let (acme, alice, bob) = (
        org(&db, "acme").await,
        user(&db, "Alice").await,
        user(&db, "Bob").await,
    );
    let shop = database(&db, acme, "shop").await;
    let v1 = sqlx::query_scalar!(
        "INSERT INTO versions (database_id, number, model, source, source_kind, origin, created_by)
         VALUES ($1, 1, '{}', '', 'sql', 'upload', $2) RETURNING id",
        shop,
        alice,
    )
    .fetch_one(&db)
    .await
    .unwrap();

    let err = sqlx::query!("UPDATE versions SET created_by = $1 WHERE id = $2", bob, v1)
        .execute(&db)
        .await
        .unwrap_err();
    assert_eq!(db::constraint(&err), Some("versions_are_immutable"));

    sqlx::query!("DELETE FROM users WHERE id = $1", alice)
        .execute(&db)
        .await
        .unwrap();
    let author = sqlx::query_scalar!("SELECT created_by FROM versions WHERE id = $1", v1)
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(author, None);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn version_numbers_are_unique_per_database(db: PgPool) {
    let acme = org(&db, "acme").await;
    let (shop, blog) = (
        database(&db, acme, "shop").await,
        database(&db, acme, "blog").await,
    );
    version(&db, shop, 1).await.unwrap();
    version(&db, blog, 1).await.unwrap();
    let err = version(&db, shop, 1).await.unwrap_err();
    assert_eq!(
        db::constraint(&err),
        Some("versions_database_id_number_key")
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn version_sources_match_their_origin(db: PgPool) {
    let acme = org(&db, "acme").await;
    let shop = database(&db, acme, "shop").await;
    let mut number = 0;
    let mut insert = |kind: &'static str, origin: &'static str| {
        number += 1;
        sqlx::query(
            "INSERT INTO versions (database_id, number, model, source, source_kind, origin)
             VALUES ($1, $2, '{}', '{}', $3::version_source_kind, $4::version_origin)",
        )
        .bind(shop)
        .bind(number)
        .bind(kind)
        .bind(origin)
        .execute(&db)
    };
    // remus JSON only arrives by push, a migration only by import, and an
    // import only stores migrations.
    insert("remus_json", "push").await.unwrap();
    insert("migration", "import").await.unwrap();
    insert("sql", "upload").await.unwrap();
    for (kind, origin) in [
        ("remus_json", "upload"),
        ("migration", "push"),
        ("migration", "draft"),
        ("sql", "import"),
    ] {
        let err = insert(kind, origin).await.unwrap_err();
        assert_eq!(
            db::constraint(&err),
            Some("versions_source_matches_origin"),
            "{kind} by {origin}"
        );
    }
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_draft_cannot_base_on_another_databases_version(db: PgPool) {
    let acme = org(&db, "acme").await;
    let (shop, blog) = (
        database(&db, acme, "shop").await,
        database(&db, acme, "blog").await,
    );
    let blog_v1 = version(&db, blog, 1).await.unwrap();

    let err = sqlx::query!(
        "INSERT INTO drafts (database_id, name, base_version_id) VALUES ($1, 'Draft from v1', $2)",
        shop,
        blog_v1,
    )
    .execute(&db)
    .await
    .unwrap_err();
    assert_eq!(db::constraint(&err), Some("drafts_base_version_fkey"));
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_deleted_database_frees_its_slug(db: PgPool) {
    let acme = org(&db, "acme").await;
    let shop = database(&db, acme, "shop").await;
    let err = sqlx::query!(
        "INSERT INTO databases (org_id, name, slug) VALUES ($1, 'Shop', 'shop')",
        acme
    )
    .execute(&db)
    .await
    .unwrap_err();
    assert!(db::is_unique_violation(&err));

    sqlx::query!(
        "UPDATE databases SET deleted_at = now() WHERE id = $1",
        shop
    )
    .execute(&db)
    .await
    .unwrap();
    database(&db, acme, "shop").await;
}
