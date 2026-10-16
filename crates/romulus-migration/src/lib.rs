//! A migration from one remus schema model to another: the `ALTER …`
//! statements that turn a database matching `old` into one matching `new`,
//! in an order Postgres accepts, with what can lose data flagged.
//!
//! [`migration`] plans the statements; [`files`] lays them out for a
//! migration tool ([`Format`]: plain SQL, sqlx, Flyway, Prisma, dbmate,
//! golang-migrate), with a header, the destructive statements marked and,
//! where the tool has one, a best-effort down migration (the migration from
//! `new` back to `old`).
//!
//! Like `remus-core` and `romulus-diff`, this crate has no I/O and no async:
//! it builds for `wasm32-unknown-unknown`. Its correctness oracle lives in
//! `tests/oracle`: `render_sql(old)` followed by the migration, compiled in
//! the sandbox Postgres, must introspect as `new`.
//!
//! ```
//! use remus_core::Schema;
//! use romulus_migration::{Options, migration};
//!
//! let old = Schema::from_json(r#"{
//!     "remus_version": "0.1", "database": "app", "server_version": "17",
//!     "entities": [{
//!         "schema": "public", "name": "users", "kind": "r", "comment": null,
//!         "partition_key": null, "view_definition": null,
//!         "columns": [{
//!             "name": "email", "type": "text", "type_schema": "pg_catalog",
//!             "type_name": "text", "type_kind": "b", "not_null": false,
//!             "default": null, "identity": null, "generated": null, "comment": null
//!         }]
//!     }]
//! }"#)?;
//! let mut new = old.clone();
//! new.entities[0].columns[0].not_null = true;
//!
//! let m = migration(&old, &new, &Options::default());
//! assert_eq!(m.statements[0].sql, "ALTER TABLE public.users ALTER COLUMN email SET NOT NULL;");
//! assert!(m.statements[0].destructive);
//! assert!(migration(&old, &old, &Options::default()).is_empty());
//! # Ok::<(), remus_core::Error>(())
//! ```
//!
//! # What it does not do
//!
//! The model is the schema, not the data, and not everything in a database:
//! functions, triggers, grants, sequences' state and foreign servers are not
//! modelled, so they are neither created nor dropped. A rename is a removal
//! plus an addition (the model has no identity beyond names); when a removed
//! and an added column or table look alike, the statements say so and offer
//! the `RENAME` in a comment. Postgres cannot insert a column in the middle
//! of a table: added columns go last, and a [`WarningCode::ColumnOrder`]
//! warning says where the new model puts them.

mod cast;
mod format;
mod plan;
mod sql;
#[cfg(test)]
mod tests;

pub use format::{File, FileRole, Format, Lines, Meta, files, slug};
use remus_core::Schema;
use serde::{Deserialize, Serialize};

/// Choices that change the statements, not what they do.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct Options {
    /// Build (and rebuild) non-unique indexes on existing tables with
    /// `CREATE INDEX CONCURRENTLY`, which does not block writes but cannot
    /// run inside a transaction: those statements go in
    /// [`Stage::Concurrent`], after the transaction. Indexes on new or
    /// partitioned tables, and unique ones (a foreign key may need them),
    /// stay in the transaction.
    #[serde(default)]
    pub concurrently: bool,
}

/// What [`migration`] returns: the statements, in order, and what a
/// reviewer should know before running them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct Migration {
    /// In the order to run them: [`Stage::Commit`] first, then
    /// [`Stage::Transaction`], then [`Stage::Concurrent`].
    pub statements: Vec<Statement>,
    pub warnings: Vec<Warning>,
}

impl Migration {
    /// Nothing to run (warnings aside).
    pub fn is_empty(&self) -> bool {
        self.statements.is_empty()
    }

    /// How many statements can lose data or fail on existing rows.
    pub fn destructive(&self) -> usize {
        self.statements.iter().filter(|s| s.destructive).count()
    }

    /// Every statement as one script, in order, without the transaction
    /// control the tools add (the sandbox oracle runs exactly this).
    pub fn script(&self) -> String {
        let mut out = String::new();
        for statement in &self.statements {
            out.push_str(&statement.sql);
            out.push('\n');
        }
        out
    }
}

/// One SQL statement of a migration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct Statement {
    /// One statement, ending with `;` (it may span lines).
    pub sql: String,
    pub kind: Kind,
    /// Deletes data (a dropped table, column or partition, a conversion
    /// that can lose or reject values), or fails when existing rows break
    /// it (`SET NOT NULL`, a `NOT NULL` column without a default).
    pub destructive: bool,
    /// What a reviewer should know: why it is there, what it can do to
    /// existing rows, how to do it more safely.
    pub note: Option<String>,
    /// SQL to run instead, when this statement is half of a likely rename
    /// (a `RENAME` keeps the data a drop and an add would lose). Files show
    /// it commented out.
    pub alternative: Option<String>,
    pub stage: Stage,
    /// The part of the migration it belongs to, for headings.
    pub section: Section,
}

/// The parts of a migration, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum Section {
    /// Schemas and extensions the rest needs.
    Schemas,
    /// Dropped first: changed, or in the way of a change (policies, views,
    /// foreign keys, indexes, constraints; tables changing kind).
    Dependents,
    Types,
    /// Tables created and altered, partitions.
    Tables,
    /// Constraints, indexes and foreign keys.
    Keys,
    Views,
    /// Row level security and policies.
    Security,
    Comments,
    /// Columns, tables, types and extensions dropped: last, together.
    Removals,
}

impl Section {
    /// A heading for files and screens.
    pub fn title(self) -> &'static str {
        match self {
            Self::Schemas => "Schemas and extensions",
            Self::Dependents => "Dropped first, to change or to make way",
            Self::Types => "Types",
            Self::Tables => "Tables and columns",
            Self::Keys => "Constraints and indexes",
            Self::Views => "Views",
            Self::Security => "Row level security",
            Self::Comments => "Comments",
            Self::Removals => "Removals",
        }
    }
}

/// When a statement runs relative to the migration's transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    /// Committed before the transaction: enum values the migration then
    /// uses (Postgres refuses a value added in the same transaction).
    Commit,
    /// The migration's transaction.
    Transaction,
    /// After it, one statement at a time: `CREATE INDEX CONCURRENTLY`.
    Concurrent,
}

/// What a statement does, for grouping and icons.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    CreateSchema,
    CreateExtension,
    AlterExtension,
    DropExtension,
    CreateType,
    AlterType,
    DropType,
    CreateDomain,
    AlterDomain,
    DropDomain,
    CreateSequence,
    AlterSequence,
    CreateTable,
    AlterTable,
    DropTable,
    AddColumn,
    AlterColumn,
    DropColumn,
    AddConstraint,
    DropConstraint,
    CreateIndex,
    DropIndex,
    CreatePartition,
    AttachPartition,
    DetachPartition,
    CreateView,
    DropView,
    CreatePolicy,
    AlterPolicy,
    DropPolicy,
    RowLevelSecurity,
    Comment,
}

/// Something about the migration as a whole that a reviewer must know.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct Warning {
    pub code: WarningCode,
    pub message: String,
    /// The object it is about, schema-qualified and unquoted
    /// (`public.users`, `public.mood`), when it is about one.
    pub object: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum WarningCode {
    /// Columns (or composite attributes) end up in another order than the
    /// new model's: Postgres adds them last and cannot reorder them.
    ColumnOrder,
    /// A removed and an added object look alike: maybe a rename.
    PossibleRename,
    /// An enum lost or reordered labels, or a domain changed its base type:
    /// the type is recreated and every column using it converted.
    TypeRecreated,
    /// A table's kind or partition key changed: it is dropped and created
    /// again, empty.
    TableRecreated,
    /// A column converted to a type with no cast from its old one: through
    /// text, which fails unless the values read as the new type.
    Cast,
    /// A domain's CHECK is dropped by the name Postgres gives an unnamed
    /// one; the model does not know the real name.
    ConstraintName,
    /// Foreign tables are not migrated (foreign servers are not modelled).
    ForeignTable,
    /// Something Postgres cannot change in place and the migration does not
    /// attempt (a partition key column's type, a composite in use).
    Unsupported,
    /// A schema holds nothing the model knows of any more; it is not
    /// dropped (it may hold functions).
    EmptySchema,
}

/// The migration from `old` to `new`. No statements when
/// `romulus_diff::diff(old, new)` is empty; a change it cannot make
/// (moving a column, a foreign table) is a warning instead.
pub fn migration(old: &Schema, new: &Schema, options: &Options) -> Migration {
    let old = romulus_diff::normalize(old);
    let new = romulus_diff::normalize(new);
    plan::Planner::new(&old, &new, options).plan()
}
