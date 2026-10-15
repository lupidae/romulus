//! The diff's output types: what the API serialises and the web renders.
//!
//! The JSON shape is part of the contract with the SPA, so every field is
//! always present (`null` or `[]` rather than missing) and enum values are
//! lowercase words, never the catalog's single-letter codes the remus model
//! uses. Lists are sorted the same way whichever schema is called old, which is
//! what makes [`SchemaDiff::reversed`] equal to the diff taken the other way.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Where an object, or a part of one, stands between the old and the new
/// schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Added,
    Removed,
    Changed,
    /// Only in [`StatusMap`]: the change lists leave unchanged things out.
    Unchanged,
}

impl Status {
    pub(crate) fn reversed(self) -> Self {
        match self {
            Self::Added => Self::Removed,
            Self::Removed => Self::Added,
            Self::Changed | Self::Unchanged => self,
        }
    }
}

/// Everything that differs between two schemas, matched by schema and name:
/// a rename is a removal plus an addition.
///
/// Lists hold only what was added, removed or changed. [`SchemaDiff::status`]
/// is the total view (every object of either side, unchanged ones included)
/// for highlighting a graph.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct SchemaDiff {
    pub summary: Summary,
    /// Tables, partitioned tables, views, materialized views and foreign
    /// tables, by schema then name. Partitions are listed under the entity
    /// their chain of parents leads to, not here.
    pub entities: Vec<EntityDiff>,
    pub enums: Vec<EnumDiff>,
    pub domains: Vec<DomainDiff>,
    pub composites: Vec<CompositeDiff>,
    pub extensions: Vec<ExtensionDiff>,
    pub orphan_partitions: OrphanPartitions,
    pub status: StatusMap,
}

/// A schema-qualified relation name, unquoted.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct Relation {
    pub schema: String,
    pub name: String,
}

/// Partitions whose chain of parents reaches no entity in either schema: the
/// root table was left out of the model (a schema filter that kept a
/// sub-partition's schema but not its root's), or the model was written by
/// hand. They are matched like any partition and described here instead of
/// under an entity.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct OrphanPartitions {
    /// By schema then name.
    pub partitions: Vec<PartitionDiff>,
    pub lines: Vec<String>,
    /// Every orphan partition of each side as `remus-sql` renders it; `null`
    /// on a side with none, and on both sides when nothing changed.
    pub sql: Sides,
}

/// Counts per category, and the same as one sentence.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct Summary {
    /// Categories with at least one change, in [`Category`] order.
    pub counts: Vec<Count>,
    /// `"3 tables added, 1 changed; 1 enum changed"`, or `"no changes"`.
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct Count {
    pub category: Category,
    pub added: usize,
    pub removed: usize,
    pub changed: usize,
}

/// What [`Summary`] counts. Partitioned tables count as tables; an entity whose
/// kind changed counts under its new kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum Category {
    Table,
    View,
    MaterializedView,
    ForeignTable,
    Partition,
    Enum,
    Domain,
    Composite,
    Extension,
}

/// Status of every object on either side, unchanged ones included.
///
/// Schema-qualified objects are nested by schema, then name, both unquoted
/// exactly as in the model: `status.entities["shop"]["orders"]`. Nesting keeps
/// keys collision-free without making the web reproduce Postgres's quoting
/// rules (a flat `schema.name` key cannot tell schema `a.b` + table `c` from
/// schema `a` + table `b.c`). Extensions are database-wide, keyed by name.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct StatusMap {
    pub entities: BTreeMap<String, BTreeMap<String, EntityStatus>>,
    /// See [`OrphanPartitions`].
    pub orphan_partitions: BTreeMap<String, BTreeMap<String, Status>>,
    pub enums: BTreeMap<String, BTreeMap<String, Status>>,
    pub domains: BTreeMap<String, BTreeMap<String, Status>>,
    pub composites: BTreeMap<String, BTreeMap<String, Status>>,
    pub extensions: BTreeMap<String, Status>,
}

impl StatusMap {
    pub fn entity(&self, schema: &str, name: &str) -> Option<&EntityStatus> {
        self.entities.get(schema)?.get(name)
    }
}

/// One graph node: the entity's status and that of every part it has on
/// either side. Foreign keys are the graph's edges. Columns, constraints,
/// indexes and policies are keyed by name; partitions (every level under this
/// entity) by schema, then name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct EntityStatus {
    pub status: Status,
    pub columns: BTreeMap<String, Status>,
    pub constraints: BTreeMap<String, Status>,
    pub indexes: BTreeMap<String, Status>,
    pub policies: BTreeMap<String, Status>,
    pub partitions: BTreeMap<String, BTreeMap<String, Status>>,
}

/// The same thing on both sides, `null` where it does not exist.
///
/// For an object it is the DDL `remus-sql` renders for it alone (statements
/// separated by blank lines, ending with a newline), so the web can show the
/// two side by side. For a part of an object it is the one-line definition:
/// a column's type and clauses, a constraint's `pg_get_constraintdef`, an
/// index's `CREATE INDEX`, a policy's clauses after `ON table`, a partition's
/// bound.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct Sides {
    #[cfg_attr(feature = "utoipa", schema(required = true))]
    pub before: Option<String>,
    #[cfg_attr(feature = "utoipa", schema(required = true))]
    pub after: Option<String>,
}

impl Sides {
    pub(crate) fn reversed(self) -> Self {
        Self {
            before: self.after,
            after: self.before,
        }
    }
}

/// 0-based index in the old and the new list, `null` on the side the item is
/// absent from. Every item carries both, moved or not.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct Positions {
    #[cfg_attr(feature = "utoipa", schema(required = true))]
    pub before: Option<usize>,
    #[cfg_attr(feature = "utoipa", schema(required = true))]
    pub after: Option<usize>,
}

impl Positions {
    pub(crate) fn reversed(self) -> Self {
        Self {
            before: self.after,
            after: self.before,
        }
    }
}

/// Defines a field-change enum: serialised as
/// `{"field": "<variant in snake_case>", "before": …, "after": …}`, so the SPA
/// gets a discriminated union and can render any change as `before → after`.
macro_rules! changes {
    ($(#[$meta:meta])* $name:ident { $($(#[$vmeta:meta])* $variant:ident($ty:ty),)* }) => {
        $(#[$meta])*
        ///
        /// Every variant carries the old value as `before` and the new one as
        /// `after`.
        #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
        #[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
        #[serde(tag = "field", rename_all = "snake_case")]
        pub enum $name {
            $($(#[$vmeta])* $variant {
                #[cfg_attr(feature = "utoipa", schema(required = true))]
                before: $ty,
                #[cfg_attr(feature = "utoipa", schema(required = true))]
                after: $ty,
            },)*
        }

        impl $name {
            pub(crate) fn reversed(self) -> Self {
                match self {
                    $(Self::$variant { before, after } => Self::$variant {
                        before: after,
                        after: before,
                    },)*
                }
            }
        }
    };
}

/// `pg_class.relkind`, as a word.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum EntityKind {
    Table,
    PartitionedTable,
    View,
    MaterializedView,
    ForeignTable,
}

/// A table, view, materialized view or foreign table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct EntityDiff {
    pub schema: String,
    pub name: String,
    /// The new kind, or the old one when the entity was removed.
    pub kind: EntityKind,
    pub status: Status,
    /// Changes to the entity itself. Empty when added or removed.
    pub changes: Vec<EntityChange>,
    /// Every column of an added or removed entity; the added, removed and
    /// changed ones otherwise. In table order (see [`ColumnDiff`]).
    pub columns: Vec<ColumnDiff>,
    /// By name, like the next three.
    pub constraints: Vec<ConstraintDiff>,
    pub indexes: Vec<IndexDiff>,
    pub policies: Vec<PolicyDiff>,
    /// Partitions of a partitioned table, at every level (a sub-partition is
    /// not an entity, so its own partitions are listed here too), by schema
    /// then name.
    pub partitions: Vec<PartitionDiff>,
    /// One human-readable line per change, ready for a change list.
    pub lines: Vec<String>,
    /// Table, sequences it owns, partitions, foreign keys, indexes, row level
    /// security and comments; or the view.
    pub sql: Sides,
}

changes! {
    /// A change to an entity's own attributes.
    EntityChange {
        Kind(EntityKind),
        Comment(Option<String>),
        /// Row level security enabled.
        Rls(bool),
        /// Row level security forced on the table owner too.
        RlsForced(bool),
        Unlogged(bool),
        /// `PARTITION BY` clause, without the keywords.
        PartitionKey(Option<String>),
        /// A view's `SELECT`, as `pg_get_viewdef` prints it. Compared with
        /// whitespace outside quotes ignored, since Postgres majors indent it
        /// differently; reported raw.
        ViewDefinition(Option<String>),
        /// Relations a view reads from, sorted.
        DependsOn(Vec<Relation>),
    }
}

/// A column. Columns are listed in table order: by their smallest position on
/// either side, then their largest, then name, which reads naturally and does
/// not depend on which side is old.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct ColumnDiff {
    pub name: String,
    pub status: Status,
    pub position: Positions,
    /// Empty when added or removed.
    pub changes: Vec<ColumnChange>,
    /// Type and clauses as in `CREATE TABLE`, without the name:
    /// `bigint GENERATED ALWAYS AS IDENTITY NOT NULL`.
    pub definition: Sides,
}

/// `pg_type.typtype` of a column's type, as a word.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum TypeKind {
    Base,
    Enum,
    Domain,
    Composite,
    Range,
    Multirange,
    Pseudo,
}

/// `pg_attribute.attidentity`, as a word.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum Identity {
    Always,
    ByDefault,
}

/// `pg_attribute.attgenerated`, as a word.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum Generated {
    Stored,
    Virtual,
}

changes! {
    /// A change to one column.
    ColumnChange {
        /// As `format_type` prints it.
        Type(String),
        /// `type_schema.type_name`, with `[]` for an array: reported instead
        /// of [`ColumnChange::Type`] when the type prints the same but is
        /// another type (`email` from another schema on the search path).
        TypeName(String),
        /// Same type text, different kind of type (a domain replaced by an
        /// enum of the same name).
        TypeKind(TypeKind),
        NotNull(bool),
        /// Default expression. For a column generated on both sides the
        /// expression is reported as [`ColumnChange::Expression`] instead.
        Default(Option<String>),
        /// Generation expression of a column generated on both sides.
        Expression(Option<String>),
        Identity(Option<Identity>),
        Generated(Option<Generated>),
        Comment(Option<String>),
        /// The column moved relative to the others. Only for real moves: an
        /// insertion before a column does not move it (positions are compared
        /// through the longest common ordering of the shared columns).
        Position(usize),
    }
}

/// `pg_constraint.contype`, as a word.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum ConstraintKind {
    PrimaryKey,
    Unique,
    ForeignKey,
    Check,
    Exclusion,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct ConstraintDiff {
    pub name: String,
    /// The new kind, or the old one when the constraint was removed.
    pub kind: ConstraintKind,
    pub status: Status,
    pub changes: Vec<ConstraintChange>,
    /// `pg_get_constraintdef`: `FOREIGN KEY (customer_id) REFERENCES shop.customers(id)`.
    pub definition: Sides,
}

changes! {
    /// A change to one constraint.
    ConstraintChange {
        Kind(ConstraintKind),
        /// `pg_get_constraintdef`, compared with whitespace outside quotes
        /// ignored and reported raw. Also reported when only a structural
        /// field behind the same text differs (columns, target, actions,
        /// deferrable).
        Definition(String),
        Comment(Option<String>),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct IndexDiff {
    pub name: String,
    pub status: Status,
    pub changes: Vec<IndexChange>,
    /// `pg_get_indexdef`, a complete `CREATE INDEX` statement.
    pub definition: Sides,
}

changes! {
    /// A change to one index.
    IndexChange {
        /// `pg_get_indexdef`. Also reported when only the unique or partial
        /// flag behind the same text differs.
        Definition(String),
    }
}

/// `pg_policy.polcmd`, as a word.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum PolicyCommand {
    All,
    Select,
    Insert,
    Update,
    Delete,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct PolicyDiff {
    pub name: String,
    pub status: Status,
    pub changes: Vec<PolicyChange>,
    /// What follows `CREATE POLICY name ON table`:
    /// `AS RESTRICTIVE FOR SELECT TO reader USING (…)`.
    pub definition: Sides,
}

changes! {
    /// A change to one row level security policy.
    PolicyChange {
        Command(PolicyCommand),
        Permissive(bool),
        /// Sorted; empty means `PUBLIC`.
        Roles(Vec<String>),
        Using(Option<String>),
        Check(Option<String>),
    }
}

/// A partition, matched by its own schema and name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct PartitionDiff {
    pub schema: String,
    pub name: String,
    /// The table it is a partition of (the entity, or a partition that is
    /// itself partitioned): the new one, or the old one when removed.
    pub parent: Relation,
    pub status: Status,
    pub changes: Vec<PartitionChange>,
    /// `FOR VALUES …` or `DEFAULT`.
    pub definition: Sides,
}

changes! {
    /// A change to one partition.
    PartitionChange {
        /// Attached to another table of the same tree.
        Parent(Relation),
        /// `FOR VALUES …`, or `DEFAULT`. A `null` bound means `DEFAULT` too
        /// and never differs from it.
        Bound(Option<String>),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct EnumDiff {
    pub schema: String,
    pub name: String,
    pub status: Status,
    pub changes: Vec<EnumChange>,
    /// Every label of an added or removed enum; the added, removed and moved
    /// ones otherwise. Ordered like [`ColumnDiff`]s.
    pub labels: Vec<LabelDiff>,
    pub lines: Vec<String>,
    pub sql: Sides,
}

changes! {
    /// A change to an enum other than its labels.
    EnumChange {
        Comment(Option<String>),
    }
}

/// One enum label. `changed` means moved: its order relative to the labels
/// present on both sides changed, which matters because enums compare by
/// declaration order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct LabelDiff {
    pub label: String,
    pub status: Status,
    pub position: Positions,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct DomainDiff {
    pub schema: String,
    pub name: String,
    pub status: Status,
    pub changes: Vec<DomainChange>,
    pub lines: Vec<String>,
    pub sql: Sides,
}

changes! {
    /// A change to a domain.
    DomainChange {
        BaseType(String),
        NotNull(bool),
        Default(Option<String>),
        /// `CHECK (…)` clauses, sorted. The catalog's constraint names are not
        /// in the model, so checks compare by text, whitespace outside quotes
        /// ignored.
        Constraints(Vec<String>),
        Comment(Option<String>),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct CompositeDiff {
    pub schema: String,
    pub name: String,
    pub status: Status,
    pub changes: Vec<CompositeChange>,
    /// Ordered like [`ColumnDiff`]s.
    pub attributes: Vec<AttributeDiff>,
    pub lines: Vec<String>,
    pub sql: Sides,
}

changes! {
    /// A change to a composite type other than its attributes.
    CompositeChange {
        Comment(Option<String>),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct AttributeDiff {
    pub name: String,
    pub status: Status,
    pub position: Positions,
    pub changes: Vec<AttributeChange>,
    /// The attribute's type.
    pub definition: Sides,
}

changes! {
    /// A change to one attribute of a composite type.
    AttributeChange {
        Type(String),
        /// Moved relative to the other attributes, as for columns.
        Position(usize),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct ExtensionDiff {
    pub name: String,
    pub status: Status,
    pub changes: Vec<ExtensionChange>,
    pub lines: Vec<String>,
    pub sql: Sides,
}

changes! {
    /// A change to an installed extension.
    ExtensionChange {
        Schema(String),
        Version(String),
    }
}
