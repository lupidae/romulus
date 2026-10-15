//! [`SchemaDiff::reversed`]: the same diff read from the other side.

use std::collections::BTreeMap;

use crate::model::{
    AttributeDiff, ColumnDiff, CompositeDiff, ConstraintChange, ConstraintDiff, DomainDiff,
    EntityChange, EntityDiff, EntityStatus, EnumDiff, ExtensionDiff, IndexDiff, LabelDiff,
    PartitionDiff, PolicyDiff, SchemaDiff, Status, StatusMap, Summary,
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
            orphan_partitions: self
                .orphan_partitions
                .into_iter()
                .map(PartitionDiff::reversed)
                .collect(),
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
        Self {
            status: self.status.reversed(),
            changes: each(self.changes, |c| c.reversed()),
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

impl StatusMap {
    fn reversed(self) -> Self {
        Self {
            entities: self
                .entities
                .into_iter()
                .map(|(k, e)| (k, e.reversed()))
                .collect(),
            enums: flip(self.enums),
            domains: flip(self.domains),
            composites: flip(self.composites),
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
        }
    }
}
