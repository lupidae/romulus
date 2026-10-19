//! The names Postgres gives what a text leaves unnamed: `orders_pkey`,
//! `orders_user_id_fkey`, `users_lower_idx`, `t_x_check1`… (`makeObjectName`,
//! `ChooseRelationName`, `ChooseConstraintName`, `ChooseIndexColumnNames`;
//! romulus-sandbox's `key` module predicts the same names on the server).

use serde_json::Value;

use crate::{catalog::Catalog, tree};

/// `NAMEDATALEN - 1`.
pub const MAX_IDENTIFIER: usize = 63;

/// `s` cut to at most `n` bytes, by whole characters.
fn cut(s: &str, n: usize) -> &str {
    let mut n = n.min(s.len());
    while !s.is_char_boundary(n) {
        n = n.saturating_sub(1);
    }
    s.get(..n).unwrap_or_default()
}

/// `makeObjectName`: `name1[_name2][_label]`, the longer of the two names
/// cut (by whole characters) to fit 63 bytes.
pub fn make_object_name(name1: &str, name2: Option<&str>, label: &str) -> String {
    let overhead =
        usize::from(name2.is_some()) + if label.is_empty() { 0 } else { label.len() + 1 };
    let available = MAX_IDENTIFIER.saturating_sub(overhead);
    let (mut n1, mut n2) = (name1.len(), name2.map_or(0, str::len));
    while n1 + n2 > available {
        if n1 > n2 {
            n1 -= 1;
        } else {
            n2 -= 1;
        }
    }
    let mut name = cut(name1, n1).to_owned();
    if let Some(name2) = name2 {
        name.push('_');
        name.push_str(cut(name2, n2));
    }
    if !label.is_empty() {
        name.push('_');
        name.push_str(label);
    }
    name
}

/// `ChooseIndexNameAddition`: the names joined by `_`, stopping once past
/// the identifier limit.
pub fn name_addition(names: &[String]) -> String {
    let mut addition = String::new();
    for name in names {
        if !addition.is_empty() {
            addition.push('_');
        }
        addition.push_str(cut(name, MAX_IDENTIFIER));
        if addition.len() > MAX_IDENTIFIER {
            break;
        }
    }
    addition
}

/// `ChooseRelationName`: the first `name1_name2_label[N]` no relation of
/// the schema has (nor, for an index backing a constraint, a constraint).
pub fn choose_relation_name(
    catalog: &Catalog,
    schema: &str,
    name1: &str,
    name2: Option<&str>,
    label: &str,
    constraint: bool,
    taken: &[String],
) -> String {
    let mut pass = 0;
    loop {
        let label = if pass == 0 {
            label.to_owned()
        } else {
            format!("{label}{pass}")
        };
        let name = make_object_name(name1, name2, &label);
        let used = catalog.relation_name_taken(schema, &name)
            || (constraint && catalog.constraint_name_taken(schema, &name))
            || taken.contains(&name);
        if !used {
            return name;
        }
        pass += 1;
    }
}

/// `ChooseConstraintName`: like relations, against the schema's
/// constraint names (and those just chosen in the same statement).
pub fn choose_constraint_name(
    catalog: &Catalog,
    schema: &str,
    name1: &str,
    name2: Option<&str>,
    label: &str,
    taken: &[String],
) -> String {
    let mut pass = 0;
    loop {
        let label = if pass == 0 {
            label.to_owned()
        } else {
            format!("{label}{pass}")
        };
        let name = make_object_name(name1, name2, &label);
        if !catalog.constraint_name_taken(schema, &name) && !taken.contains(&name) {
            return name;
        }
        pass += 1;
    }
}

/// `ChooseIndexColumnNames`: each element's name (its column, or what an
/// expression is called, else `expr`), made unique with a number.
pub fn index_column_names(params: &[Value]) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for param in params {
        let Some(e) = tree::as_kind(param, "IndexElem") else {
            continue;
        };
        let original = tree::opt_str(e, "indexcolname")
            .or_else(|| tree::opt_str(e, "name"))
            .map(str::to_owned)
            .or_else(|| tree::field(e, "expr").and_then(figure_column_name))
            .unwrap_or_else(|| "expr".to_owned());
        let mut name = original.clone();
        let mut i = 1;
        while names.contains(&name) {
            let suffix = i.to_string();
            let keep = cut(&original, MAX_IDENTIFIER.saturating_sub(suffix.len()));
            name = format!("{keep}{suffix}");
            i += 1;
        }
        names.push(name);
    }
    names
}

/// `FigureColname` for an index expression.
pub fn figure_column_name(node: &Value) -> Option<String> {
    let (kind, f) = tree::kind(node)?;
    match kind {
        "FuncCall" => tree::strings(tree::list(f, "funcname")).pop(),
        "ColumnRef" => tree::strings(tree::list(f, "fields")).pop(),
        "TypeCast" => tree::field(f, "arg")
            .and_then(figure_column_name)
            .or_else(|| {
                tree::field(f, "typeName").and_then(|t| tree::strings(tree::list(t, "names")).pop())
            }),
        "CollateClause" => tree::field(f, "arg").and_then(figure_column_name),
        "CaseExpr" => Some("case".to_owned()),
        "A_ArrayExpr" => Some("array".to_owned()),
        "RowExpr" => Some("row".to_owned()),
        "CoalesceExpr" => Some("coalesce".to_owned()),
        "MinMaxExpr" => Some(
            if tree::str(f, "op") == "IS_GREATEST" {
                "greatest"
            } else {
                "least"
            }
            .to_owned(),
        ),
        "SQLValueFunction" => Some(
            match tree::str(f, "op") {
                "SVFOP_CURRENT_DATE" => "current_date",
                "SVFOP_CURRENT_TIME" | "SVFOP_CURRENT_TIME_N" => "current_time",
                "SVFOP_CURRENT_TIMESTAMP" | "SVFOP_CURRENT_TIMESTAMP_N" => "current_timestamp",
                "SVFOP_LOCALTIME" | "SVFOP_LOCALTIME_N" => "localtime",
                "SVFOP_LOCALTIMESTAMP" | "SVFOP_LOCALTIMESTAMP_N" => "localtimestamp",
                "SVFOP_CURRENT_USER" => "current_user",
                _ => return None,
            }
            .to_owned(),
        ),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{make_object_name, name_addition};

    #[test]
    fn names_are_cut_to_fit_like_postgres() {
        assert_eq!(
            make_object_name("orders", Some("user_id"), "fkey"),
            "orders_user_id_fkey"
        );
        assert_eq!(make_object_name("users", None, "pkey"), "users_pkey");
        let long = "a".repeat(60);
        let name = make_object_name(&long, Some("column_name"), "fkey");
        assert_eq!(name.len(), 63);
        assert!(name.ends_with("_column_name_fkey"));
        assert_eq!(
            name_addition(&["org_id".into(), "email".into()]),
            "org_id_email"
        );
    }
}
