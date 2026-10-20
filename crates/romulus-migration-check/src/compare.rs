//! Is the database the model? It is introspected (remus's own query, as a
//! push reads a real database) and compared with `romulus_diff::diff`,
//! after what may rightly differ is set aside:
//!
//! - objects an extension owns (`EXTENSION_OBJECTS`: PostGIS's
//!   `spatial_ref_sys`…), and extensions' versions;
//! - column order where the migration warned that Postgres adds a column
//!   last ([`SchemaOptions::reordered`]);
//! - policy roles where the database runs every policy for `PUBLIC`
//!   ([`SchemaOptions::policies_public`]: a playground, the sandbox).
//!
//! This is the comparison the migration oracle has always made; it lives
//! here so the oracle and a playground's test cannot disagree on it.

use std::time::Duration;

use remus_core::{INTROSPECT_SQL, Schema};

use crate::session::{Rows, Session, SessionError};

/// What may differ between a database and its model.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SchemaOptions {
    /// Tables and composite types (`schema.name`) whose columns the
    /// migration warned Postgres puts last (`WarningCode::ColumnOrder`):
    /// compared in the model's order.
    pub reordered: Vec<String>,
    /// The database's policies all name `PUBLIC` (roles are not created
    /// there): roles are not compared.
    pub policies_public: bool,
}

/// The database's schema, as remus reads it.
pub async fn introspect<S: Session>(
    session: &mut S,
    limit: Duration,
) -> Result<Schema, SessionError> {
    let answer = session.run(INTROSPECT_SQL, limit, Rows::READ).await?;
    let json = answer
        .rows
        .first()
        .and_then(|r| r.first().cloned().flatten())
        .ok_or_else(|| SessionError::Lost("the introspection returned nothing".to_owned()))?;
    Schema::from_json(&json)
        .map(Schema::canonical)
        .map_err(|e| SessionError::Lost(format!("the introspected schema does not read: {e}")))
}

fn prepared(schema: &Schema, options: &SchemaOptions) -> Schema {
    let mut schema = schema.clone();
    romulus_migration::without_extension_objects(&mut schema);
    for extension in &mut schema.extensions {
        extension.version.clear();
    }
    if options.policies_public {
        for entity in &mut schema.entities {
            for policy in &mut entity.policies {
                policy.roles = vec!["public".to_owned()];
            }
        }
    }
    schema
}

/// How `actual` differs from `expected`, one line per difference (empty:
/// the same schema).
pub fn differences(actual: &Schema, expected: &Schema, options: &SchemaOptions) -> Vec<String> {
    let mut actual = prepared(actual, options);
    let expected = prepared(expected, options);
    for object in &options.reordered {
        if let Some(wanted) = expected
            .entities
            .iter()
            .find(|e| format!("{}.{}", e.schema, e.name) == *object)
            && let Some(entity) = actual
                .entities
                .iter_mut()
                .find(|e| e.schema == wanted.schema && e.name == wanted.name)
        {
            entity.columns.sort_by_key(|c| {
                wanted
                    .columns
                    .iter()
                    .position(|w| w.name == c.name)
                    .unwrap_or(usize::MAX)
            });
        }
        if let Some(wanted) = expected
            .composites
            .iter()
            .find(|c| format!("{}.{}", c.schema, c.name) == *object)
            && let Some(composite) = actual
                .composites
                .iter_mut()
                .find(|c| c.schema == wanted.schema && c.name == wanted.name)
        {
            composite.attributes.sort_by_key(|a| {
                wanted
                    .attributes
                    .iter()
                    .position(|w| w.name == a.name)
                    .unwrap_or(usize::MAX)
            });
        }
    }
    romulus_diff::diff(&actual, &expected)
        .lines()
        .map(str::to_owned)
        .collect()
}

/// The objects whose column order a migration warned about.
pub fn reordered(migration: &romulus_migration::Migration) -> Vec<String> {
    migration
        .warnings
        .iter()
        .filter(|w| w.code == romulus_migration::WarningCode::ColumnOrder)
        .filter_map(|w| w.object.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(columns: &[&str], roles: &[&str]) -> Schema {
        let columns: Vec<serde_json::Value> = columns
            .iter()
            .map(|name| {
                serde_json::json!({
                    "name": name, "type": "text", "type_schema": "pg_catalog", "type_name": "text",
                    "type_kind": "b", "not_null": false, "default": null, "identity": null,
                    "generated": null, "comment": null
                })
            })
            .collect();
        Schema::from_json(
            &serde_json::json!({
                "remus_version": "0.2", "database": "t", "server_version": "17",
                "entities": [{
                    "schema": "public", "name": "t", "kind": "r", "comment": null,
                    "partition_key": null, "view_definition": null, "rls": true,
                    "columns": columns,
                    "policies": [{"name": "p", "command": "*", "permissive": true,
                                  "roles": roles, "using": "true", "check": null}]
                }]
            })
            .to_string(),
        )
        .unwrap()
    }

    #[test]
    fn column_order_is_forgiven_only_where_warned() {
        let actual = model(&["a", "c", "b"], &["public"]);
        let expected = model(&["a", "b", "c"], &["public"]);
        assert_ne!(
            differences(&actual, &expected, &SchemaOptions::default()),
            Vec::<String>::new()
        );
        let warned = SchemaOptions {
            reordered: vec!["public.t".to_owned()],
            ..SchemaOptions::default()
        };
        assert_eq!(
            differences(&actual, &expected, &warned),
            Vec::<String>::new()
        );
    }

    #[test]
    fn policy_roles_are_forgiven_where_every_policy_names_public() {
        let actual = model(&["a"], &["public"]);
        let expected = model(&["a"], &["app_user"]);
        assert_ne!(
            differences(&actual, &expected, &SchemaOptions::default()),
            Vec::<String>::new()
        );
        let public = SchemaOptions {
            policies_public: true,
            ..SchemaOptions::default()
        };
        assert_eq!(
            differences(&actual, &expected, &public),
            Vec::<String>::new()
        );
    }
}
