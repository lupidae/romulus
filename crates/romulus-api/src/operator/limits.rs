//! A plan's cap reached (hosted billing): worth an upgrade conversation, so
//! the operators hear of it once per organization and cap in
//! [`AGAIN_AFTER_DAYS`] (`operator_limit_notices`).
//!
//! Every refusal is a 402 `plan_limit` whose details ride in the response's
//! extensions (`error::PlanLimitReached`); [`watch`] (a layer on the whole
//! app) and the MCP adapter (`mcp::api`, which calls the routes itself) take
//! it out ([`reached`]) and hand it to [`observe`], which finds the
//! organization from the path
//! (`/api/orgs/{org}/…`, `/api/databases/{database_id}/…`). The refusal
//! wrote nothing, so this is written in a transaction of its own, after it;
//! a failure there is logged and the 402 goes out as it was. A limit only an
//! import's progress stream reports (no 402 of its own) is not seen.

use axum::{
    extract::{Request, State},
    middleware::Next,
    response::Response,
};
use uuid::Uuid;

use super::{Dashboard, Event, Notice, notify, who};
use crate::{
    AppState,
    billing::{Cap, PlanLimitDetails},
    error::PlanLimitReached,
};

/// Days before the same organization reaching the same cap is told again.
pub const AGAIN_AFTER_DAYS: i32 = 30;

/// The layer: tells [`observe`] of every answer, when notifications are on.
pub async fn watch(State(state): State<AppState>, request: Request, next: Next) -> Response {
    if state.config.operators().is_empty() {
        return next.run(request).await;
    }
    let path = request.uri().path().to_owned();
    let response = next.run(request).await;
    if let Some(details) = reached(&response) {
        observe(&state, &path, &details).await;
    }
    response
}

/// The cap a response refused for, if it is a 402 `plan_limit`.
pub fn reached(response: &Response) -> Option<PlanLimitDetails> {
    let PlanLimitReached(details) = response.extensions().get::<PlanLimitReached>()?;
    serde_json::from_value(details.clone()).ok()
}

/// Where a request was: an organization's pages, or a database's machine
/// API.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Place {
    Org(String),
    Database(Uuid),
}

impl Place {
    fn of(path: &str) -> Option<Self> {
        let mut segments = path.strip_prefix("/api/")?.split('/');
        match (segments.next()?, segments.next()?) {
            ("orgs", slug) if !slug.is_empty() => Some(Self::Org(slug.to_owned())),
            ("databases", id) => id.parse().ok().map(Self::Database),
            _ => None,
        }
    }
}

/// A request for `path` was refused for a plan's cap (`details`): tells the
/// operators, once per organization and cap in [`AGAIN_AFTER_DAYS`]. A
/// failure is logged, never the caller's.
pub async fn observe(state: &AppState, path: &str, details: &PlanLimitDetails) {
    if state.config.operators().is_empty() {
        return;
    }
    let Some(place) = Place::of(path) else {
        return;
    };
    if let Err(err) = record(state, &place, details).await {
        tracing::warn!(error = %err, "operator: cannot record a plan limit reached");
    }
}

/// The cap as stored (`operator_limit_notices.cap`), as the API names it.
pub fn cap_key(cap: Cap) -> &'static str {
    match cap {
        Cap::Members => "members",
        Cap::Databases => "databases",
        Cap::Versions => "versions",
        Cap::SchemaSize => "schema_size",
        Cap::History => "history",
        Cap::PlaygroundSize => "playground_size",
        Cap::PlaygroundHours => "playground_hours",
        Cap::Approvals => "approvals",
        Cap::ApprovalsOverride => "approvals_override",
    }
}

fn cap_words(cap: Cap) -> &'static str {
    match cap {
        Cap::Members => "members",
        Cap::Databases => "databases",
        Cap::Versions => "versions per database",
        Cap::SchemaSize => "schema size",
        Cap::History => "version history",
        Cap::PlaygroundSize => "playground size",
        Cap::PlaygroundHours => "playground hours",
        Cap::Approvals => "required approvals",
        Cap::ApprovalsOverride => "publishing without the approvals",
    }
}

async fn record(
    state: &AppState,
    place: &Place,
    details: &PlanLimitDetails,
) -> Result<(), sqlx::Error> {
    let mut tx = state.db.begin().await?;
    let org = match place {
        Place::Org(slug) => sqlx::query!(
            "SELECT id, name, slug FROM organizations WHERE slug = $1",
            slug
        )
        .fetch_optional(&mut *tx)
        .await?
        .map(|o| (o.id, o.name, o.slug)),
        Place::Database(id) => sqlx::query!(
            "SELECT o.id, o.name, o.slug FROM databases d JOIN organizations o ON o.id = d.org_id
             WHERE d.id = $1",
            id
        )
        .fetch_optional(&mut *tx)
        .await?
        .map(|o| (o.id, o.name, o.slug)),
    };
    let Some((org, name, slug)) = org else {
        return Ok(());
    };
    let Some(notified_at) = sqlx::query_scalar!(
        "INSERT INTO operator_limit_notices (org_id, cap) VALUES ($1, $2)
         ON CONFLICT (org_id, cap) DO UPDATE SET notified_at = now()
         WHERE operator_limit_notices.notified_at < now() - make_interval(days => $3)
         RETURNING notified_at",
        org,
        cap_key(details.cap),
        AGAIN_AFTER_DAYS,
    )
    .fetch_optional(&mut *tx)
    .await?
    else {
        return Ok(());
    };
    let admins: Vec<String> = sqlx::query!(
        "SELECT u.name, u.email FROM memberships m JOIN users u ON u.id = m.user_id
         WHERE m.org_id = $1 AND m.role = 'admin' ORDER BY m.created_at LIMIT 3",
        org
    )
    .fetch_all(&mut *tx)
    .await?
    .into_iter()
    .map(|a| who(&a.name, &a.email))
    .collect();
    let customer =
        sqlx::query_scalar!("SELECT customer_id FROM org_billing WHERE org_id = $1", org)
            .fetch_optional(&mut *tx)
            .await?
            .flatten();
    let mut notice = notice(
        &format!("{name} ({slug})"),
        details,
        &admins,
        customer.as_deref(),
        Dashboard::of(state),
    );
    notice.about = format!(
        "{org}:{}:{}",
        cap_key(details.cap),
        notified_at.timestamp_micros()
    );
    notify(&mut tx, state.config.operator.as_ref(), &notice).await?;
    tx.commit().await
}

fn notice(
    org: &str,
    details: &PlanLimitDetails,
    admins: &[String],
    customer: Option<&str>,
    dashboard: Option<Dashboard>,
) -> Notice {
    let cap = cap_words(details.cap);
    let plan = details.plan.name();
    let count = match (details.usage, details.limit) {
        (Some(usage), Some(limit)) => format!(": {usage} of {limit}"),
        _ => String::new(),
    };
    let mut lines = vec![
        format!("{org}, on the {plan} plan, reached its limit of {cap}{count}."),
        details.upgrade_to.map_or_else(
            || "No plan lifts it.".to_owned(),
            |to| format!("The {} plan lifts it.", to.name()),
        ),
    ];
    if !admins.is_empty() {
        lines.push(format!("Admins: {}.", admins.join(", ")));
    }
    if let (Some(d), Some(customer)) = (dashboard, customer) {
        lines.push(format!("Stripe customer: {}", d.customer(customer)));
    }
    lines.push(format!(
        "Told once in {AGAIN_AFTER_DAYS} days per organization and limit."
    ));
    Notice {
        event: Event::PlanLimitReached,
        // Set by `record`: the organization, the cap and when it was told.
        about: String::new(),
        subject: format!("Plan limit reached: {org}, {plan}, {cap}{count}"),
        lines,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::Plan;

    #[test]
    fn the_place_comes_from_an_organizations_path_or_a_databases() {
        assert_eq!(
            Place::of("/api/orgs/acme/databases"),
            Some(Place::Org("acme".into()))
        );
        let id = Uuid::from_u128(9);
        assert_eq!(
            Place::of(&format!("/api/databases/{id}/versions")),
            Some(Place::Database(id))
        );
        assert_eq!(Place::of("/api/databases/nope/versions"), None);
        assert_eq!(Place::of("/api/orgs"), None);
        assert_eq!(Place::of("/app/acme"), None);
    }

    #[test]
    fn the_notice_says_the_cap_the_usage_and_what_lifts_it() {
        let notice = notice(
            "Acme (acme)",
            &PlanLimitDetails {
                cap: Cap::Databases,
                plan: Plan::Free,
                limit: Some(1),
                usage: Some(1),
                upgrade_to: Some(Plan::Starter),
            },
            &["Ada <ada@example.com>".to_owned()],
            None,
            None,
        );
        assert_eq!(
            notice.subject,
            "Plan limit reached: Acme (acme), Free, databases: 1 of 1"
        );
        assert!(
            notice
                .lines
                .contains(&"The Starter plan lifts it.".to_owned())
        );
        assert!(
            notice
                .lines
                .contains(&"Admins: Ada <ada@example.com>.".to_owned())
        );
    }
}
