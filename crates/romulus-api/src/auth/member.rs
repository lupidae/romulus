//! [`Member`]: the extractor every organization endpoint starts with.
//!
//! It reads the `{org}` path segment (a slug), loads the signed-in user's
//! membership there, and checks the role against the endpoint's minimum, so
//! a handler that compiles cannot forget the check. The minimum is a type
//! parameter, named after the roles of docs/MVP.md "Roles":
//!
//! | extractor | who | for |
//! |---|---|---|
//! | `Member` (`Member<min::Reviewer>`) | every member | reading databases, versions, diffs; watching a draft |
//! | `Member<min::Developer>` | developers, admins | editing drafts, creating versions, uploads, API tokens, creating and deleting databases |
//! | `Member<min::Admin>` | admins | inviting, changing roles, removing members, organization settings (billing later) |
//!
//! Someone who is not a member gets 404 (an organization's existence is not
//! public); a member below the minimum gets 403 `forbidden` with a sentence
//! saying who can.

use std::marker::PhantomData;

use axum::{
    extract::{FromRequestParts, RawPathParams},
    http::request::Parts,
};
use uuid::Uuid;

use super::CurrentUser;
use crate::{ApiError, AppState, domain::Role};

/// The minimum roles, as types: `Member<min::Developer>`.
pub mod min {
    use crate::domain::Role;

    /// A role an endpoint requires at least.
    pub trait MinRole: Send + Sync + 'static {
        const ROLE: Role;
        /// The 403's message: who may do this.
        const DENIED: &'static str;
    }

    /// Any member: the default.
    #[derive(Debug, Clone, Copy)]
    pub struct Reviewer;
    /// Developers and admins.
    #[derive(Debug, Clone, Copy)]
    pub struct Developer;
    /// Admins only.
    #[derive(Debug, Clone, Copy)]
    pub struct Admin;

    impl MinRole for Reviewer {
        const ROLE: Role = Role::Reviewer;
        const DENIED: &'static str = "Only members of this organization can do this.";
    }

    impl MinRole for Developer {
        const ROLE: Role = Role::Developer;
        const DENIED: &'static str =
            "Reviewers have read-only access. Ask an admin to make you a developer.";
    }

    impl MinRole for Admin {
        const ROLE: Role = Role::Admin;
        const DENIED: &'static str = "Only admins of this organization can do this.";
    }
}

use min::MinRole;

/// The organization a request is about.
#[derive(Debug, Clone)]
pub struct OrgRef {
    pub id: Uuid,
    pub slug: String,
    pub name: String,
    /// Approvals publishing a draft needs (0: none).
    pub approvals_required: i16,
}

/// The signed-in user, a member of the `{org}` in the path with at least
/// role `R`. See the module documentation for which `R` an endpoint needs.
#[derive(Debug, Clone)]
pub struct Member<R: MinRole = min::Reviewer> {
    pub user: CurrentUser,
    pub org: OrgRef,
    /// Their actual role (at least `R`).
    pub role: Role,
    min: PhantomData<R>,
}

impl<R: MinRole> FromRequestParts<AppState> for Member<R> {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        let user = CurrentUser::from_request_parts(parts, state).await?;
        let params = RawPathParams::from_request_parts(parts, state)
            .await
            .map_err(|rejection| ApiError::BadRequest(rejection.body_text()))?;
        let slug = params
            .iter()
            .find_map(|(name, value)| (name == "org").then_some(value))
            .ok_or_else(|| {
                ApiError::internal(std::io::Error::other(
                    "Member is extracted on a route without an {org} segment",
                ))
            })?;

        let membership = sqlx::query!(
            r#"SELECT o.id, o.slug, o.name, o.approvals_required, m.role AS "role: Role"
               FROM organizations o
               JOIN memberships m ON m.org_id = o.id AND m.user_id = $2
               WHERE o.slug = $1"#,
            slug,
            user.id,
        )
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| ApiError::not_found("No such organization."))?;

        if !membership.role.at_least(R::ROLE) {
            return Err(ApiError::Forbidden(R::DENIED.to_owned()));
        }
        Ok(Self {
            user,
            org: OrgRef {
                id: membership.id,
                slug: membership.slug,
                name: membership.name,
                approvals_required: membership.approvals_required,
            },
            role: membership.role,
            min: PhantomData,
        })
    }
}
