//! The hosted service's contract with each person (docs/MVP.md "Legal:
//! terms, checkout, withdrawal"). On only with `ROMULUS_TERMS_VERSION`
//! (`Config::legal`); a self-hosted install has no Terms, and every check
//! here passes.
//!
//! - **Two editions** of the Terms (`TermsEdition`): while romulus sells to
//!   businesses only (`ROMULUS_CONSUMER_SALES` off, the default), the
//!   professional edition, and everyone attests professional use when
//!   accepting it; once consumers may buy, the full edition (withdrawal,
//!   consumer checkout). Each has its pages ([`LegalLinks`]).
//! - **The Terms at the first sign-in** (Terms 3.1, 3.4 b): the app asks
//!   before anything else, and creating an organization or joining one is
//!   refused (403 `terms_required`, [`require_accepted`]) until the person
//!   has accepted a version. A new version (or edition) is asked for again
//!   at the next sign-in (Terms 21.4: consumers must agree to it); until
//!   then the one accepted applies.
//! - **Records** (`legal_acceptances`): who, when, which version, which
//!   language; checkouts add what the buyer stated (`billing::api`). They
//!   outlive the organization and the account, and are purged by [`purge`]:
//!   a checkout's 10 years after (Article L213-1 of the French Consumer
//!   Code), a sign-in's 5 years after the account ends.
//! - **The language** (`users.locale`) of a person's contract and billing
//!   emails: chosen when accepting, changed in account settings.
//! - **Change notices** ([`notice`], `romulus-api legal-notice`).
//! - **The Terms as a PDF** ([`pdf`]), attached to the subscription's
//!   confirmation (a durable medium).
//!
//! Endpoints: `GET /me/terms` (the person's status and the documents),
//! `POST /me/terms` (accept the version in force), `PATCH /me/preferences`
//! (the language). Agents (personal access tokens) may read but never
//! accept on the person's behalf. Tables: `legal_acceptances`,
//! `withdrawals` (`billing::withdrawal`), `legal_notices`, `users.locale`.

pub mod notice;
pub mod pdf;
pub mod words;

use std::time::Duration;

use axum::extract::State;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{PgExecutor, PgPool};
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

use crate::{
    ApiError, ApiResult, AppState,
    auth::CurrentUser,
    config::LegalConfig,
    domain::{AcceptanceKind, Locale, TermsEdition},
    error::ErrorBody,
    extract::Json,
};

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(terms_status, accept_terms))
        .routes(routes!(preferences))
}

/// The pages of one language.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DocumentLinks {
    pub terms: String,
    pub privacy: String,
    /// The Data Processing Agreement (English only).
    pub dpa: String,
}

/// Where the hosted service's legal documents in force are published, in
/// English and in French (the French version prevails for consumers in
/// France).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct LegalLinks {
    /// The Terms in force: their version and edition.
    pub terms_version: String,
    pub edition: TermsEdition,
    /// Consumers may buy (`ROMULUS_CONSUMER_SALES`): the checkout asks
    /// whether a business or a consumer buys. Off: businesses only, and
    /// accepting the Terms includes attesting professional use.
    pub consumer_sales: bool,
    /// The version of the boxes' wording (sign-in and checkout) in force:
    /// what a request sends with what was ticked.
    pub copy_version: String,
    pub en: DocumentLinks,
    pub fr: DocumentLinks,
    /// The seller's address for withdrawals, complaints and contract
    /// questions.
    pub contact: String,
}

/// Where each edition's Terms and Privacy Policy are published
/// (`site/legal`): English, then French. The professional edition, in force
/// while romulus sells to businesses only, has the canonical pages; the
/// full one waits under `/legal/full/` until consumer sales open.
pub const PAGES: [(TermsEdition, [&str; 4]); 2] = [
    (
        TermsEdition::Professional,
        [
            "/legal/terms",
            "/legal/privacy",
            "/fr/legal/conditions",
            "/fr/legal/confidentialite",
        ],
    ),
    (
        TermsEdition::Full,
        [
            "/legal/full/terms",
            "/legal/full/privacy",
            "/fr/legal/full/conditions",
            "/fr/legal/full/confidentialite",
        ],
    ),
];

impl LegalLinks {
    pub fn of(config: &LegalConfig) -> Self {
        let edition = config.edition();
        let (_, [terms_en, privacy_en, terms_fr, privacy_fr]) = PAGES
            .into_iter()
            .find(|(e, _)| *e == edition)
            .unwrap_or(PAGES[0]);
        Self {
            terms_version: config.terms_version.clone(),
            edition,
            consumer_sales: config.consumer_sales,
            copy_version: crate::billing::copy::version(edition).to_owned(),
            en: DocumentLinks {
                terms: config.url(terms_en),
                privacy: config.url(privacy_en),
                dpa: config.url("/legal/dpa"),
            },
            fr: DocumentLinks {
                terms: config.url(terms_fr),
                privacy: config.url(privacy_fr),
                dpa: config.url("/legal/dpa"),
            },
            contact: config.contact.clone(),
        }
    }

    pub fn of_locale(&self, locale: Locale) -> &DocumentLinks {
        match locale {
            Locale::En => &self.en,
            Locale::Fr => &self.fr,
        }
    }
}

/// The signed-in person and the Terms.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct TermsStatus {
    /// This server has Terms (the hosted service); false on a self-hosted
    /// install, where the rest is empty.
    pub required: bool,
    /// The person accepted the version in force (true when there are no
    /// Terms).
    pub accepted: bool,
    /// The latest version the person accepted, and when; `null`: never.
    pub accepted_version: Option<String>,
    pub accepted_at: Option<DateTime<Utc>>,
    /// The language of their contract and billing emails; `null`: never
    /// chosen (the app follows the browser).
    pub locale: Option<Locale>,
    pub documents: Option<LegalLinks>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct AcceptTerms {
    /// The version shown, which must be the one in force.
    pub version: String,
    /// The language the Terms were read in: the contract's and the emails'.
    pub locale: Locale,
    /// The professional edition: the person attests using romulus for their
    /// trade, business or profession (required then).
    #[serde(default)]
    pub professional_use: bool,
    /// The version of the boxes' wording shown (`LegalLinks::copy_version`).
    pub copy_version: String,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct Preferences {
    pub locale: Locale,
}

/// The latest acceptance of a person, any kind.
struct Latest {
    version: String,
    edition: TermsEdition,
    at: DateTime<Utc>,
}

impl Latest {
    fn is_of(&self, legal: &LegalConfig) -> bool {
        self.version == legal.terms_version && self.edition == legal.edition()
    }
}

async fn latest(db: impl PgExecutor<'_>, user: Uuid) -> Result<Option<Latest>, sqlx::Error> {
    sqlx::query_as!(
        Latest,
        r#"SELECT terms_version AS version, edition AS "edition: TermsEdition", accepted_at AS at
           FROM legal_acceptances
           WHERE user_id = $1 ORDER BY accepted_at DESC, id DESC LIMIT 1"#,
        user,
    )
    .fetch_optional(db)
    .await
}

/// The language of `user`'s contract and emails (English when never chosen).
pub async fn locale_of(db: impl PgExecutor<'_>, user: Uuid) -> Result<Locale, sqlx::Error> {
    Ok(sqlx::query_scalar!(
        r#"SELECT locale AS "locale: Locale" FROM users WHERE id = $1"#,
        user
    )
    .fetch_optional(db)
    .await?
    .flatten()
    .unwrap_or_default())
}

async fn status(state: &AppState, user: &CurrentUser) -> ApiResult<TermsStatus> {
    let locale = sqlx::query_scalar!(
        r#"SELECT locale AS "locale: Locale" FROM users WHERE id = $1"#,
        user.id
    )
    .fetch_one(&state.db)
    .await?;
    let Some(legal) = &state.config.legal else {
        return Ok(TermsStatus {
            required: false,
            accepted: true,
            accepted_version: None,
            accepted_at: None,
            locale,
            documents: None,
        });
    };
    let latest = latest(&state.db, user.id).await?;
    Ok(TermsStatus {
        required: true,
        accepted: latest.as_ref().is_some_and(|l| l.is_of(legal)),
        accepted_version: latest.as_ref().map(|l| l.version.clone()),
        accepted_at: latest.map(|l| l.at),
        locale,
        documents: Some(LegalLinks::of(legal)),
    })
}

/// Refuses what an agent may not do in the person's name.
fn in_person(user: &CurrentUser) -> ApiResult<()> {
    if user.via.is_some() {
        return Err(ApiError::Denied {
            code: "not_for_agents",
            message: "Only the person, signed in, can do this; an access token cannot.".into(),
        });
    }
    Ok(())
}

/// The signed-in person's acceptance of the Terms, and where the legal
/// documents are.
#[utoipa::path(
    get,
    path = "/me/terms",
    operation_id = "terms_status",
    tag = "legal",
    responses(
        (status = 200, body = TermsStatus),
        (status = 401, body = ErrorBody),
    )
)]
async fn terms_status(
    user: CurrentUser,
    State(state): State<AppState>,
) -> ApiResult<Json<TermsStatus>> {
    Ok(Json(status(&state, &user).await?))
}

/// Accepts the Terms in force (the box at sign-in), in the language they
/// were read in, which becomes the person's. Recorded every time a version
/// is first accepted; accepting the same version again only changes the
/// language.
#[utoipa::path(
    post,
    path = "/me/terms",
    operation_id = "accept_terms",
    tag = "legal",
    request_body = AcceptTerms,
    responses(
        (status = 200, body = TermsStatus),
        (status = 403, description = "`not_for_agents`: an access token cannot accept", body = ErrorBody),
        (status = 404, description = "This server has no Terms", body = ErrorBody),
        (status = 409, description = "`terms_changed`: another version (or the boxes' wording) is in force: read it again", body = ErrorBody),
        (status = 422, description = "`validation` (`professional_use`): the professional edition needs the attestation", body = ErrorBody),
    )
)]
async fn accept_terms(
    user: CurrentUser,
    State(state): State<AppState>,
    Json(body): Json<AcceptTerms>,
) -> ApiResult<Json<TermsStatus>> {
    in_person(&user)?;
    let legal = state
        .config
        .legal
        .as_ref()
        .ok_or_else(|| ApiError::not_found("This server has no Terms of Service."))?;
    let edition = legal.edition();
    if edition == TermsEdition::Professional && !body.professional_use {
        return Err(crate::orgs::invalid(
            "professional_use",
            "Confirm that you will use romulus for professional purposes.",
        ));
    }
    let copy_version = crate::billing::copy::version(edition);
    if body.version != legal.terms_version || body.copy_version != copy_version {
        return Err(ApiError::Conflict {
            code: "terms_changed",
            message: format!(
                "Version {} of the Terms is in force now: read it, then accept it.",
                legal.terms_version
            ),
        });
    }
    let mut tx = state.db.begin().await?;
    // One acceptance at a time per person: two tabs record one.
    sqlx::query!("SELECT id FROM users WHERE id = $1 FOR UPDATE", user.id)
        .fetch_one(&mut *tx)
        .await?;
    let accepted = latest(&mut *tx, user.id)
        .await?
        .is_some_and(|l| l.is_of(legal));
    if !accepted {
        sqlx::query!(
            "INSERT INTO legal_acceptances (kind, user_id, user_email, user_name, terms_version, edition,
                                            locale, professional_use, copy_version)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
            AcceptanceKind::Terms as AcceptanceKind,
            user.id,
            user.email,
            user.name,
            legal.terms_version,
            edition as TermsEdition,
            body.locale as Locale,
            (edition == TermsEdition::Professional).then_some(true),
            copy_version,
        )
        .execute(&mut *tx)
        .await?;
        tracing::info!(version = %legal.terms_version, "legal: terms accepted");
    }
    sqlx::query!(
        "UPDATE users SET locale = $2 WHERE id = $1",
        user.id,
        body.locale as Locale
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Json(status(&state, &user).await?))
}

/// The language of the person's contract and billing emails.
#[utoipa::path(
    patch,
    path = "/me/preferences",
    operation_id = "update_preferences",
    tag = "legal",
    request_body = Preferences,
    responses(
        (status = 200, body = TermsStatus),
        (status = 403, description = "`not_for_agents`", body = ErrorBody),
    )
)]
async fn preferences(
    user: CurrentUser,
    State(state): State<AppState>,
    Json(body): Json<Preferences>,
) -> ApiResult<Json<TermsStatus>> {
    in_person(&user)?;
    sqlx::query!(
        "UPDATE users SET locale = $2 WHERE id = $1",
        user.id,
        body.locale as Locale
    )
    .execute(&state.db)
    .await?;
    Ok(Json(status(&state, &user).await?))
}

/// 403 `terms_required` unless the person accepted the Terms (a version,
/// not necessarily the latest): creating or joining an organization is
/// using the Service, whose contract the box at sign-in concludes.
pub async fn require_accepted(
    db: impl PgExecutor<'_>,
    config: &crate::Config,
    user: Uuid,
) -> ApiResult<()> {
    if config.legal.is_none() {
        return Ok(());
    }
    let accepted = sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM legal_acceptances WHERE user_id = $1) AS "accepted!""#,
        user
    )
    .fetch_one(db)
    .await?;
    if accepted {
        Ok(())
    } else {
        Err(ApiError::Denied {
            code: "terms_required",
            message: "Accept the Terms of Service first: sign in again to read them.".into(),
        })
    }
}

/// How often [`purge`] runs.
pub const PURGE_EVERY: Duration = Duration::from_hours(24);

/// Records past their retention (the Privacy Policy, section 6): under the
/// professional edition, the contract and 5 years after it ends (the
/// account deleted); under the full one, a checkout's and a withdrawal's 10
/// years after (Article L213-1), a sign-in's 5 years after the account
/// ends. How many went.
pub async fn purge(db: &PgPool) -> Result<u64, sqlx::Error> {
    let withdrawals =
        sqlx::query!("DELETE FROM withdrawals WHERE requested_at < now() - interval '10 years'")
            .execute(db)
            .await?
            .rows_affected();
    let acceptances = sqlx::query!(
        "DELETE FROM legal_acceptances
         WHERE (edition = 'full' AND kind = 'checkout' AND accepted_at < now() - interval '10 years')
            OR ((edition = 'professional' OR kind = 'terms')
                AND account_deleted_at < now() - interval '5 years')"
    )
    .execute(db)
    .await?
    .rows_affected();
    Ok(withdrawals + acceptances)
}

/// [`purge`] now and every [`PURGE_EVERY`], when the Terms are on.
pub fn tasks(state: &AppState) -> Vec<tokio::task::JoinHandle<()>> {
    if state.config.legal.is_none() {
        return Vec::new();
    }
    let db = state.db.clone();
    vec![tokio::spawn(async move {
        let mut tick = tokio::time::interval(PURGE_EVERY);
        loop {
            tick.tick().await;
            match purge(&db).await {
                Ok(0) => {}
                Ok(n) => tracing::info!(records = n, "legal: records past their retention purged"),
                Err(err) => tracing::warn!(error = %err, "legal: purge failed"),
            }
        }
    })]
}
