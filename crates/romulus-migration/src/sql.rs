//! SQL text helpers: definitions as `remus-sql` writes them, and the small
//! amount of looking inside expressions the planner needs.

use std::fmt::Write;

use remus_core::{
    emit::ident::{qualified, quote_ident},
    model::{Column, Entity, EntityKind, Generated, Identity, Policy, PolicyCommand},
};

/// `sql` with every whitespace run outside quotes reduced to one space and
/// none at either end: how romulus-diff compares deparsed SQL, which
/// Postgres majors lay out differently.
pub(crate) fn squash(sql: &str) -> String {
    let mut out = String::with_capacity(sql.len());
    let mut quote = None;
    let mut space = false;
    for c in sql.chars() {
        match quote {
            Some(q) => {
                out.push(c);
                if c == q {
                    quote = None;
                }
            }
            None if c.is_whitespace() => space = true,
            None => {
                if space && !out.is_empty() {
                    out.push(' ');
                }
                space = false;
                if c == '\'' || c == '"' {
                    quote = Some(c);
                }
                out.push(c);
            }
        }
    }
    out
}

/// Same text, whitespace outside quotes aside.
pub(crate) fn same_sql(a: &str, b: &str) -> bool {
    a == b || squash(a) == squash(b)
}

pub(crate) fn table_name(entity: &Entity) -> String {
    qualified(&entity.schema, &entity.name)
}

/// A column as `remus-sql` writes it inside `CREATE TABLE`, name first.
pub(crate) fn column_definition(column: &Column) -> String {
    let mut line = format!("{} {}", quote_ident(&column.name), column.r#type);
    match (&column.generated, &column.identity, &column.default) {
        (Some(kind), _, Some(expression)) => {
            let _ = write!(
                line,
                " GENERATED ALWAYS AS ({expression}) {}",
                storage(kind)
            );
        }
        (None, Some(identity), _) => {
            let _ = write!(line, " GENERATED {} AS IDENTITY", identity_when(identity));
        }
        (None, None, Some(default)) => {
            let _ = write!(line, " DEFAULT {default}");
        }
        (Some(_), _, None) | (None, None, None) => {}
    }
    if column.not_null {
        line.push_str(" NOT NULL");
    }
    line
}

pub(crate) fn storage(kind: &Generated) -> &'static str {
    match kind {
        Generated::Stored => "STORED",
        Generated::Virtual => "VIRTUAL",
    }
}

pub(crate) fn identity_when(identity: &Identity) -> &'static str {
    match identity {
        Identity::Always => "ALWAYS",
        Identity::ByDefault => "BY DEFAULT",
    }
}

/// The sequence a serial column's default draws from:
/// `nextval('shop.orders_id_seq'::regclass)` → `shop.orders_id_seq`, as
/// `remus-sql` reads it (the literal is already quoted as needed).
pub(crate) fn owned_sequence(default: &str) -> Option<&str> {
    let inner = default.strip_prefix("nextval('")?;
    let (name, rest) = inner.split_once('\'')?;
    (rest == "::regclass)").then_some(name)
}

/// `CREATE POLICY …` for `policy` on `table`, its expressions in the
/// parentheses `CREATE POLICY` requires (Postgres prints `true`, not
/// `(true)`; remus#16).
pub(crate) fn create_policy(table: &str, policy: &Policy) -> String {
    let mut out = format!("CREATE POLICY {} ON {table}", quote_ident(&policy.name));
    if !policy.permissive {
        out.push_str(" AS RESTRICTIVE");
    }
    let command = match policy.command {
        PolicyCommand::All => "ALL",
        PolicyCommand::Select => "SELECT",
        PolicyCommand::Insert => "INSERT",
        PolicyCommand::Update => "UPDATE",
        PolicyCommand::Delete => "DELETE",
    };
    let _ = write!(out, " FOR {command}");
    out.push_str(&policy_roles(policy));
    if let Some(using) = &policy.using {
        let _ = write!(out, " USING {}", parenthesized(using));
    }
    if let Some(check) = &policy.check {
        let _ = write!(out, " WITH CHECK {}", parenthesized(check));
    }
    out.push(';');
    out
}

/// ` TO a, b`, or nothing for PUBLIC.
pub(crate) fn policy_roles(policy: &Policy) -> String {
    if policy.roles.is_empty() {
        return String::new();
    }
    let mut roles: Vec<&str> = policy.roles.iter().map(String::as_str).collect();
    roles.sort_unstable();
    let roles: Vec<_> = roles.into_iter().map(quote_ident).collect();
    format!(" TO {}", roles.join(", "))
}

pub(crate) fn parenthesized(expr: &str) -> String {
    if is_parenthesized(expr) {
        expr.to_owned()
    } else {
        format!("({expr})")
    }
}

/// `(a = b)` yes; `f(x)`, `true`, `(a) AND (b)` no. Quotes are respected.
fn is_parenthesized(expr: &str) -> bool {
    let expr = expr.trim();
    if !expr.starts_with('(') || !expr.ends_with(')') {
        return false;
    }
    let mut depth = 0_i32;
    let mut quote = None;
    let last = expr.len() - 1;
    for (i, c) in expr.char_indices() {
        if let Some(q) = quote {
            if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '\'' | '"' => quote = Some(c),
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 && i != last {
                    return false;
                }
            }
            _ => {}
        }
    }
    depth == 0
}

/// `CREATE [MATERIALIZED] VIEW …` as `remus-sql` writes it.
pub(crate) fn create_view(view: &Entity) -> String {
    let body = view
        .view_definition
        .as_deref()
        .unwrap_or("")
        .trim()
        .trim_end_matches(';')
        .trim_end();
    format!(
        "CREATE {} {} AS\n{body};",
        relation_keyword(&view.kind),
        table_name(view)
    )
}

/// The word `DROP`/`COMMENT ON` use for an entity.
pub(crate) fn relation_keyword(kind: &EntityKind) -> &'static str {
    match kind {
        EntityKind::Table | EntityKind::PartitionedTable => "TABLE",
        EntityKind::View => "VIEW",
        EntityKind::MaterializedView => "MATERIALIZED VIEW",
        EntityKind::ForeignTable => "FOREIGN TABLE",
    }
}

/// Whether `sql` names `ident` as an identifier outside string literals:
/// `mood` in `'a'::mood` or `public.mood[]`, not in `moods` or `'mood'`.
/// Quoted identifiers count when they match exactly. Good enough to find
/// what refers to a type or a column; a false positive only recreates
/// something that did not need it.
pub(crate) fn mentions(sql: &str, ident: &str) -> bool {
    let mut chars = sql.char_indices().peekable();
    while let Some((start, c)) = chars.next() {
        if c == '\'' {
            // A doubled quote inside closes and reopens: the same thing.
            for (_, d) in chars.by_ref() {
                if d == '\'' {
                    break;
                }
            }
        } else if c == '"' {
            let mut name = String::new();
            while let Some((_, d)) = chars.next() {
                if d == '"' {
                    if chars.next_if(|(_, e)| *e == '"').is_some() {
                        name.push('"');
                        continue;
                    }
                    break;
                }
                name.push(d);
            }
            if name == ident {
                return true;
            }
        } else if c.is_alphabetic() || c == '_' {
            let mut end = start + c.len_utf8();
            while let Some((i, d)) =
                chars.next_if(|(_, d)| d.is_alphanumeric() || *d == '_' || *d == '$')
            {
                end = i + d.len_utf8();
            }
            if sql[start..end].to_lowercase() == ident {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::{is_parenthesized, mentions, owned_sequence, squash};

    #[test]
    fn mentions_finds_identifiers_not_literals_or_longer_words() {
        assert!(mentions("'a'::mood", "mood"));
        assert!(mentions(
            "(state)::text = ANY (ARRAY['a'::public.mood])",
            "mood"
        ));
        assert!(mentions("x::\"Mood\"", "Mood"));
        assert!(!mentions("'mood'", "mood"));
        assert!(!mentions("moods > 1", "mood"));
        assert!(!mentions("x::\"Mood\"", "mood"));
        assert!(mentions("lower(email)", "email"));
    }

    #[test]
    fn whitespace_outside_quotes_is_squashed() {
        assert_eq!(squash("CHECK ((a >\n  0))"), "CHECK ((a > 0))");
        assert_eq!(squash(" 'a  b' "), "'a  b'");
    }

    #[test]
    fn serial_defaults_name_their_sequence() {
        assert_eq!(
            owned_sequence("nextval('shop.orders_id_seq'::regclass)"),
            Some("shop.orders_id_seq")
        );
        assert_eq!(owned_sequence("now()"), None);
    }

    #[test]
    fn only_wrapped_expressions_are_parenthesized() {
        assert!(is_parenthesized("(a = b)"));
        assert!(!is_parenthesized("(a) AND (b)"));
        assert!(!is_parenthesized("true"));
    }
}
