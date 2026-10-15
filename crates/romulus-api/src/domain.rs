//! Types more than one module needs: the Postgres enums, mapped once.
//!
//! The `query!` macros cannot infer custom enum types; annotate the column:
//! `SELECT role AS "role: Role" FROM memberships`.

/// The schema model every version and compile result holds: remus's JSON.
pub use remus_core::Schema;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// A member's role in an organization (`member_role`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, sqlx::Type, ToSchema)]
#[sqlx(type_name = "member_role", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Admin,
    Developer,
    Reviewer,
}

impl Role {
    /// Every role, most powerful first.
    pub const ALL: [Self; 3] = [Self::Admin, Self::Developer, Self::Reviewer];

    /// Whether this role has every capability of `min`: the roles nest
    /// (admin ⊃ developer ⊃ reviewer, docs/MVP.md "Roles").
    pub fn at_least(self, min: Self) -> bool {
        self.rank() >= min.rank()
    }

    fn rank(self) -> u8 {
        match self {
            Self::Admin => 3,
            Self::Developer => 2,
            Self::Reviewer => 1,
        }
    }

    /// Edit drafts, create versions, upload, manage API tokens, create and
    /// delete databases.
    pub fn can_write(self) -> bool {
        matches!(self, Self::Admin | Self::Developer)
    }

    /// Invite, change roles, remove members.
    pub fn can_manage_members(self) -> bool {
        self == Self::Admin
    }
}

/// What a version's `source` holds (`version_source_kind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, sqlx::Type, ToSchema)]
#[sqlx(type_name = "version_source_kind", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    Sql,
    /// Only ever from the push endpoint (a database CHECK enforces it).
    RemusJson,
}

/// How a version came to be (`version_origin`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, sqlx::Type, ToSchema)]
#[sqlx(type_name = "version_origin", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum VersionOrigin {
    Upload,
    Push,
    Draft,
}

/// A draft's lifecycle (`draft_status`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, sqlx::Type, ToSchema)]
#[sqlx(type_name = "draft_status", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum DraftStatus {
    Open,
    Published,
    Deleted,
}

/// What an API token may do (`api_token_scope`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, sqlx::Type, ToSchema)]
#[sqlx(type_name = "api_token_scope", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum TokenScope {
    /// Push remus JSON versions, nothing else (CI).
    Push,
    Full,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reviewers_read_only() {
        assert!(!Role::Reviewer.can_write());
        assert!(Role::Developer.can_write());
        assert!(!Role::Developer.can_manage_members());
        assert!(Role::Admin.can_manage_members());
    }

    #[test]
    fn roles_nest() {
        for role in Role::ALL {
            assert!(role.at_least(Role::Reviewer), "{role:?}");
            assert_eq!(role.at_least(Role::Developer), role.can_write(), "{role:?}");
            assert_eq!(
                role.at_least(Role::Admin),
                role.can_manage_members(),
                "{role:?}"
            );
        }
    }

    #[test]
    fn enums_serialize_like_postgres_spells_them() {
        assert_eq!(
            serde_json::to_string(&SourceKind::RemusJson).unwrap(),
            r#""remus_json""#
        );
        assert_eq!(serde_json::to_string(&Role::Admin).unwrap(), r#""admin""#);
    }
}
