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
//! # Renames
//!
//! The model has no identity beyond names, so a renamed column reads as one
//! removed and one added. [`romulus_diff::resolve`] decides which pairs are
//! renames ([`Options::renames`]: the ones known to have happened, the ones
//! detected and confident enough, the ones a person turned on); the
//! migration runs them first (`ALTER … RENAME`, [`Section::Renames`]) and
//! plans the rest from the old model as Postgres has it after them, so a
//! renamed column that also changes type is converted, never dropped and
//! added, and what depends on it (keys, indexes, views, policies) follows
//! the rename instead of being built again. [`Migration::renames`] lists
//! every candidate, applied or not, for a person to choose; the statements
//! a candidate left off would replace show it as their `alternative`.
//! [`down`] mirrors a migration's renames exactly.
//!
//! # What it does not do
//!
//! The model is the schema, not the data, and not everything in a database:
//! functions, triggers, grants, sequences' state and foreign servers are not
//! modelled, so they are neither created nor dropped (nor renamed). Postgres
//! cannot insert a column in the middle of a table: added columns go last,
//! and a [`WarningCode::ColumnOrder`] warning says where the new model puts
//! them.

mod cast;
mod extensions;
mod format;
mod partitions;
mod plan;
mod sql;
#[cfg(test)]
mod tests;
mod versions;

pub use extensions::{
    PROVIDED as EXTENSION_OBJECTS, REQUIRES as EXTENSION_REQUIREMENTS,
    without_provided as without_extension_objects,
};
pub use format::{File, FileRole, Format, Mark, Meta, archive_name, editable, files, slug};
use remus_core::Schema;
pub use romulus_diff::{Candidate, Confidence, Rename, RenameChoices, RenameKind};
use serde::{Deserialize, Serialize};

/// Choices that change the statements, not what they do.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema), schema(as = MigrationOptions))]
pub struct Options {
    /// Build (and rebuild) non-unique indexes on existing tables with
    /// `CREATE INDEX CONCURRENTLY`, which does not block writes but cannot
    /// run inside a transaction: those statements go in
    /// [`Stage::Concurrent`], after the transaction. Indexes on new or
    /// partitioned tables, and unique ones (a foreign key may need them),
    /// stay in the transaction.
    #[serde(default)]
    pub concurrently: bool,
    /// The Postgres major the migration runs on (the database's); `None`:
    /// any, the newest. Statements then use what it has (a generated
    /// column's new expression is `SET EXPRESSION` from 17 on, the column
    /// dropped and added again before), and what the new model needs that
    /// it lacks is a [`WarningCode::NewerPostgres`] warning.
    #[serde(default)]
    pub major: Option<u32>,
    /// Which removed and added objects are renames: the known ones, the
    /// decisions taken on detected ones, whether to detect at all.
    #[serde(default)]
    pub renames: RenameChoices,
}

impl Options {
    /// Whether the target major has what `since` brought (any, without one).
    pub(crate) fn has(&self, since: u32) -> bool {
        self.major.is_none_or(|major| major >= since)
    }
}

/// What [`migration`] returns: the statements, in order, and what a
/// reviewer should know before running them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema), schema(as = Migration))]
pub struct Migration {
    /// In the order to run them: [`Stage::Commit`] first, then
    /// [`Stage::Transaction`], then [`Stage::Concurrent`].
    pub statements: Vec<Statement>,
    pub warnings: Vec<Warning>,
    /// The Postgres major it was planned for ([`Options::major`]); the
    /// files' header names it.
    #[serde(default)]
    pub major: Option<u32>,
    /// Every rename considered: known ones, then detected ones, applied or
    /// not. A person turns them on or off by key
    /// ([`RenameChoices::decisions`]).
    #[serde(default)]
    pub renames: Vec<Candidate>,
    /// The renames it runs, from the old name to the final one, in order:
    /// what [`down`] reverses, and what a version keeps of the migration it
    /// was published with.
    #[serde(default)]
    pub applied: Vec<Rename>,
    /// Written by hand (a draft's in migration mode), not planned: its
    /// statements as written, in the author's order, [`Kind::Written`] and
    /// unflagged. Its files say so ([`files`]) and keep that order.
    #[serde(default)]
    pub written: bool,
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

    /// How many statements carry `hazard`.
    pub fn count(&self, hazard: Hazard) -> usize {
        self.statements
            .iter()
            .filter(|s| s.hazard == Some(hazard))
            .count()
    }

    /// Every statement as one script, in order, without the transaction
    /// control the tools add.
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
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema), schema(as = MigrationStatement))]
pub struct Statement {
    /// One statement, ending with `;` (it may span lines).
    pub sql: String,
    pub kind: Kind,
    /// What running it can do beyond changing the schema; `null` when
    /// nothing.
    pub hazard: Option<Hazard>,
    /// `hazard` is `deletes_data` or `can_fail`: existing data is at stake.
    pub destructive: bool,
    /// What a reviewer should know: why it is there, what it can do to
    /// existing rows, how to do it more safely.
    pub note: Option<String>,
    /// SQL to run instead, when this statement is half of a rename left
    /// off (a `RENAME` keeps the data a drop and an add would lose). Files
    /// show it commented out.
    pub alternative: Option<String>,
    pub stage: Stage,
    /// The part of the migration it belongs to, for headings.
    pub section: Section,
}

/// The parts of a migration, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema), schema(as = MigrationSection))]
#[serde(rename_all = "snake_case")]
pub enum Section {
    /// Schemas and extensions the rest needs.
    Schemas,
    /// `ALTER … RENAME`: before anything else, so the rest names objects
    /// as they are called from then on.
    Renames,
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
    /// A migration written by hand ([`Migration::written`]): one part, in
    /// its own order, without headings.
    Written,
}

impl Section {
    /// A heading for files and screens.
    pub fn title(self) -> &'static str {
        match self {
            Self::Schemas => "Schemas and extensions",
            Self::Renames => "Renames",
            Self::Dependents => "Dropped first, to change or to make way",
            Self::Types => "Types",
            Self::Tables => "Tables and columns",
            Self::Keys => "Constraints and indexes",
            Self::Views => "Views",
            Self::Security => "Row level security",
            Self::Comments => "Comments",
            Self::Removals => "Removals",
            Self::Written => "Written by hand",
        }
    }
}

/// What a statement risks, beyond changing the schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema), schema(as = MigrationHazard))]
#[serde(rename_all = "snake_case")]
pub enum Hazard {
    /// Deletes data, or changes it: a table, column or partition dropped, a
    /// conversion that rounds or cuts values, a column that becomes
    /// generated.
    DeletesData,
    /// Fails when existing rows break it: `SET NOT NULL`, a constraint or a
    /// unique index added to a table that has rows, a conversion that
    /// rejects values, a partition attached.
    CanFail,
    /// Lets more rows through to more roles: row level security disabled
    /// or no longer forced, a restrictive policy dropped, a permissive
    /// policy added or changed.
    WidensAccess,
}

/// When a statement runs relative to the migration's transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema), schema(as = MigrationStage))]
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
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema), schema(as = MigrationStatementKind))]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    CreateSchema,
    /// `ALTER … RENAME` of a table, column, constraint, index, type, label…
    Rename,
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
    DropSequence,
    /// `SELECT setval(…)`: moves a sequence past the rows a table already
    /// has (data, not schema: the only statement that is not DDL).
    SetSequence,
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
    /// A statement written by hand ([`Migration::written`]): neither
    /// planned nor flagged by romulus.
    Written,
}

/// Something about the migration as a whole that a reviewer must know.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema), schema(as = MigrationWarning))]
pub struct Warning {
    pub code: WarningCode,
    pub message: String,
    /// The object it is about, schema-qualified and unquoted
    /// (`public.users`, `public.mood`), when it is about one.
    pub object: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema), schema(as = MigrationWarningCode))]
#[serde(rename_all = "snake_case")]
pub enum WarningCode {
    /// Columns (or composite attributes) end up in another order than the
    /// new model's: Postgres adds them last and cannot reorder them.
    ColumnOrder,
    /// A removed and an added object look alike, and the rename is off:
    /// dropped and added, the data with it.
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
    /// Statements that widen who can read or write which rows.
    WidensAccess,
    /// Foreign tables are not migrated (foreign servers are not modelled).
    ForeignTable,
    /// Something Postgres cannot change in place and the migration does not
    /// attempt (a partition key column's type, a composite in use).
    Unsupported,
    /// A schema holds nothing the model knows of any more; it is not
    /// dropped (it may hold functions).
    EmptySchema,
    /// A partition becomes a table: the names of its keys and indexes are
    /// Postgres's defaults, assumed (the model does not list a partition's).
    GuessedNames,
    /// A statement needs a newer Postgres than the migration's
    /// [`Options::major`] (`NULLS NOT DISTINCT` is 15+, an exclusion
    /// constraint on a partitioned table 17+, a virtual generated column
    /// 18+…): it is kept, and fails on that server.
    NewerPostgres,
    /// Columns, tables, views or types renamed, or columns converted:
    /// function and trigger bodies are not in the model, and Postgres does
    /// not rewrite them, so any naming those may break when they next run.
    FunctionBodies,
}

/// The migration from `old` to `new`. No statements when
/// `romulus_diff::diff(old, new)` is empty; a change it cannot make
/// (moving a column, a foreign table) is a warning instead.
pub fn migration(old: &Schema, new: &Schema, options: &Options) -> Migration {
    let mut old = romulus_diff::normalize(old);
    let mut new = romulus_diff::normalize(new);
    extensions::without_provided(&mut old);
    extensions::without_provided(&mut new);
    let resolved = romulus_diff::resolve(&old, &new, &options.renames);
    let mut renamed = romulus_diff::normalize(&resolved.schema);
    let moves = partitions::reconcile(&mut renamed, &mut new);
    plan::Planner::new(&renamed, &new, options, moves, &resolved, &old).plan()
}

/// `schema` without the foreign keys Postgres makes itself: the copies, one
/// per partition, of a key referencing a partitioned table (`…_fkey1`).
/// They come with the key they copy, and SQL creating them fails ("already
/// exists"): what renders a model as SQL leaves them out.
pub fn without_fk_clones(schema: &mut Schema) {
    let clones: Vec<(usize, String)> = schema
        .entities
        .iter()
        .enumerate()
        .flat_map(|(i, e)| {
            e.foreign_keys()
                .filter(|fk| plan::fk_clone(schema, e, fk))
                .map(move |fk| (i, fk.name.clone()))
        })
        .collect();
    for (i, name) in clones {
        schema.entities[i].constraints.retain(|c| c.name != name);
    }
}

/// The migration back from `new` to `old` that mirrors `up` (planned from
/// `old` to `new`): its renames undone, last first, nothing else taken for
/// one. Planned for the same major, without `CONCURRENTLY`.
pub fn down(old: &Schema, new: &Schema, up: &Migration) -> Migration {
    migration(
        new,
        old,
        &Options {
            concurrently: false,
            major: up.major,
            renames: RenameChoices::reverse_of(&up.applied),
        },
    )
}
