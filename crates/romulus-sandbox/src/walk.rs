//! A complete walk over a `pg_query` tree, through its serde form.
//!
//! The typed `pg_query` API has no visitor that reaches every node, and a
//! hand-written one over ~270 node types would silently miss some. The
//! security checks (denied functions, temporary objects) must not miss any,
//! so they walk the JSON a parse tree serialises to, where every node is
//! `{"node": {"<Kind>": {…}}}` and nothing is skipped by construction.

use serde_json::Value;

/// Calls `f(kind, fields)` for every node under `value`, parents first.
pub(crate) fn visit(value: &Value, f: &mut impl FnMut(&str, &Value)) {
    match value {
        Value::Object(map) => {
            if let Some(Value::Object(wrapped)) = map.get("node")
                && wrapped.len() == 1
                && let Some((kind, fields)) = wrapped.iter().next()
            {
                f(kind, fields);
            }
            for child in map.values() {
                visit(child, f);
            }
        }
        Value::Array(items) => {
            for item in items {
                visit(item, f);
            }
        }
        _ => {}
    }
}

/// Calls `f` for every JSON object under `value`, wrapped in a node or not
/// (a `RangeVar` is a plain field of `CREATE TABLE`, a node elsewhere).
pub(crate) fn visit_objects(value: &Value, f: &mut impl FnMut(&serde_json::Map<String, Value>)) {
    match value {
        Value::Object(map) => {
            f(map);
            for child in map.values() {
                visit_objects(child, f);
            }
        }
        Value::Array(items) => {
            for item in items {
                visit_objects(item, f);
            }
        }
        _ => {}
    }
}

/// The dotted name of a `FuncCall`'s `funcname` list.
pub(crate) fn func_name(fields: &Value) -> Vec<String> {
    fields["funcname"]
        .as_array()
        .map(|parts| {
            parts
                .iter()
                .filter_map(|p| p["node"]["String"]["sval"].as_str())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// A node's `location`, when it has a known one.
pub(crate) fn location(fields: &Value) -> Option<usize> {
    fields["location"]
        .as_i64()
        .and_then(|l| usize::try_from(l).ok())
}

#[cfg(test)]
mod tests {
    use super::{func_name, visit};

    #[test]
    fn every_function_call_is_reached_however_deep() {
        let parsed = pg_query::parse(
            "CREATE TABLE t (a int DEFAULT f(g(1)), CHECK (h() OR (SELECT i()) > 0)) \
             PARTITION BY RANGE (j(a))",
        )
        .unwrap();
        let value = serde_json::to_value(&parsed.protobuf).unwrap();
        let mut names = Vec::new();
        visit(&value, &mut |kind, fields| {
            if kind == "FuncCall" {
                names.push(func_name(fields).join("."));
            }
        });
        names.sort();
        assert_eq!(names, ["f", "g", "h", "i", "j"]);
    }
}
