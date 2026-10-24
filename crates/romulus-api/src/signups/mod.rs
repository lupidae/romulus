//! Where sign-ups come from (docs/MVP.md "Signup sources and analytics"),
//! so marketing channels can be compared: the campaign parameters
//! (`utm_source`, `utm_medium`, `utm_campaign`, `utm_content`) and the
//! referring site's origin of the visit that signed up. They ride in URLs
//! only, never in the browser's storage: the homepage adds them to its links
//! into the app (`ref_origin` for the referrer), the sign-in page carries
//! them through `next`, and the page signing in lands on reports them
//! (`web/src/features/marketing/source.ts`). Nothing else about the visitor
//! is ever stored.
//!
//! Endpoint: `POST /me/signup-source` (the person, signed in). A password
//! sign-up sends them with its form instead (`PasswordSignUp::source`, kept
//! on the emailed link until the account exists: [`store`]). Table:
//! `signup_sources`, one row per account reported, deleted with it. Export:
//! [`export`] and [`write_csv`], the operator's `romulus-api signups`.
//!
//! A report counts only for an account made within [`REPORT_WINDOW`]; the
//! first that carries something wins over an empty one (a direct visit, or
//! a page load without the parameters). Its answer tells the app whether
//! this report made the account's row ([`SignupReported::new_account`]),
//! which the app counts as a completed sign-up.

use std::io::{self, Write};

use axum::extract::State;
use chrono::{DateTime, Duration, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{PgConnection, PgPool};
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

use crate::{
    ApiError, ApiResult, AppState, auth::CurrentUser, billing::state::Stored, domain::Plan,
    error::ErrorBody, extract::Json,
};

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(report))
}

/// An account older than this reports nothing: the parameters of a later
/// visit are not how it signed up. The report follows the sign-up's own
/// landing (the identity provider's way back, or a password sign-up's
/// link): well within it.
pub const REPORT_WINDOW: Duration = Duration::hours(1);
/// Each campaign parameter is cut to this many characters.
const UTM_MAX: usize = 200;

/// The source of the visit signing up, as its URL carried it.
#[derive(Debug, Default, Clone, Deserialize, ToSchema)]
pub struct SignupSource {
    pub utm_source: Option<String>,
    pub utm_medium: Option<String>,
    pub utm_campaign: Option<String>,
    pub utm_content: Option<String>,
    /// The site that linked here (the homepage's `ref_origin`, or
    /// `document.referrer`); only its origin is kept, and nothing when it
    /// is this site.
    pub referrer: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct SignupReported {
    /// The source was stored.
    pub recorded: bool,
    /// This report made the account's row (its first, within the hour of
    /// signing up): a sign-up just completed.
    pub new_account: bool,
}

/// The source of a sign-up, cleaned: what is stored (and kept on a password
/// sign-up's link, as JSON).
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Source {
    pub utm_source: Option<String>,
    pub utm_medium: Option<String>,
    pub utm_campaign: Option<String>,
    pub utm_content: Option<String>,
    pub referrer: Option<String>,
}

impl Source {
    /// Campaign parameters trimmed, control characters dropped, cut to
    /// [`UTM_MAX`]; the referrer reduced to an http(s) origin other than
    /// `own_origin`.
    pub fn clean(reported: &SignupSource, own_origin: &str) -> Self {
        Self {
            utm_source: utm(reported.utm_source.as_deref()),
            utm_medium: utm(reported.utm_medium.as_deref()),
            utm_campaign: utm(reported.utm_campaign.as_deref()),
            utm_content: utm(reported.utm_content.as_deref()),
            referrer: referrer(reported.referrer.as_deref(), own_origin),
        }
    }

    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

fn utm(value: Option<&str>) -> Option<String> {
    let value: String = value?
        .chars()
        .filter(|c| !c.is_control())
        .collect::<String>()
        .trim()
        .chars()
        .take(UTM_MAX)
        .collect();
    let value = value.trim_end();
    (!value.is_empty()).then(|| value.to_owned())
}

fn referrer(value: Option<&str>, own_origin: &str) -> Option<String> {
    let url = url::Url::parse(value?.trim()).ok()?;
    if !matches!(url.scheme(), "http" | "https") || url.host().is_none() {
        return None;
    }
    let origin = url.origin().ascii_serialization();
    (origin != own_origin && origin.len() <= 300).then_some(origin)
}

/// Records where the signed-in person came from, once.
#[utoipa::path(
    post,
    path = "/me/signup-source",
    operation_id = "report_signup_source",
    tag = "auth",
    request_body = SignupSource,
    responses(
        (status = 200, body = SignupReported),
        (status = 401, body = ErrorBody),
        (status = 403, description = "`not_for_agents`: an access token's request", body = ErrorBody),
    )
)]
async fn report(
    user: CurrentUser,
    State(state): State<AppState>,
    Json(reported): Json<SignupSource>,
) -> ApiResult<Json<SignupReported>> {
    if user.via.is_some() {
        return Err(ApiError::Denied {
            code: "not_for_agents",
            message: "Only the person, signed in, can do this; an access token cannot.".into(),
        });
    }
    let source = Source::clean(&reported, &state.config.public_origin());
    Ok(Json(record(&state.db, user.id, &source).await?))
}

/// Stores `source` for `user` if it counts (see the module docs).
pub async fn record(
    db: &PgPool,
    user: Uuid,
    source: &Source,
) -> Result<SignupReported, sqlx::Error> {
    let refused = SignupReported {
        recorded: false,
        new_account: false,
    };
    let Some(created_at) = sqlx::query_scalar!("SELECT created_at FROM users WHERE id = $1", user)
        .fetch_optional(db)
        .await?
    else {
        return Ok(refused);
    };
    if Utc::now() - created_at > REPORT_WINDOW {
        return Ok(refused);
    }
    let mut db = db.acquire().await?;
    let written = upsert(&mut db, user, source).await?;
    Ok(SignupReported {
        recorded: written.is_some() && !source.is_empty(),
        new_account: written == Some(true),
    })
}

/// The account's row for a sign-up that brought its source along (a
/// password sign-up's form), in the account's transaction: written even
/// empty, so the app's report that follows finds the sign-up told.
pub async fn store(db: &mut PgConnection, user: Uuid, source: &Source) -> Result<(), sqlx::Error> {
    upsert(db, user, source).await.map(drop)
}

/// Inserts the row (`Some(true)`), fills an empty one (`Some(false)`), or
/// leaves one that has something (`None`).
async fn upsert(
    db: &mut PgConnection,
    user: Uuid,
    source: &Source,
) -> Result<Option<bool>, sqlx::Error> {
    sqlx::query_scalar!(
        r#"INSERT INTO signup_sources
               (user_id, utm_source, utm_medium, utm_campaign, utm_content, referrer)
           VALUES ($1, $2, $3, $4, $5, $6)
           ON CONFLICT (user_id) DO UPDATE SET
               utm_source = EXCLUDED.utm_source,
               utm_medium = EXCLUDED.utm_medium,
               utm_campaign = EXCLUDED.utm_campaign,
               utm_content = EXCLUDED.utm_content,
               referrer = EXCLUDED.referrer
           WHERE num_nonnulls(signup_sources.utm_source, signup_sources.utm_medium,
                              signup_sources.utm_campaign, signup_sources.utm_content,
                              signup_sources.referrer) = 0
             AND num_nonnulls(EXCLUDED.utm_source, EXCLUDED.utm_medium,
                              EXCLUDED.utm_campaign, EXCLUDED.utm_content,
                              EXCLUDED.referrer) > 0
           RETURNING (xmax = 0) AS "inserted!""#,
        user,
        source.utm_source,
        source.utm_medium,
        source.utm_campaign,
        source.utm_content,
        source.referrer,
    )
    .fetch_optional(&mut *db)
    .await
}

/// One account, for the operators' export.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signup {
    pub created_at: DateTime<Utc>,
    pub source: Source,
    /// The best plan in force among the person's organizations.
    pub plan: Plan,
    /// Within 7 days of signing up, the person edited a draft (each edit
    /// compiles) or made a version (an upload, a push with their token, a
    /// publish, an import).
    pub activated: bool,
    /// One of the person's organizations pays for a plan (a live, paid
    /// subscription, not a trial).
    pub paying: bool,
}

/// Every account made since `since` (all of them without), oldest first,
/// with where it came from. Accounts at the operators' ignored domains
/// (`ROMULUS_OPERATOR_IGNORE_DOMAINS`) are left out, as from the digest.
pub async fn export(
    db: &PgPool,
    since: Option<NaiveDate>,
    ignored: &[String],
) -> Result<Vec<Signup>, sqlx::Error> {
    let since = since
        .and_then(|day| day.and_hms_opt(0, 0, 0))
        .map(|at| at.and_utc());
    let rows = sqlx::query!(
        r#"SELECT u.id, u.created_at,
                  s.utm_source, s.utm_medium, s.utm_campaign, s.utm_content, s.referrer,
                  (EXISTS (SELECT 1 FROM draft_editors e
                           WHERE e.user_id = u.id
                             AND e.first_edit_at < u.created_at + interval '7 days')
                   OR EXISTS (SELECT 1 FROM versions v
                              LEFT JOIN api_tokens t ON t.id = v.created_by_token
                              WHERE (v.created_by = u.id OR t.created_by = u.id)
                                AND v.created_at < u.created_at + interval '7 days')
                  ) AS "activated!"
           FROM users u LEFT JOIN signup_sources s ON s.user_id = u.id
           WHERE ($1::timestamptz IS NULL OR u.created_at >= $1)
             AND NOT email_in_domains(u.email, $2)
           ORDER BY u.created_at, u.id"#,
        since,
        ignored,
    )
    .fetch_all(db)
    .await?;
    let billing = sqlx::query!(
        r#"SELECT m.user_id, b.plan AS "plan: Plan", b.status, b.past_due_since, b.withdrawn_at
           FROM memberships m JOIN org_billing b ON b.org_id = m.org_id
           JOIN users u ON u.id = m.user_id
           WHERE ($1::timestamptz IS NULL OR u.created_at >= $1)"#,
        since,
    )
    .fetch_all(db)
    .await?;
    let now = Utc::now();
    let mut plans = std::collections::HashMap::<Uuid, (Plan, bool)>::new();
    for row in billing {
        let stored = Stored {
            plan: row.plan,
            status: row.status,
            past_due_since: row.past_due_since,
            withdrawn_at: row.withdrawn_at,
            ..Stored::default()
        };
        let plan = stored.effective(now);
        let paying = plan != Plan::Free && stored.status.as_deref() != Some("trialing");
        let best = plans.entry(row.user_id).or_insert((Plan::Free, false));
        *best = (best.0.max(plan), best.1 || paying);
    }
    Ok(rows
        .into_iter()
        .map(|row| {
            let (plan, paying) = plans.get(&row.id).copied().unwrap_or_default();
            Signup {
                created_at: row.created_at,
                source: Source {
                    utm_source: row.utm_source,
                    utm_medium: row.utm_medium,
                    utm_campaign: row.utm_campaign,
                    utm_content: row.utm_content,
                    referrer: row.referrer,
                },
                plan,
                activated: row.activated,
                paying,
            }
        })
        .collect())
}

/// The CSV's first line.
pub const CSV_HEADER: &str =
    "created_at,utm_source,utm_medium,utm_campaign,utm_content,referrer,plan,activated_7d,paying";

/// `signups` as CSV (RFC 4180, `\r\n` lines), a header first.
pub fn write_csv(signups: &[Signup], out: &mut impl Write) -> io::Result<()> {
    write!(out, "{CSV_HEADER}\r\n")?;
    for signup in signups {
        let source = &signup.source;
        let fields = [
            signup.created_at.format("%Y-%m-%dT%H:%M:%SZ").to_string(),
            csv_field(source.utm_source.as_deref()),
            csv_field(source.utm_medium.as_deref()),
            csv_field(source.utm_campaign.as_deref()),
            csv_field(source.utm_content.as_deref()),
            csv_field(source.referrer.as_deref()),
            signup.plan.slug().to_owned(),
            signup.activated.to_string(),
            signup.paying.to_string(),
        ];
        write!(out, "{}\r\n", fields.join(","))?;
    }
    Ok(())
}

/// A value anyone could have put in a link, as one CSV field: quoted when
/// it must be, and never a formula a spreadsheet would run (a leading `=`,
/// `+`, `-`, `@` gets a `'` before it).
fn csv_field(value: Option<&str>) -> String {
    let Some(value) = value else {
        return String::new();
    };
    let value = if value.starts_with(['=', '+', '-', '@', '\t', '\r']) {
        format!("'{value}")
    } else {
        value.to_owned()
    };
    if value.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OWN: &str = "https://romulus.example.com";

    fn reported(utm_source: &str, referrer: &str) -> SignupSource {
        SignupSource {
            utm_source: Some(utm_source.to_owned()),
            referrer: Some(referrer.to_owned()),
            ..SignupSource::default()
        }
    }

    #[test]
    fn referrers_keep_only_another_sites_origin() {
        let clean = |r: &str| Source::clean(&reported("x", r), OWN).referrer;
        assert_eq!(
            clean("https://news.example.org/item?id=1&email=a@b.c#top").as_deref(),
            Some("https://news.example.org")
        );
        assert_eq!(
            clean("http://localhost:3000/x").as_deref(),
            Some("http://localhost:3000")
        );
        assert_eq!(clean("https://romulus.example.com/app/signin"), None);
        assert_eq!(clean("android-app://com.google.android.gm/"), None);
        assert_eq!(clean("not a url"), None);
        assert_eq!(clean(""), None);
    }

    #[test]
    fn campaign_parameters_are_trimmed_and_cut() {
        let clean = |s: &str| Source::clean(&reported(s, ""), OWN).utm_source;
        assert_eq!(clean("  newsletter \n").as_deref(), Some("newsletter"));
        assert_eq!(clean("a\u{0}b\u{1b}c").as_deref(), Some("abc"));
        assert_eq!(clean("   "), None);
        assert_eq!(
            clean(&"é".repeat(300)).map(|s| s.chars().count()),
            Some(200)
        );
        assert!(Source::clean(&SignupSource::default(), OWN).is_empty());
    }

    #[test]
    fn csv_quotes_what_it_must_and_never_writes_a_formula() {
        assert_eq!(csv_field(None), "");
        assert_eq!(csv_field(Some("hn")), "hn");
        assert_eq!(csv_field(Some("a,b")), "\"a,b\"");
        assert_eq!(csv_field(Some("say \"hi\"")), "\"say \"\"hi\"\"\"");
        assert_eq!(
            csv_field(Some("=HYPERLINK(\"x\")")),
            "\"'=HYPERLINK(\"\"x\"\")\""
        );
        assert_eq!(csv_field(Some("-1+1")), "'-1+1");
        assert_eq!(csv_field(Some("@sum")), "'@sum");
    }

    #[test]
    fn csv_has_a_header_and_a_line_per_signup() {
        let signup = Signup {
            created_at: "2026-10-24T09:30:00Z".parse().unwrap(),
            source: Source {
                utm_source: Some("newsletter".into()),
                utm_campaign: Some("launch, part 2".into()),
                referrer: Some("https://news.example.org".into()),
                ..Source::default()
            },
            plan: Plan::Team,
            activated: true,
            paying: false,
        };
        let mut out = Vec::new();
        write_csv(&[signup], &mut out).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            format!(
                "{CSV_HEADER}\r\n2026-10-24T09:30:00Z,newsletter,,\"launch, part 2\",,https://news.example.org,team,true,false\r\n"
            )
        );
    }
}
