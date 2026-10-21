//! Enforcing the plan table: each cap refuses creating the next resource
//! with 402 `plan_limit` (what was refused, the cap, the usage, the plan that
//! lifts it), and never deletes or hides anything but versions past the
//! history window (hidden, kept, back on upgrade). With billing off every
//! check passes: a self-hosted install is never capped.

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgExecutor;
use utoipa::ToSchema;
use uuid::Uuid;

use super::{
    Billing,
    plans::{Limits, lifting, room, size_allowed},
};
use crate::{
    ApiError, ApiResult,
    domain::{Plan, PlaygroundSize},
};

/// Which cap refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Cap {
    Members,
    Databases,
    Versions,
    SchemaSize,
    History,
    PlaygroundSize,
    PlaygroundHours,
    Approvals,
    ApprovalsOverride,
}

/// The `details` of a 402 `plan_limit`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PlanLimitDetails {
    pub cap: Cap,
    /// The organization's plan now.
    pub plan: Plan,
    /// What the plan allows (`null`: not a count, e.g. a playground size).
    pub limit: Option<i64>,
    /// What there is now (or what was asked for).
    pub usage: Option<i64>,
    /// The cheapest plan that allows it; `null` when none does.
    pub upgrade_to: Option<Plan>,
}

/// A 402 for `cap`, saying `message`.
pub fn refuse(details: PlanLimitDetails, message: String) -> ApiError {
    ApiError::PlanLimit {
        message,
        details: serde_json::to_value(details).unwrap_or_default(),
    }
}

fn what_lifts(upgrade_to: Option<Plan>, says: impl Fn(&Limits) -> String) -> String {
    match upgrade_to {
        Some(plan) => format!("{} {}.", plan.name(), says(plan.limits())),
        None => "Contact us for more.".to_owned(),
    }
}

fn count(n: Option<u32>) -> String {
    n.map_or_else(|| "unlimited".to_owned(), |n| n.to_string())
}

fn plural(n: impl Into<i64>, one: &str, many: &str) -> String {
    let n = n.into();
    format!("{n} {}", if n == 1 { one } else { many })
}

/// One more database for an organization that has `used`.
pub fn databases(plan: Plan, used: i64) -> ApiResult<()> {
    let limit = plan.limits().databases;
    if room(Some(limit), used) {
        return Ok(());
    }
    let upgrade_to = lifting(plan, |l| room(Some(l.databases), used));
    Err(refuse(
        PlanLimitDetails {
            cap: Cap::Databases,
            plan,
            limit: Some(limit.into()),
            usage: Some(used),
            upgrade_to,
        },
        format!(
            "The {} plan keeps {}, and this organization has {used}. {}",
            plan.name(),
            plural(limit, "database", "databases"),
            what_lifts(upgrade_to, |l| format!(
                "keeps {}",
                plural(l.databases, "database", "databases")
            )),
        ),
    ))
}

/// One more member (or invitation) for an organization that has `used`.
pub fn members(plan: Plan, used: i64) -> ApiResult<()> {
    let limit = plan.limits().members;
    if room(limit, used) {
        return Ok(());
    }
    let upgrade_to = lifting(plan, |l| room(l.members, used));
    Err(refuse(
        PlanLimitDetails {
            cap: Cap::Members,
            plan,
            limit: limit.map(i64::from),
            usage: Some(used),
            upgrade_to,
        },
        format!(
            "The {} plan has {}, counting pending invitations. {}",
            plan.name(),
            plural(limit.unwrap_or(0), "member", "members"),
            what_lifts(upgrade_to, |l| format!("has {} members", count(l.members))),
        ),
    ))
}

/// What a new version is checked against, under the database's lock
/// (`versions::store`). `None`: billing is off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VersionLimits(pub Option<Plan>);

impl VersionLimits {
    /// No caps (billing off, or the caller is romulus itself).
    pub const NONE: Self = Self(None);

    /// A database with `live` versions storing `adding` more, each with at
    /// most `objects` tables and views.
    pub fn check(self, live: i64, adding: i64, objects: usize) -> ApiResult<()> {
        let Some(plan) = self.0 else { return Ok(()) };
        let limits = plan.limits();
        let objects = i64::try_from(objects).unwrap_or(i64::MAX);
        if objects > i64::from(limits.schema_objects) {
            let upgrade_to = lifting(plan, |l| objects <= i64::from(l.schema_objects));
            return Err(refuse(
                PlanLimitDetails {
                    cap: Cap::SchemaSize,
                    plan,
                    limit: Some(limits.schema_objects.into()),
                    usage: Some(objects),
                    upgrade_to,
                },
                format!(
                    "This schema has {objects} tables and views; the {} plan keeps up to {} per database. Nothing was stored. {}",
                    plan.name(),
                    limits.schema_objects,
                    what_lifts(upgrade_to, |l| format!("keeps {}", l.schema_objects)),
                ),
            ));
        }
        let after = live.saturating_add(adding);
        if limits
            .versions_per_database
            .is_some_and(|limit| after > i64::from(limit))
        {
            let limit = limits.versions_per_database.unwrap_or_default();
            let upgrade_to = lifting(plan, |l| {
                l.versions_per_database
                    .is_none_or(|limit| after <= i64::from(limit))
            });
            return Err(refuse(
                PlanLimitDetails {
                    cap: Cap::Versions,
                    plan,
                    limit: Some(limit.into()),
                    usage: Some(live),
                    upgrade_to,
                },
                format!(
                    "This database has {}, and the {} plan keeps {limit} per database. Nothing was stored. {} An admin can also remove old versions.",
                    plural(live, "version", "versions"),
                    plan.name(),
                    what_lifts(upgrade_to, |l| format!(
                        "keeps {}",
                        l.versions_per_database
                            .map_or_else(|| "unlimited versions".to_owned(), |n| format!("{n}"))
                    )),
                ),
            ));
        }
        Ok(())
    }
}

/// A playground (or a migration test's) of `size`.
pub fn playground_size(plan: Plan, size: PlaygroundSize) -> ApiResult<()> {
    let largest = plan.limits().playground_size;
    if size_allowed(largest, size) {
        return Ok(());
    }
    let upgrade_to = lifting(plan, |l| size_allowed(l.playground_size, size));
    Err(refuse(
        PlanLimitDetails {
            cap: Cap::PlaygroundSize,
            plan,
            limit: None,
            usage: None,
            upgrade_to,
        },
        format!(
            "{} playgrounds come with {}. The {} plan makes {} ones.",
            size_word(size),
            upgrade_to.map_or("a larger plan", Plan::name),
            plan.name(),
            size_word(largest).to_lowercase(),
        ),
    ))
}

fn size_word(size: PlaygroundSize) -> &'static str {
    match size {
        PlaygroundSize::Small => "Small",
        PlaygroundSize::Medium => "Medium",
    }
}

/// Starting a playground (or a test) when `used_seconds` ran this month.
pub fn playground_hours(plan: Plan, used_seconds: i64, month: &str, next: &str) -> ApiResult<()> {
    let hours = plan.limits().playground_hours;
    if used_seconds < i64::from(hours) * 3600 {
        return Ok(());
    }
    let upgrade_to = lifting(plan, |l| {
        used_seconds < i64::from(l.playground_hours) * 3600
    });
    Err(refuse(
        PlanLimitDetails {
            cap: Cap::PlaygroundHours,
            plan,
            limit: Some(i64::from(hours) * 3600),
            usage: Some(used_seconds),
            upgrade_to,
        },
        format!(
            "This organization used its {} of playgrounds for {month} ({} plan). They come back on {next}. {}",
            plural(hours, "hour", "hours"),
            plan.name(),
            what_lifts(upgrade_to, |l| format!(
                "has {} a month",
                plural(l.playground_hours, "hour", "hours")
            )),
        ),
    ))
}

/// Making publishing need `wanted` approvals (lowering is always allowed:
/// the caller passes only raises).
pub fn approvals(plan: Plan, wanted: i64) -> ApiResult<()> {
    let limit = i64::from(plan.limits().approvals);
    if wanted <= limit {
        return Ok(());
    }
    let upgrade_to = lifting(plan, |l| wanted <= i64::from(l.approvals));
    let allows = if limit == 0 {
        format!("The {} plan has no required approvals.", plan.name())
    } else {
        format!(
            "The {} plan allows up to {}.",
            plan.name(),
            plural(limit, "required approval", "required approvals")
        )
    };
    Err(refuse(
        PlanLimitDetails {
            cap: Cap::Approvals,
            plan,
            limit: Some(limit),
            usage: Some(wanted),
            upgrade_to,
        },
        format!(
            "{allows} {}",
            what_lifts(upgrade_to, |l| format!("allows up to {}", l.approvals)),
        ),
    ))
}

/// An admin publishing without the approvals.
pub fn approvals_override(plan: Plan) -> ApiResult<()> {
    if plan.limits().approvals_override {
        return Ok(());
    }
    let upgrade_to = lifting(plan, |l| l.approvals_override);
    Err(refuse(
        PlanLimitDetails {
            cap: Cap::ApprovalsOverride,
            plan,
            limit: None,
            usage: None,
            upgrade_to,
        },
        format!(
            "Publishing without the approvals is part of {}. The {} plan publishes with them only.",
            upgrade_to.map_or("a larger plan", Plan::name),
            plan.name(),
        ),
    ))
}

/// The history a plan shows: versions created before `cutoff` are hidden
/// (but the latest one and a version an open draft is based on).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HistoryWindow {
    pub plan: Option<Plan>,
    pub cutoff: Option<DateTime<Utc>>,
}

impl HistoryWindow {
    /// Everything shown (billing off, or an unlimited plan).
    pub const ALL: Self = Self {
        plan: None,
        cutoff: None,
    };

    pub fn of(plan: Option<Plan>, now: DateTime<Utc>) -> Self {
        let cutoff = plan
            .and_then(|plan| plan.limits().history_days)
            .map(|days| now - Duration::days(days.into()));
        Self { plan, cutoff }
    }

    /// Whether a version created at `created_at` is past the window (before
    /// the exceptions are looked at).
    pub fn is_before(self, created_at: DateTime<Utc>) -> bool {
        self.cutoff.is_some_and(|cutoff| created_at < cutoff)
    }

    /// The 402 for opening version `number`, hidden by the plan.
    pub fn refuse(self, number: i32) -> ApiError {
        let plan = self.plan.unwrap_or_default();
        let days = plan.limits().history_days.unwrap_or_default();
        let upgrade_to = lifting(plan, |l| l.history_days.is_none_or(|d| d > days));
        refuse(
            PlanLimitDetails {
                cap: Cap::History,
                plan,
                limit: Some(days.into()),
                usage: None,
                upgrade_to,
            },
            format!(
                "v{number} is older than the {} of history the {} plan shows. It is kept, not deleted: {}",
                plural(days, "day", "days"),
                plan.name(),
                match upgrade_to {
                    Some(up) => format!(
                        "{} shows {}.",
                        up.name(),
                        up.limits()
                            .history_days
                            .map_or_else(|| "all of it".to_owned(), |d| plural(d, "day", "days"))
                    ),
                    None => "contact us to see it.".to_owned(),
                }
            ),
        )
    }
}

impl Billing {
    /// The history window of `org` now.
    pub async fn history(&self, db: impl PgExecutor<'_>, org: Uuid) -> ApiResult<HistoryWindow> {
        Ok(HistoryWindow::of(self.plan(db, org).await?, Utc::now()))
    }

    /// The limits a new version of `database` is checked against.
    pub async fn version_limits(
        &self,
        db: impl PgExecutor<'_>,
        database: Uuid,
    ) -> ApiResult<VersionLimits> {
        if !self.is_on() {
            return Ok(VersionLimits::NONE);
        }
        let org = sqlx::query_scalar!("SELECT org_id FROM databases WHERE id = $1", database)
            .fetch_optional(db)
            .await?;
        let Some(org) = org else {
            return Ok(VersionLimits::NONE);
        };
        // A second connection: `db` may be a transaction's.
        Ok(VersionLimits(self.plan_now(org).await?))
    }
}

impl Billing {
    /// 402 `plan_limit` when version `id` (`number`, created at
    /// `created_at`) of `database`, in `org`, is past the plan's history
    /// window. Nothing to check with billing off.
    pub async fn shown(
        &self,
        db: &sqlx::PgPool,
        org: Uuid,
        database: Uuid,
        version: (Uuid, i32, DateTime<Utc>),
    ) -> ApiResult<()> {
        if !self.is_on() {
            return Ok(());
        }
        let window = self.history(db, org).await?;
        visible(db, window, database, version.0, version.1, version.2).await
    }

    /// [`Billing::shown`] for live version `number` of `database` (its
    /// organization looked up).
    pub async fn shown_number(
        &self,
        db: &sqlx::PgPool,
        database: Uuid,
        number: i32,
    ) -> ApiResult<()> {
        if !self.is_on() {
            return Ok(());
        }
        let row = sqlx::query!(
            "SELECT v.id, v.created_at, d.org_id FROM versions v JOIN databases d ON d.id = v.database_id
             WHERE v.database_id = $1 AND v.number = $2 AND v.removed_at IS NULL",
            database,
            number,
        )
        .fetch_optional(db)
        .await?;
        match row {
            Some(row) => {
                self.shown(db, row.org_id, database, (row.id, number, row.created_at))
                    .await
            }
            None => Ok(()),
        }
    }
}

/// Whether version `id` (created at `created_at`) of `database` is shown by
/// `window`; 402 `plan_limit` otherwise.
pub async fn visible(
    db: impl PgExecutor<'_>,
    window: HistoryWindow,
    database: Uuid,
    id: Uuid,
    number: i32,
    created_at: DateTime<Utc>,
) -> ApiResult<()> {
    if !window.is_before(created_at) {
        return Ok(());
    }
    let kept = sqlx::query_scalar!(
        r#"SELECT version_kept_visible($1, $2) AS "kept!""#,
        id,
        database
    )
    .fetch_one(db)
    .await?;
    if kept {
        Ok(())
    } else {
        Err(window.refuse(number))
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    fn details(err: &ApiError) -> PlanLimitDetails {
        match err {
            ApiError::PlanLimit { details, .. } => serde_json::from_value(details.clone()).unwrap(),
            other => panic!("not a plan limit: {other:?}"),
        }
    }

    #[test]
    fn a_free_organization_keeps_one_database_and_starter_lifts_it() {
        assert!(databases(Plan::Free, 0).is_ok());
        let err = databases(Plan::Free, 1).unwrap_err();
        assert_eq!(err.code(), "plan_limit");
        assert_eq!(err.status(), axum::http::StatusCode::PAYMENT_REQUIRED);
        let d = details(&err);
        assert_eq!(
            (d.cap, d.limit, d.usage, d.upgrade_to),
            (Cap::Databases, Some(1), Some(1), Some(Plan::Starter))
        );
        assert!(
            err.to_string().contains("Starter keeps 3 databases"),
            "{err}"
        );
        // Over the cap after a downgrade: the plan that holds them all.
        assert_eq!(
            details(&databases(Plan::Free, 7).unwrap_err()).upgrade_to,
            Some(Plan::Team)
        );
        assert!(databases(Plan::Business, 49).is_ok());
        assert_eq!(
            details(&databases(Plan::Business, 50).unwrap_err()).upgrade_to,
            None
        );
    }

    #[test]
    fn only_free_caps_members() {
        assert!(members(Plan::Free, 0).is_ok());
        let d = details(&members(Plan::Free, 1).unwrap_err());
        assert_eq!(
            (d.cap, d.limit, d.upgrade_to),
            (Cap::Members, Some(1), Some(Plan::Starter))
        );
        assert!(members(Plan::Starter, 10_000).is_ok());
    }

    #[test]
    fn versions_and_schema_size_are_checked_on_what_would_be_stored() {
        let free = VersionLimits(Some(Plan::Free));
        assert!(free.check(49, 1, 50).is_ok());
        let d = details(&free.check(50, 1, 10).unwrap_err());
        assert_eq!(
            (d.cap, d.limit, d.usage, d.upgrade_to),
            (Cap::Versions, Some(50), Some(50), Some(Plan::Starter))
        );
        let d = details(&free.check(0, 1, 51).unwrap_err());
        assert_eq!(
            (d.cap, d.limit, d.usage, d.upgrade_to),
            (Cap::SchemaSize, Some(50), Some(51), Some(Plan::Starter))
        );
        assert_eq!(
            details(&free.check(0, 1, 201).unwrap_err()).upgrade_to,
            Some(Plan::Team)
        );
        // An import of 20 migrations into 40 versions: all or none.
        assert_eq!(
            details(&free.check(40, 20, 1).unwrap_err()).cap,
            Cap::Versions
        );
        assert!(
            VersionLimits(Some(Plan::Team))
                .check(100_000, 1, 1000)
                .is_ok()
        );
        assert!(VersionLimits::NONE.check(i64::MAX, 1, usize::MAX).is_ok());
    }

    #[test]
    fn playground_sizes_and_hours_follow_the_plan() {
        assert!(playground_size(Plan::Starter, PlaygroundSize::Small).is_ok());
        let d = details(&playground_size(Plan::Starter, PlaygroundSize::Medium).unwrap_err());
        assert_eq!(
            (d.cap, d.upgrade_to),
            (Cap::PlaygroundSize, Some(Plan::Team))
        );
        assert!(playground_size(Plan::Team, PlaygroundSize::Medium).is_ok());
        assert!(playground_hours(Plan::Free, 7199, "October", "November 1").is_ok());
        let err = playground_hours(Plan::Free, 7200, "October", "November 1").unwrap_err();
        let d = details(&err);
        assert_eq!(
            (d.cap, d.limit, d.upgrade_to),
            (Cap::PlaygroundHours, Some(7200), Some(Plan::Starter))
        );
        assert!(err.to_string().contains("November 1"), "{err}");
    }

    #[test]
    fn approvals_and_their_override_follow_the_plan() {
        assert!(approvals(Plan::Free, 0).is_ok());
        assert_eq!(
            details(&approvals(Plan::Free, 1).unwrap_err()).upgrade_to,
            Some(Plan::Starter)
        );
        assert!(approvals(Plan::Starter, 1).is_ok());
        assert_eq!(
            details(&approvals(Plan::Starter, 2).unwrap_err()).upgrade_to,
            Some(Plan::Team)
        );
        assert!(approvals(Plan::Team, 10).is_ok());
        assert_eq!(
            details(&approvals_override(Plan::Team).unwrap_err()).upgrade_to,
            Some(Plan::Business)
        );
        assert!(approvals_override(Plan::Business).is_ok());
    }

    #[test]
    fn the_history_window_hides_what_is_older_than_the_plan_keeps() {
        let now = Utc.with_ymd_and_hms(2026, 10, 20, 0, 0, 0).unwrap();
        let free = HistoryWindow::of(Some(Plan::Free), now);
        assert_eq!(free.cutoff, Some(now - Duration::days(90)));
        assert!(free.is_before(now - Duration::days(91)));
        assert!(!free.is_before(now - Duration::days(89)));
        let starter = HistoryWindow::of(Some(Plan::Starter), now);
        assert!(!starter.is_before(now - Duration::days(91)));
        assert!(starter.is_before(now - Duration::days(366)));
        assert_eq!(HistoryWindow::of(Some(Plan::Team), now).cutoff, None);
        assert_eq!(HistoryWindow::of(None, now), HistoryWindow::ALL);
        let err = free.refuse(3);
        let d = details(&err);
        assert_eq!(
            (d.cap, d.limit, d.upgrade_to),
            (Cap::History, Some(90), Some(Plan::Starter))
        );
        assert!(err.to_string().contains("It is kept"), "{err}");
    }
}
