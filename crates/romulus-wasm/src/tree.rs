//! Reading `libpg_query`'s JSON parse tree: a node is `{"Kind": {fields}}`,
//! fields left out when they hold their default (0, "", false, empty list),
//! enums by name, strings as `{"String": {"sval": …}}`.

use serde_json::Value;

/// A node's kind and fields: `("CreateStmt", {…})`.
pub fn kind(node: &Value) -> Option<(&str, &Value)> {
    let object = node.as_object()?;
    if object.len() != 1 {
        return None;
    }
    let (kind, fields) = object.iter().next()?;
    Some((kind.as_str(), fields))
}

/// The fields of `node` when it is of kind `name`.
pub fn as_kind<'a>(node: &'a Value, name: &str) -> Option<&'a Value> {
    match kind(node) {
        Some((k, fields)) if k == name => Some(fields),
        _ => None,
    }
}

pub fn str<'a>(fields: &'a Value, name: &str) -> &'a str {
    fields.get(name).and_then(Value::as_str).unwrap_or("")
}

pub fn opt_str<'a>(fields: &'a Value, name: &str) -> Option<&'a str> {
    fields
        .get(name)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
}

pub fn int(fields: &Value, name: &str) -> i64 {
    fields.get(name).and_then(Value::as_i64).unwrap_or(0)
}

pub fn bool(fields: &Value, name: &str) -> bool {
    fields.get(name).and_then(Value::as_bool).unwrap_or(false)
}

pub fn list<'a>(fields: &'a Value, name: &str) -> &'a [Value] {
    fields
        .get(name)
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice)
}

pub fn field<'a>(fields: &'a Value, name: &str) -> Option<&'a Value> {
    fields.get(name).filter(|v| !v.is_null())
}

/// A byte offset into the SQL (`location`), when the node has one.
pub fn location(fields: &Value) -> Option<usize> {
    fields
        .get("location")
        .and_then(Value::as_i64)
        .and_then(|l| usize::try_from(l).ok())
}

/// The `sval` of a `String` node.
pub fn string(node: &Value) -> Option<&str> {
    as_kind(node, "String").map(|s| str(s, "sval"))
}

/// The `sval`s of the `String` nodes of a list, others skipped.
pub fn strings(nodes: &[Value]) -> Vec<String> {
    nodes.iter().filter_map(string).map(str::to_owned).collect()
}

/// The items of a `List` node.
pub fn list_items(node: &Value) -> &[Value] {
    as_kind(node, "List").map_or(&[], |l| list(l, "items"))
}

/// A possibly qualified name, as written: `(schema, name)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Name {
    pub schema: Option<String>,
    pub name: String,
}

impl Name {
    /// From a name list (`[name]`, `[schema, name]`, `[catalog, schema, name]`).
    pub fn from_list(nodes: &[Value]) -> Option<Self> {
        let mut parts = strings(nodes);
        let name = parts.pop()?;
        Some(Self {
            schema: parts.pop(),
            name,
        })
    }

    /// From a `RangeVar`'s fields.
    pub fn from_range_var(fields: &Value) -> Self {
        Self {
            schema: opt_str(fields, "schemaname").map(str::to_owned),
            name: str(fields, "relname").to_owned(),
        }
    }
}

/// The value of an `A_Const`: what was written, typed as the grammar typed it.
#[derive(Debug, Clone, PartialEq)]
pub enum Literal {
    Integer(i64),
    /// A number with a point or an exponent, or too large for an integer:
    /// kept as written.
    Float(String),
    String(String),
    Bool(bool),
    BitString(String),
    Null,
}

pub fn literal(fields: &Value) -> Literal {
    if bool(fields, "isnull") {
        return Literal::Null;
    }
    if let Some(v) = field(fields, "ival") {
        return Literal::Integer(int(v, "ival"));
    }
    if let Some(v) = field(fields, "fval") {
        return Literal::Float(str(v, "fval").to_owned());
    }
    if let Some(v) = field(fields, "boolval") {
        return Literal::Bool(bool(v, "boolval"));
    }
    if let Some(v) = field(fields, "bsval") {
        return Literal::BitString(str(v, "bsval").to_owned());
    }
    if let Some(v) = field(fields, "sval") {
        return Literal::String(str(v, "sval").to_owned());
    }
    // `{"ival": {}}` is 0, which an absent `ival` would read as too.
    Literal::Integer(0)
}

/// Visits every node under `value`, depth first, with its kind and fields.
pub fn visit<'a>(value: &'a Value, f: &mut impl FnMut(&'a str, &'a Value)) {
    match value {
        Value::Object(object) => {
            if object.len() == 1
                && let Some((k, fields)) = object.iter().next()
                && k.chars().next().is_some_and(char::is_uppercase)
            {
                f(k, fields);
            }
            for inner in object.values() {
                visit(inner, f);
            }
        }
        Value::Array(items) => {
            for inner in items {
                visit(inner, f);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{Literal, Name, kind, literal, strings};

    #[test]
    fn reads_nodes_names_and_literals() {
        let node = json!({"ColumnRef": {"fields": [{"String": {"sval": "a"}}], "location": 3}});
        let (k, fields) = kind(&node).unwrap();
        assert_eq!(k, "ColumnRef");
        assert_eq!(strings(super::list(fields, "fields")), ["a"]);
        let names = json!([{"String": {"sval": "s"}}, {"String": {"sval": "t"}}]);
        assert_eq!(
            Name::from_list(names.as_array().unwrap()),
            Some(Name {
                schema: Some("s".into()),
                name: "t".into()
            })
        );
        assert_eq!(literal(&json!({"ival": {}})), Literal::Integer(0));
        assert_eq!(
            literal(&json!({"sval": {"sval": "x"}})),
            Literal::String("x".into())
        );
        assert_eq!(literal(&json!({"isnull": true})), Literal::Null);
    }
}
