//! Semantic diff between two remus schema models.
//!
//! [`diff`] compares two [`Schema`]s object by object, matched by schema and
//! name (a rename is a removal plus an addition), and returns a
//! [`SchemaDiff`] that serves the three views of a version diff:
//!
//! - **a change list**, grouped per object: [`SchemaDiff::entities`],
//!   [`SchemaDiff::enums`], … each with structured changes and ready-made
//!   `lines` of text;
//! - **a highlighted graph**: [`SchemaDiff::status`], the status of every
//!   object of either side and of every column, constraint, index, policy and
//!   partition of every entity, nested by schema then name (see
//!   [`StatusMap`]);
//! - **a side-by-side SQL diff**: each changed object's `sql.before` /
//!   `sql.after`, rendered by `remus-sql`, and [`render_sql`] for whole
//!   schemas in a consistent order.
//!
//! Like `remus-core`, this crate has no I/O and no async, so it builds for
//! `wasm32-unknown-unknown`.
//!
//! ```
//! use remus_core::Schema;
//! use romulus_diff::{Status, diff};
//!
//! let old = Schema::from_json(r#"{
//!     "remus_version": "0.1", "database": "app", "server_version": "16",
//!     "entities": [{
//!         "schema": "public", "name": "users", "kind": "r", "comment": null,
//!         "partition_key": null, "view_definition": null,
//!         "columns": [{
//!             "name": "email", "type": "text", "type_schema": "pg_catalog",
//!             "type_name": "text", "type_kind": "b", "not_null": true,
//!             "default": null, "identity": null, "generated": null, "comment": null
//!         }]
//!     }]
//! }"#)?;
//! let mut new = old.clone();
//! let email = &mut new.entities[0].columns[0];
//! email.r#type = "citext".into();
//! email.type_schema = "public".into();
//! email.type_name = "citext".into();
//!
//! let d = diff(&old, &new);
//! assert_eq!(d.summary.text, "1 table changed");
//! assert_eq!(d.entities[0].lines, ["`users.email`: type text → citext"]);
//! assert_eq!(d.status.entities["public"]["users"].columns["email"], Status::Changed);
//! assert!(diff(&old, &old).is_empty());
//! assert_eq!(diff(&new, &old), d.reversed());
//! # Ok::<(), remus_core::Error>(())
//! ```
//!
//! # JSON shape
//!
//! [`SchemaDiff`] serialises as below (abridged: one entity, one column). Every
//! field is always present; absent values are `null`, empty lists `[]`.
//! Statuses are `added | removed | changed | unchanged`; kinds are words
//! (`table`, `partitioned_table`, `foreign_key`, …), not catalog letters. A
//! structured change is `{"field": …, "before": …, "after": …}`, its `field`
//! naming the model field, so the SPA can switch on it or print any change as
//! `before → after`.
//!
//! ```json
//! {
//!   "summary": {
//!     "counts": [{ "category": "table", "added": 0, "removed": 0, "changed": 1 }],
//!     "text": "1 table changed"
//!   },
//!   "entities": [{
//!     "schema": "public", "name": "users", "kind": "table", "status": "changed",
//!     "changes": [],
//!     "columns": [{
//!       "name": "email", "status": "changed",
//!       "position": { "before": 0, "after": 0 },
//!       "changes": [{ "field": "type", "before": "text", "after": "citext" }],
//!       "definition": { "before": "text NOT NULL", "after": "citext NOT NULL" }
//!     }],
//!     "constraints": [], "indexes": [], "policies": [], "partitions": [],
//!     "lines": ["`users.email`: type text → citext"],
//!     "sql": {
//!       "before": "CREATE TABLE public.users (\n    email text NOT NULL\n);\n",
//!       "after": "CREATE TABLE public.users (\n    email citext NOT NULL\n);\n"
//!     }
//!   }],
//!   "enums": [], "domains": [], "composites": [], "extensions": [],
//!   "orphan_partitions": { "partitions": [], "lines": [], "sql": { "before": null, "after": null } },
//!   "status": {
//!     "entities": {
//!       "public": {
//!         "users": {
//!           "status": "changed",
//!           "columns": { "email": "changed" },
//!           "constraints": {}, "indexes": {}, "policies": {}, "partitions": {}
//!         }
//!       }
//!     },
//!     "orphan_partitions": {}, "enums": {}, "domains": {}, "composites": {},
//!     "extensions": {}
//!   }
//! }
//! ```
//!
//! # Keys
//!
//! Objects are identified by schema and name, unquoted, exactly as in the
//! model. The status maps nest them, `status.entities[schema][name]`, rather
//! than joining them into `schema.name`, which is ambiguous when a name holds
//! a dot. Inside an entity, columns, constraints, indexes and policies are
//! keyed by name and partitions by schema then name; extensions, which are
//! database-wide, by name. Relations inside changes (a partition's parent, a
//! view's dependencies) are `{"schema", "name"}` objects.
//!
//! # Partitions
//!
//! remus lists no partition among the entities, sub-partitioned ones
//! included, so a partition of a partition names a parent that is not an
//! entity. Each partition is listed under the entity at the top of its tree,
//! found by following parents, with its own immediate `parent`; the entity
//! counts as changed when any partition under it does. The rare partition
//! whose parents lead to no entity is in [`SchemaDiff::orphan_partitions`].
//!
//! Lists are sorted by name (columns, enum labels and composite attributes in
//! table order, see [`ColumnDiff`]) and the order does not depend on which
//! side is old, so `diff(b, a)` is exactly `diff(a, b).reversed()`.

mod compute;
mod describe;
mod model;
mod order;
mod reverse;
mod sql;
#[cfg(test)]
mod tests;

pub use model::{
    AttributeChange, AttributeDiff, Category, ColumnChange, ColumnDiff, CompositeChange,
    CompositeDiff, ConstraintChange, ConstraintDiff, ConstraintKind, Count, DomainChange,
    DomainDiff, EntityChange, EntityDiff, EntityKind, EntityStatus, EnumChange, EnumDiff,
    ExtensionChange, ExtensionDiff, Generated, Identity, IndexChange, IndexDiff, LabelDiff,
    OrphanPartitions, PartitionChange, PartitionDiff, PolicyChange, PolicyCommand, PolicyDiff,
    Positions, Relation, SchemaDiff, Sides, Status, StatusMap, Summary, TypeKind,
};
use remus_core::Schema;
pub use sql::{normalize, render_sql};

/// Everything that differs from `old` to `new`.
///
/// Every field of the model is compared, so the diff is empty exactly when
/// the two schemas are equal after [`normalize`], with three exceptions that
/// differ between environments without the schema differing: metadata
/// (database name, server and remus versions, timestamp), and whitespace
/// outside quotes in view definitions, constraint definitions and domain
/// checks, which Postgres majors deparse differently. That makes
/// `diff(a, b).is_empty()` a sounder "unchanged" check than comparing
/// [`Schema::canonical`] outputs, which also differ when the catalog merely
/// returned a list in another order.
pub fn diff(old: &Schema, new: &Schema) -> SchemaDiff {
    compute::schema_diff(old, new)
}

impl SchemaDiff {
    /// Nothing was added, removed or changed.
    pub fn is_empty(&self) -> bool {
        self.entities.is_empty()
            && self.enums.is_empty()
            && self.domains.is_empty()
            && self.composites.is_empty()
            && self.extensions.is_empty()
            && self.orphan_partitions.partitions.is_empty()
    }

    /// Every object's lines, entities first, then orphan partitions, enums,
    /// domains, composite types and extensions: the whole change list as
    /// plain text, for a commit message or a CI log.
    pub fn lines(&self) -> impl Iterator<Item = &str> {
        let entities = self.entities.iter().flat_map(|e| &e.lines);
        let orphans = self.orphan_partitions.lines.iter();
        let enums = self.enums.iter().flat_map(|e| &e.lines);
        let domains = self.domains.iter().flat_map(|d| &d.lines);
        let composites = self.composites.iter().flat_map(|c| &c.lines);
        let extensions = self.extensions.iter().flat_map(|x| &x.lines);
        entities
            .chain(orphans)
            .chain(enums)
            .chain(domains)
            .chain(composites)
            .chain(extensions)
            .map(String::as_str)
    }
}
