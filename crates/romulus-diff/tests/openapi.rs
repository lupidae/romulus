//! The `utoipa` feature describes the JSON serde writes, so the TypeScript
//! types generated from the API's OpenAPI document match what the SPA gets.
#![cfg(feature = "utoipa")]

use std::collections::BTreeMap;

use romulus_diff::SchemaDiff;
use serde_json::Value;
use utoipa::ToSchema;

fn components() -> BTreeMap<String, Value> {
    let mut schemas = Vec::new();
    SchemaDiff::schemas(&mut schemas);
    schemas
        .into_iter()
        .map(|(name, schema)| (name, serde_json::to_value(schema).unwrap()))
        .collect()
}

/// Every object schema, including the variants of tagged enums.
fn objects(value: &Value, out: &mut Vec<Value>) {
    if value.get("properties").is_some() {
        out.push(value.clone());
    }
    for variant in value["oneOf"].as_array().into_iter().flatten() {
        objects(variant, out);
    }
}

#[test]
fn every_output_type_is_a_named_component() {
    assert_eq!(SchemaDiff::name(), "SchemaDiff");
    let names = components();
    for name in [
        "Summary",
        "StatusMap",
        "EntityStatus",
        "EntityDiff",
        "ColumnDiff",
        "ColumnChange",
        "ConstraintDiff",
        "PolicyChange",
        "EnumDiff",
        "LabelDiff",
        "Sides",
        "Positions",
        "Status",
    ] {
        assert!(names.contains_key(name), "{name} missing");
    }
}

#[test]
fn every_property_serde_writes_is_required() {
    let mut all = Vec::new();
    for schema in components().values() {
        objects(schema, &mut all);
    }
    assert!(all.len() > 30);
    for object in all {
        let properties: Vec<&String> = object["properties"].as_object().unwrap().keys().collect();
        let required: Vec<&str> = object["required"]
            .as_array()
            .map(|r| r.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        for property in properties {
            assert!(
                required.contains(&property.as_str()),
                "{property} not required in {object}"
            );
        }
    }
}

#[test]
fn changes_are_unions_tagged_by_field() {
    let components = components();
    let variants = components["ColumnChange"]["oneOf"].as_array().unwrap();
    let fields: Vec<&str> = variants
        .iter()
        .map(|v| v["properties"]["field"]["enum"][0].as_str().unwrap())
        .collect();
    assert_eq!(
        fields,
        [
            "type",
            "type_kind",
            "not_null",
            "default",
            "expression",
            "identity",
            "generated",
            "comment",
            "position"
        ]
    );
    let statuses = &components["Status"]["enum"];
    assert_eq!(
        statuses,
        &serde_json::json!(["added", "removed", "changed", "unchanged"])
    );
}
