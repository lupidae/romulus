//! Types more than one module needs: the Postgres enums, mapped once.
//!
//! The `query!` macros cannot infer custom enum types; annotate the column:
//! `SELECT role AS "role: Role" FROM memberships`.

use std::{borrow::Cow, fmt};

/// The schema model every version and compile result holds: remus's JSON.
pub use remus_core::Schema;
use serde::{Deserialize, Serialize};
use utoipa::{
    PartialSchema, ToSchema,
    openapi::{ObjectBuilder, RefOr, Type, schema::Schema as OpenApiSchema},
};

/// A Postgres major a database targets (`databases.pg_major`): 15, 16, 17
/// (the default) or 18. Its SQL compiles on that major's sandbox and its
/// migrations are generated for it (docs/MVP.md, "Several Postgres
/// majors"). JSON and SQL: the number (`smallint`); anything else is
/// refused when read, from either.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "i16", into = "i16")]
pub struct PgMajor(i16);

impl sqlx::Type<sqlx::Postgres> for PgMajor {
    fn type_info() -> sqlx::postgres::PgTypeInfo {
        <i16 as sqlx::Type<sqlx::Postgres>>::type_info()
    }

    fn compatible(ty: &sqlx::postgres::PgTypeInfo) -> bool {
        <i16 as sqlx::Type<sqlx::Postgres>>::compatible(ty)
    }
}

impl sqlx::Encode<'_, sqlx::Postgres> for PgMajor {
    fn encode_by_ref(
        &self,
        buf: &mut sqlx::postgres::PgArgumentBuffer,
    ) -> Result<sqlx::encode::IsNull, sqlx::error::BoxDynError> {
        <i16 as sqlx::Encode<sqlx::Postgres>>::encode_by_ref(&self.0, buf)
    }
}

/// A column that holds another major (none should: `databases` checks its
/// own, and nothing writes another) is an error, never a `PgMajor`.
impl sqlx::Decode<'_, sqlx::Postgres> for PgMajor {
    fn decode(value: sqlx::postgres::PgValueRef<'_>) -> Result<Self, sqlx::error::BoxDynError> {
        let major = <i16 as sqlx::Decode<sqlx::Postgres>>::decode(value)?;
        Self::try_from(major).map_err(Into::into)
    }
}

impl PgMajor {
    /// Every major romulus compiles on, oldest first: a sandbox image each
    /// (deploy/sandbox) and `databases_pg_major_supported`.
    pub const SUPPORTED: [Self; 4] = [Self(15), Self(16), Self(17), Self(18)];
    /// What a new database targets, and the sandbox `ROMULUS_SANDBOX_URL`
    /// names.
    pub const DEFAULT: Self = Self(17);

    pub fn new(major: i64) -> Option<Self> {
        Self::SUPPORTED
            .into_iter()
            .find(|supported| i64::from(supported.0) == major)
    }

    pub fn get(self) -> i16 {
        self.0
    }

    /// As `romulus_migration::Options::major` and the sandbox count it.
    pub fn number(self) -> u32 {
        u32::try_from(self.0).unwrap_or_default()
    }
}

impl Default for PgMajor {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl fmt::Display for PgMajor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl TryFrom<i16> for PgMajor {
    type Error = String;

    fn try_from(major: i16) -> Result<Self, Self::Error> {
        Self::new(major.into()).ok_or_else(|| {
            format!("romulus does not compile on Postgres {major}: use 15, 16, 17 or 18")
        })
    }
}

impl From<PgMajor> for i16 {
    fn from(major: PgMajor) -> Self {
        major.0
    }
}

impl PartialSchema for PgMajor {
    fn schema() -> RefOr<OpenApiSchema> {
        ObjectBuilder::new()
            .schema_type(Type::Integer)
            .description(Some(
                "A Postgres major romulus compiles on: 15, 16, 17 (the default) or 18.",
            ))
            .enum_values(Some(Self::SUPPORTED.map(|m| i64::from(m.0))))
            .into()
    }
}

impl ToSchema for PgMajor {
    fn name() -> Cow<'static, str> {
        Cow::Borrowed("PgMajor")
    }
}

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
    /// A migration's up SQL: what changed, not the whole schema. Only ever
    /// from an import (a database CHECK enforces it).
    Migration,
}

/// How a version came to be (`version_origin`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, sqlx::Type, ToSchema)]
#[sqlx(type_name = "version_origin", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum VersionOrigin {
    Upload,
    Push,
    Draft,
    /// One migration of an imported history (`imports`).
    Import,
}

/// The migration tool an imported version came from (`migration_format`):
/// `romulus_sandbox::import::Format` (whose tools are
/// `romulus_migration::Format`'s), under the same wire names (a test keeps
/// them in agreement).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, sqlx::Type, ToSchema)]
#[sqlx(type_name = "migration_format", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum MigrationFormat {
    Plain,
    Sqlx,
    Flyway,
    Prisma,
    Dbmate,
    GolangMigrate,
    NodePgMigrate,
    Alembic,
    Django,
    Rails,
}

impl From<romulus_sandbox::import::Format> for MigrationFormat {
    fn from(format: romulus_sandbox::import::Format) -> Self {
        use romulus_sandbox::import::{Format as F, ReadOnly as R, Tool as T};
        match format {
            F::Tool(T::Plain) => Self::Plain,
            F::Tool(T::Sqlx) => Self::Sqlx,
            F::Tool(T::Flyway) => Self::Flyway,
            F::Tool(T::Prisma) => Self::Prisma,
            F::Tool(T::Dbmate) => Self::Dbmate,
            F::Tool(T::GolangMigrate) => Self::GolangMigrate,
            F::Read(R::NodePgMigrate) => Self::NodePgMigrate,
            F::Read(R::Alembic) => Self::Alembic,
            F::Read(R::Django) => Self::Django,
            F::Read(R::Rails) => Self::Rails,
        }
    }
}

impl From<MigrationFormat> for romulus_sandbox::import::Format {
    fn from(format: MigrationFormat) -> Self {
        use romulus_sandbox::import::{Format as F, ReadOnly as R, Tool as T};
        match format {
            MigrationFormat::Plain => F::Tool(T::Plain),
            MigrationFormat::Sqlx => F::Tool(T::Sqlx),
            MigrationFormat::Flyway => F::Tool(T::Flyway),
            MigrationFormat::Prisma => F::Tool(T::Prisma),
            MigrationFormat::Dbmate => F::Tool(T::Dbmate),
            MigrationFormat::GolangMigrate => F::Tool(T::GolangMigrate),
            MigrationFormat::NodePgMigrate => F::Read(R::NodePgMigrate),
            MigrationFormat::Alembic => F::Read(R::Alembic),
            MigrationFormat::Django => F::Read(R::Django),
            MigrationFormat::Rails => F::Read(R::Rails),
        }
    }
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

/// What an audit event records (`audit_action`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, sqlx::Type, ToSchema)]
#[sqlx(type_name = "audit_action", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum AuditAction {
    /// An admin removed a version (hidden, restorable for 30 days).
    VersionRemoved,
    VersionRestored,
    /// A removed version's 30 days passed: romulus deleted it.
    VersionPurged,
    /// A draft was published with the approvals the organization requires
    /// (who approved is kept).
    VersionPublished,
    /// An admin published a draft without the approvals it needed.
    ApprovalsOverridden,
    /// An admin changed how many approvals publishing needs.
    ApprovalsRequiredChanged,
    /// A developer moved the database to another Postgres major (`from`,
    /// `to` in the details).
    MajorChanged,
    /// A developer or an admin deleted someone else's comment (or a thread
    /// others wrote in) in a draft's discussion.
    CommentDeleted,
}

/// What a draft is edited as (`draft_mode`): one source of truth at a time
/// (docs/MVP.md "Review round 1", item 2).
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, sqlx::Type, ToSchema,
)]
#[sqlx(type_name = "draft_mode", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum DraftMode {
    /// The schema SQL is the source; its migration is generated.
    #[default]
    Schema,
    /// A migration from the base version is the source, written by hand;
    /// the schema is what it makes of the base version.
    Migration,
}

impl DraftMode {
    /// The wire name, as the doc's `meta.mode` holds it.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Schema => "schema",
            Self::Migration => "migration",
        }
    }
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
    fn migration_formats_keep_the_import_formats_wire_names() {
        let formats: Vec<_> = romulus_sandbox::import::Format::all().collect();
        assert_eq!(formats.len(), 10);
        for format in formats {
            let stored = MigrationFormat::from(format);
            assert_eq!(serde_json::to_value(stored).unwrap(), format.as_str());
            assert_eq!(romulus_sandbox::import::Format::from(stored), format);
        }
    }

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

    #[test]
    fn majors_are_numbers_and_only_supported_ones_are_read() {
        assert_eq!(serde_json::to_string(&PgMajor::DEFAULT).unwrap(), "17");
        assert_eq!(
            serde_json::from_str::<PgMajor>("15").unwrap(),
            PgMajor::SUPPORTED[0]
        );
        let refused = serde_json::from_str::<PgMajor>("14").unwrap_err();
        assert!(
            refused.to_string().contains("15, 16, 17 or 18"),
            "{refused}"
        );
        assert!(serde_json::from_str::<PgMajor>("\"17\"").is_err());
        assert_eq!(PgMajor::new(18).map(PgMajor::number), Some(18));
        assert_eq!(PgMajor::new(19), None);
    }
}
