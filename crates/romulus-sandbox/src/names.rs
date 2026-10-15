//! Reading names out of `pg_query` nodes, and the `search_path` they resolve
//! against.

use pg_query::{
    NodeEnum,
    protobuf::{AConst, Node, RangeVar, TypeName, VariableSetKind, VariableSetStmt, a_const::Val},
};
pub(crate) use remus_core::emit::ident::quote_ident;

/// A possibly schema-qualified name, as written.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct QualifiedName {
    pub schema: Option<String>,
    pub name: String,
}

impl QualifiedName {
    /// From a name list (`[name]`, `[schema, name]`, `[catalog, schema, name]`).
    pub fn from_list(nodes: &[Node]) -> Option<Self> {
        let parts = strings(nodes);
        let (name, rest) = parts.split_last()?;
        Some(Self {
            schema: rest.last().cloned(),
            name: name.clone(),
        })
    }

    pub fn from_range_var(rv: &RangeVar) -> Self {
        Self {
            schema: (!rv.schemaname.is_empty()).then(|| rv.schemaname.clone()),
            name: rv.relname.clone(),
        }
    }

    /// Schema and name, the schema resolved against `path` when not written.
    pub fn resolve(&self, path: &SearchPath) -> (String, String) {
        (
            self.schema
                .clone()
                .unwrap_or_else(|| path.creation_schema().to_owned()),
            self.name.clone(),
        )
    }

    /// As SQL, quoted where needed: `public.users`, `"Billing"."Invoice"`.
    pub fn to_sql(&self) -> String {
        match &self.schema {
            Some(schema) => format!("{}.{}", quote_ident(schema), quote_ident(&self.name)),
            None => quote_ident(&self.name).into_owned(),
        }
    }
}

/// The `sval` of every `String` node in a list, skipping anything else.
pub(crate) fn strings(nodes: &[Node]) -> Vec<String> {
    nodes.iter().filter_map(string).map(str::to_owned).collect()
}

pub(crate) fn string(node: &Node) -> Option<&str> {
    match node.node.as_ref()? {
        NodeEnum::String(s) => Some(&s.sval),
        _ => None,
    }
}

pub(crate) fn a_const_string(node: &Node) -> Option<&str> {
    match node.node.as_ref()? {
        NodeEnum::AConst(AConst {
            val: Some(Val::Sval(s)),
            ..
        }) => Some(&s.sval),
        _ => None,
    }
}

/// A `TypeName` as SQL that names the same type in a function signature:
/// `pg_catalog.int4`, `public.mood`, `text[]`. Type modifiers are dropped
/// (function arguments ignore them). `None` for `%TYPE` references and
/// `SETOF`, which a stub signature cannot use.
pub(crate) fn type_sql(type_name: &TypeName) -> Option<String> {
    if type_name.pct_type || type_name.setof {
        return None;
    }
    let names = strings(&type_name.names);
    if names.is_empty() {
        return None;
    }
    let mut sql = match names.as_slice() {
        // The grammar's own spellings (`serial`, `int`…) arrive as
        // pg_catalog names already, except the serial pseudo-types.
        [name] => match name.as_str() {
            "serial" | "serial4" => "pg_catalog.int4".to_owned(),
            "bigserial" | "serial8" => "pg_catalog.int8".to_owned(),
            "smallserial" | "serial2" => "pg_catalog.int2".to_owned(),
            _ => quote_ident(name).into_owned(),
        },
        _ => names
            .iter()
            .map(|n| quote_ident(n).into_owned())
            .collect::<Vec<_>>()
            .join("."),
    };
    for _ in &type_name.array_bounds {
        sql.push_str("[]");
    }
    Some(sql)
}

/// The `search_path` as the text sets it, statement by statement, so an
/// unqualified name resolves to the schema Postgres would create it in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SearchPath {
    schemas: Vec<String>,
}

impl Default for SearchPath {
    fn default() -> Self {
        Self {
            schemas: vec!["$user".to_owned(), "public".to_owned()],
        }
    }
}

impl SearchPath {
    /// Where an unqualified `CREATE` goes: the first schema on the path,
    /// `$user` aside (the sandbox role never has a schema of its name).
    pub fn creation_schema(&self) -> &str {
        self.schemas
            .iter()
            .map(String::as_str)
            .find(|s| *s != "$user" && !s.starts_with("pg_"))
            .unwrap_or("public")
    }

    /// Follow a `SET` / `RESET` statement. Other settings are ignored.
    pub fn apply(&mut self, stmt: &VariableSetStmt) {
        if stmt.kind() == VariableSetKind::VarResetAll {
            *self = Self::default();
            return;
        }
        if !stmt.name.eq_ignore_ascii_case("search_path") {
            return;
        }
        match stmt.kind() {
            VariableSetKind::VarSetValue => {
                self.schemas = stmt
                    .args
                    .iter()
                    .filter_map(a_const_string)
                    .map(|s| s.trim().to_owned())
                    .collect();
            }
            VariableSetKind::VarSetDefault | VariableSetKind::VarReset => {
                *self = Self::default();
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use pg_query::NodeEnum;

    use super::{SearchPath, type_sql};

    fn set(sql: &str) -> SearchPath {
        let parsed = pg_query::parse(sql).unwrap();
        let mut path = SearchPath::default();
        for raw in &parsed.protobuf.stmts {
            if let Some(NodeEnum::VariableSetStmt(stmt)) =
                raw.stmt.as_ref().and_then(|s| s.node.as_ref())
            {
                path.apply(stmt);
            }
        }
        path
    }

    #[test]
    fn creation_schema_follows_set_search_path() {
        assert_eq!(SearchPath::default().creation_schema(), "public");
        assert_eq!(
            set("SET search_path TO app, public").creation_schema(),
            "app"
        );
        assert_eq!(
            set("SET search_path = \"$user\", billing").creation_schema(),
            "billing"
        );
        assert_eq!(
            set("SET search_path TO app; RESET search_path").creation_schema(),
            "public"
        );
        assert_eq!(
            set("SET LOCAL search_path TO 'Mixed'").creation_schema(),
            "Mixed"
        );
        assert_eq!(set("SET statement_timeout = 0").creation_schema(), "public");
    }

    fn column_type(sql: &str) -> Option<String> {
        let parsed = pg_query::parse(sql).unwrap();
        let Some(NodeEnum::CreateStmt(create)) = parsed.protobuf.stmts[0]
            .stmt
            .as_ref()
            .and_then(|s| s.node.as_ref())
        else {
            panic!("not a CREATE TABLE")
        };
        let Some(NodeEnum::ColumnDef(column)) = create.table_elts[0].node.as_ref() else {
            panic!("not a column")
        };
        type_sql(column.type_name.as_ref().unwrap())
    }

    #[test]
    fn type_names_render_as_signature_types() {
        assert_eq!(
            column_type("CREATE TABLE t (c int)").unwrap(),
            "pg_catalog.int4"
        );
        assert_eq!(
            column_type("CREATE TABLE t (c varchar(10)[])").unwrap(),
            "pg_catalog.varchar[]"
        );
        assert_eq!(
            column_type("CREATE TABLE t (c bigserial)").unwrap(),
            "pg_catalog.int8"
        );
        assert_eq!(
            column_type("CREATE TABLE t (c app.\"Mood\")").unwrap(),
            "app.\"Mood\""
        );
        assert_eq!(column_type("CREATE TABLE t (c text)").unwrap(), "text");
        assert_eq!(
            column_type("CREATE TABLE t (c timestamp with time zone)").unwrap(),
            "pg_catalog.timestamptz"
        );
    }
}
