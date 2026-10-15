//! A version's presentation: where tables sit and how they are grouped. One
//! shape everywhere (versions, drafts publishing, the web viewer):
//!
//! ```json
//! { "layout": { "public.users": { "x": 120, "y": 40 } },
//!   "groups": { "g1": { "name": "Billing", "color": "amber", "members": ["public.invoices"] } } }
//! ```
//!
//! Keys and members are qualified names (`<schema>.<table>`, remus's
//! `qualified_name`). A version without one stores the empty layout
//! (`{"layout": {}, "groups": {}}`, never null); reading is lenient (`{}`, a
//! missing field, read as empty). Stored layouts are [`pruned`](VersionLayout::pruned)
//! to the version's model.

use std::collections::{BTreeMap, HashSet};

use remus_core::Schema;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Where tables sit, and their groups.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct VersionLayout {
    /// Top-left corner of each table in graph coordinates, by qualified name.
    #[serde(default)]
    #[schema(required = true)]
    pub layout: BTreeMap<String, TablePosition>,
    /// By group id.
    #[serde(default)]
    #[schema(required = true)]
    pub groups: BTreeMap<String, TableGroup>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct TablePosition {
    pub x: f64,
    pub y: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct TableGroup {
    pub name: String,
    /// A color name (`stone`, `red`, `amber`, `green`, `teal`, `blue`,
    /// `violet`, `pink`); the web app draws an unknown one as `stone`.
    pub color: String,
    /// Qualified names of the tables in it.
    #[serde(default)]
    #[schema(required = true)]
    pub members: Vec<String>,
}

impl VersionLayout {
    /// A stored layout, leniently: anything that is not one is empty.
    pub fn from_value(value: &serde_json::Value) -> Self {
        Self::deserialize(value).unwrap_or_default()
    }

    /// Only what `model` still has: positions of its tables, and group
    /// members among them (a group left with none is kept, empty).
    #[must_use]
    pub fn pruned(mut self, model: &Schema) -> Self {
        let tables: HashSet<String> = model
            .entities
            .iter()
            .map(remus_core::model::Entity::qualified_name)
            .collect();
        self.layout.retain(|name, _| tables.contains(name));
        for group in self.groups.values_mut() {
            group.members.retain(|name| tables.contains(name));
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn model(tables: &[&str]) -> Schema {
        let entities: Vec<_> = tables
            .iter()
            .map(|name| {
                json!({ "schema": "public", "name": name, "kind": "r", "comment": null,
                        "partition_key": null, "view_definition": null, "columns": [] })
            })
            .collect();
        Schema::from_json(
            &json!({ "remus_version": "0.2", "database": "app", "server_version": "17",
                     "entities": entities })
            .to_string(),
        )
        .unwrap()
    }

    #[test]
    fn anything_but_a_layout_reads_as_empty() {
        assert_eq!(
            VersionLayout::from_value(&json!({})),
            VersionLayout::default()
        );
        assert_eq!(
            VersionLayout::from_value(&json!(null)),
            VersionLayout::default()
        );
        let empty = serde_json::to_value(VersionLayout::default()).unwrap();
        assert_eq!(empty, json!({ "layout": {}, "groups": {} }));
    }

    #[test]
    fn pruning_keeps_only_the_models_tables() {
        let layout = VersionLayout::from_value(&json!({
            "layout": { "public.users": { "x": 1, "y": 2 }, "public.gone": { "x": 3, "y": 4 } },
            "groups": { "g": { "name": "Core", "color": "blue",
                               "members": ["public.users", "public.gone"] } }
        }));
        let pruned = layout.pruned(&model(&["users", "orders"]));
        assert_eq!(pruned.layout.keys().collect::<Vec<_>>(), ["public.users"]);
        assert_eq!(pruned.groups["g"].members, ["public.users"]);
    }
}
