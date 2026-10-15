use std::path::PathBuf;

use axum::http::{StatusCode, header};
use romulus_api::Config;
use sqlx::PgPool;

use crate::harness::TestApp;

/// A build shaped like Vite's: the page, a hashed asset, a public file.
fn build() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "romulus-web-{}-{}",
        std::process::id(),
        getrandom::u64().expect("random")
    ));
    std::fs::create_dir_all(dir.join("assets")).unwrap();
    std::fs::write(
        dir.join("index.html"),
        "<!doctype html><title>romulus</title>",
    )
    .unwrap();
    std::fs::write(dir.join("assets/index-Ab12Cd34.js"), "console.log(1)").unwrap();
    std::fs::write(dir.join("favicon.svg"), "<svg/>").unwrap();
    dir
}

fn serving(db: PgPool, dir: &std::path::Path) -> TestApp {
    let mut config = Config::for_tests();
    config.web_dir = Some(dir.to_owned());
    TestApp::with_config(db, config)
}

fn header(response: &crate::harness::TestResponse, name: header::HeaderName) -> &str {
    response.headers[name].to_str().unwrap()
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn the_app_root_redirects_to_its_base(db: PgPool) {
    let dir = build();
    let response = serving(db, &dir).get("/app").await;
    assert_eq!(response.status, StatusCode::PERMANENT_REDIRECT);
    assert_eq!(response.location(), "/app/");
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn routes_answer_the_page_uncached(db: PgPool) {
    let dir = build();
    let app = serving(db, &dir);
    for path in [
        "/app/",
        "/app/acme/shop/history",
        "/app/index.html",
        "/app/assets",
    ] {
        let response = app.get(path).await;
        assert_eq!(response.status, StatusCode::OK, "{path}");
        assert!(
            header(&response, header::CONTENT_TYPE).starts_with("text/html"),
            "{path}"
        );
        assert_eq!(
            header(&response, header::CACHE_CONTROL),
            "no-cache",
            "{path}"
        );
        assert!(response.body.starts_with(b"<!doctype html>"), "{path}");
        assert_eq!(header(&response, header::X_CONTENT_TYPE_OPTIONS), "nosniff");
    }
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn hashed_assets_are_immutable(db: PgPool) {
    let dir = build();
    let response = serving(db, &dir).get("/app/assets/index-Ab12Cd34.js").await;
    assert_eq!(response.status, StatusCode::OK);
    assert!(header(&response, header::CONTENT_TYPE).contains("javascript"));
    assert_eq!(
        header(&response, header::CACHE_CONTROL),
        "public, max-age=31536000, immutable"
    );
    assert_eq!(&response.body[..], b"console.log(1)");
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_missing_asset_is_a_404_not_the_page(db: PgPool) {
    let dir = build();
    let response = serving(db, &dir).get("/app/assets/index-Old00000.js").await;
    assert_eq!(response.status, StatusCode::NOT_FOUND);
    assert_eq!(response.code(), "not_found");
    assert!(!response.headers.contains_key(header::CACHE_CONTROL));
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn other_files_are_cached_briefly(db: PgPool) {
    let dir = build();
    let response = serving(db, &dir).get("/app/favicon.svg").await;
    assert_eq!(response.status, StatusCode::OK);
    assert_eq!(header(&response, header::CONTENT_TYPE), "image/svg+xml");
    assert_eq!(
        header(&response, header::CACHE_CONTROL),
        "public, max-age=3600"
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn nothing_outside_the_build_is_served(db: PgPool) {
    let dir = build();
    std::fs::write(dir.parent().unwrap().join("secret.txt"), "no").unwrap();
    let app = serving(db, &dir);
    for path in [
        "/app/../secret.txt",
        "/app/%2e%2e/secret.txt",
        "/app/assets/..%2f..%2fsecret.txt",
    ] {
        let response = app.get(path).await;
        assert_ne!(&response.body[..], b"no", "{path}");
    }
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn the_api_still_answers_beside_the_app(db: PgPool) {
    let dir = build();
    let app = serving(db, &dir);
    assert_eq!(app.get("/api/health").await.status, StatusCode::OK);
    let response = app.get("/api/nope").await;
    assert_eq!(response.status, StatusCode::NOT_FOUND);
    assert_eq!(response.code(), "not_found");
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn without_a_build_the_app_is_not_served(db: PgPool) {
    let response = TestApp::new(db).get("/app/").await;
    assert_eq!(response.status, StatusCode::NOT_FOUND);
    assert_eq!(response.code(), "not_found");
}
