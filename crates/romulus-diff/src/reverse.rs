//! [`SchemaDiff::reversed`]: the same diff read from the other side.

use std::collections::BTreeMap;

use crate::model::{
    AttributeDiff, ColumnDiff, CompositeDiff, ConstraintChange, ConstraintDiff, DomainDiff,
    EntityChange, EntityDiff, EntityStatus, EnumDiff, ExtensionDiff, IndexDiff, LabelDiff,
    OrphanPartitions, PartitionChange, PartitionDiff, PolicyDiff, SchemaDiff, Status, StatusMap,
    Summary,
};

impl SchemaDiff {
    /// The diff from `new` to `old`: additions become removals, every
    /// `before` swaps with its `after`, and lines and summary are rewritten.
    ///
    /// Equal to `diff(new, old)`; the crate's property tests hold it to that.
    pub fn reversed(self) -> Self {
        let mut out = Self {
            summary: Summary::default(),
            entities: self
                .entities
                .into_iter()
                .map(EntityDiff::reversed)
                .collect(),
            enums: self.enums.into_iter().map(EnumDiff::reversed).collect(),
            domains: self.domains.into_iter().map(DomainDiff::reversed).collect(),
            composites: self
                .composites
                .into_iter()
                .map(CompositeDiff::reversed)
                .collect(),
            extensions: self
                .extensions
                .into_iter()
                .map(ExtensionDiff::reversed)
                .collect(),
            orphan_partitions: OrphanPartitions {
                partitions: each(self.orphan_partitions.partitions, PartitionDiff::reversed),
                lines: Vec::new(),
                sql: self.orphan_partitions.sql.reversed(),
            },
            status: self.status.reversed(),
        };
        out.finish();
        out
    }
}

fn each<T>(items: Vec<T>, f: fn(T) -> T) -> Vec<T> {
    items.into_iter().map(f).collect()
}

impl EntityDiff {
    fn reversed(self) -> Self {
        let changes = each(self.changes, EntityChange::reversed);
        // `kind` is the new side's: after swapping, that is the old kind.
        let kind = changes
            .iter()
            .find_map(|c| match c {
                EntityChange::Kind { after, .. } => Some(*after),
                _ => None,
            })
            .unwrap_or(self.kind);
        Self {
            kind,
            status: self.status.reversed(),
            changes,
            columns: each(self.columns, ColumnDiff::reversed),
            constraints: each(self.constraints, ConstraintDiff::reversed),
            indexes: each(self.indexes, IndexDiff::reversed),
            policies: each(self.policies, PolicyDiff::reversed),
            partitions: each(self.partitions, PartitionDiff::reversed),
            lines: Vec::new(),
            sql: self.sql.reversed(),
            ..self
        }
    }
}

impl ColumnDiff {
    fn reversed(self) -> Self {
        Self {
            status: self.status.reversed(),
            position: self.position.reversed(),
            changes: each(self.changes, |c| c.reversed()),
            definition: self.definition.reversed(),
            ..self
        }
    }
}

impl ConstraintDiff {
    fn reversed(self) -> Self {
        let changes = each(self.changes, ConstraintChange::reversed);
        let kind = changes
            .iter()
            .find_map(|c| match c {
                ConstraintChange::Kind { after, .. } => Some(*after),
                _ => None,
            })
            .unwrap_or(self.kind);
        Self {
            kind,
            status: self.status.reversed(),
            changes,
            definition: self.definition.reversed(),
            ..self
        }
    }
}

impl IndexDiff {
    fn reversed(self) -> Self {
        Self {
            status: self.status.reversed(),
            changes: each(self.changes, |c| c.reversed()),
            definition: self.definition.reversed(),
            ..self
        }
    }
}

impl PolicyDiff {
    fn reversed(self) -> Self {
        Self {
            status: self.status.reversed(),
            changes: each(self.changes, |c| c.reversed()),
            definition: self.definition.reversed(),
            ..self
        }
    }
}

impl PartitionDiff {
    fn reversed(self) -> Self {
        let changes = each(self.changes, PartitionChange::reversed);
        let parent = changes
            .iter()
            .find_map(|c| match c {
                PartitionChange::Parent { after, .. } => Some(after.clone()),
                PartitionChange::Bound { .. } | PartitionChange::PartitionKey { .. } => None,
            })
            .unwrap_or(self.parent);
        Self {
            parent,
            status: self.status.reversed(),
            changes,
            definition: self.definition.reversed(),
            ..self
        }
    }
}

impl EnumDiff {
    fn reversed(self) -> Self {
        Self {
            status: self.status.reversed(),
            changes: each(self.changes, |c| c.reversed()),
            labels: each(self.labels, LabelDiff::reversed),
            lines: Vec::new(),
            sql: self.sql.reversed(),
            ..self
        }
    }
}

impl LabelDiff {
    fn reversed(self) -> Self {
        Self {
            status: self.status.reversed(),
            position: self.position.reversed(),
            ..self
        }
    }
}

impl DomainDiff {
    fn reversed(self) -> Self {
        Self {
            status: self.status.reversed(),
            changes: each(self.changes, |c| c.reversed()),
            lines: Vec::new(),
            sql: self.sql.reversed(),
            ..self
        }
    }
}

impl CompositeDiff {
    fn reversed(self) -> Self {
        Self {
            status: self.status.reversed(),
            changes: each(self.changes, |c| c.reversed()),
            attributes: each(self.attributes, AttributeDiff::reversed),
            lines: Vec::new(),
            sql: self.sql.reversed(),
            ..self
        }
    }
}

impl AttributeDiff {
    fn reversed(self) -> Self {
        Self {
            status: self.status.reversed(),
            position: self.position.reversed(),
            changes: each(self.changes, |c| c.reversed()),
            definition: self.definition.reversed(),
            ..self
        }
    }
}

impl ExtensionDiff {
    fn reversed(self) -> Self {
        Self {
            status: self.status.reversed(),
            changes: each(self.changes, |c| c.reversed()),
            lines: Vec::new(),
            sql: self.sql.reversed(),
            ..self
        }
    }
}

fn flip(map: BTreeMap<String, Status>) -> BTreeMap<String, Status> {
    map.into_iter().map(|(k, s)| (k, s.reversed())).collect()
}

/// [`flip`] for maps nested by schema.
fn flip_nested(
    map: BTreeMap<String, BTreeMap<String, Status>>,
) -> BTreeMap<String, BTreeMap<String, Status>> {
    map.into_iter().map(|(k, m)| (k, flip(m))).collect()
}

impl StatusMap {
    fn reversed(self) -> Self {
        Self {
            entities: self
                .entities
                .into_iter()
                .map(|(schema, names)| {
                    let names = names.into_iter().map(|(k, e)| (k, e.reversed()));
                    (schema, names.collect())
                })
                .collect(),
            orphan_partitions: flip_nested(self.orphan_partitions),
            enums: flip_nested(self.enums),
            domains: flip_nested(self.domains),
            composites: flip_nested(self.composites),
            extensions: flip(self.extensions),
        }
    }
}

impl EntityStatus {
    fn reversed(self) -> Self {
        Self {
            status: self.status.reversed(),
            columns: flip(self.columns),
            constraints: flip(self.constraints),
            indexes: flip(self.indexes),
            policies: flip(self.policies),
            partitions: flip_nested(self.partitions),
        }
    }
}
