//! A large import's peak memory: models are staged in the database as the
//! replay reads them, so the API holds a few at a time, never the history.
//! A binary of its own: the allocator counting every byte would measure
//! (and slow) the other tests too.

#[allow(dead_code)]
#[path = "api/harness.rs"]
mod harness;

use std::io::Write;

use axum::{
    body::Body,
    http::{Request, StatusCode, header},
};
use peak_alloc::PeakAlloc;
use romulus_api::csrf;
use sqlx::PgPool;

use crate::harness::{ORIGIN, Team, TestApp, database};

#[global_allocator]
static HEAP: PeakAlloc = PeakAlloc;

const BOUNDARY: &str = "romulus-memory-boundary";

/// `migrations` migrations of `tables` tables each, as a multipart body.
fn history(migrations: usize, tables: usize) -> Vec<u8> {
    let mut body = Vec::new();
    for k in 0..migrations {
        write!(
            body,
            "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"files\"; filename=\"migrations/{:04}_tables.sql\"\r\nContent-Type: application/sql\r\n\r\n",
            k + 1
        )
        .unwrap();
        for t in 0..tables {
            writeln!(body, "CREATE TABLE table_{k}_{t} (id bigint);").unwrap();
        }
        body.extend_from_slice(b"\r\n");
    }
    write!(body, "--{BOUNDARY}--\r\n").unwrap();
    body
}

/// Imports a history into a new database: the heap's growth, and the
/// bytes of models stored.
async fn import(
    app: &TestApp,
    db: &PgPool,
    team: &Team,
    slug: &str,
    body: Vec<u8>,
) -> (usize, usize) {
    let id = database(db, team.org, slug).await;
    let request = Request::post(team.path(&format!("/databases/{slug}/imports")))
        .header(header::ORIGIN, ORIGIN)
        .header(csrf::HEADER, "1")
        .header(header::COOKIE, team.developer.cookie())
        .header(
            header::CONTENT_TYPE,
            format!("multipart/form-data; boundary={BOUNDARY}"),
        )
        .body(Body::from(body))
        .unwrap();
    HEAP.reset_peak_usage();
    let before = HEAP.current_usage();
    let imported = app.send(request).await;
    let growth = HEAP.peak_usage().saturating_sub(before);
    assert_eq!(imported.status, StatusCode::CREATED, "{}", imported.json());
    let stored = sqlx::query_scalar!(
        r#"SELECT sum(octet_length(model::text))::bigint AS "bytes!" FROM versions WHERE database_id = $1"#,
        id
    )
    .fetch_one(db)
    .await
    .unwrap();
    (growth, usize::try_from(stored).unwrap())
}

/// The same 3 000 tables in the end, reached in 20 migrations or in 100:
/// the second history stores five times the models (each version keeps
/// its whole schema), yet takes no more memory to import. Holding the
/// models would take five times as much.
#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn an_import_holds_a_few_models_not_the_history(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    let app = TestApp::compiling(db.clone());
    let (few, few_stored) = import(&app, &db, &team, "few", history(20, 150)).await;
    let (many, many_stored) = import(&app, &db, &team, "many", history(100, 30)).await;
    assert!(
        many_stored > 4 * few_stored,
        "{many_stored} vs {few_stored}"
    );
    assert!(
        many < 2 * few,
        "importing 100 migrations grew the heap by {} MiB, 20 migrations of the same schema by {} MiB",
        many >> 20,
        few >> 20
    );
    assert!(
        many < many_stored / 2,
        "{} MiB of heap for {} MiB of models",
        many >> 20,
        many_stored >> 20
    );
    let staged = sqlx::query_scalar!(r#"SELECT count(*) AS "count!" FROM import_staging"#)
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(staged, 0, "the staging rows are gone");
}
