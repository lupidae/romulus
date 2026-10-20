//! The SQL a playground runs to hold a schema (docs/playground.md,
//! "Lifecycle", step 2): the text a draft from that version starts with,
//! after it compiled, with what the compile added to make it run, minus
//! what the playground's init already did. A playground is a real database
//! people query, so unlike a compile nothing is rolled back and nothing is
//! read back: it is a plain script, run once by the schema's owner.
//!
//! - **stubs** for the functions it calls without defining them, with the
//!   signatures the compile settled on ([`Compilation::stubs`]), created
//!   right before the first statement that calls them (they return NULL);
//! - **missing schemas** created first ([`Compilation::created_schemas`]);
//! - **policy roles** become `PUBLIC`, as in the sandbox (roles are the
//!   playground's own: none but its three exist);
//! - **extensions** are left out: the playground's init installed the
//!   model's, as the superuser, before anyone could connect; nobody can
//!   create, alter or comment on one afterwards. So are the statements
//!   recreating an extension's own objects (PostGIS's `spatial_ref_sys`);
//! - **`CREATE SCHEMA`** becomes `CREATE SCHEMA IF NOT EXISTS` (without
//!   schema elements): the init created the schemas extensions live in.

use std::fmt::Write as _;

use pg_query::{NodeEnum, protobuf::ObjectType};

use crate::{
    Compilation,
    catalog::Catalog,
    names::quote_ident,
    parse::{Statement, parse},
    prepare::{policy_roles, provided_by_extensions},
};

/// The playground script for `sql`, which `compilation` (of the same text)
/// compiled.
pub fn script(sql: &str, compilation: &Compilation, catalog: &Catalog) -> String {
    let parsed = parse(sql);
    let provided = provided_by_extensions(&parsed.statements, catalog);
    let mut out = String::with_capacity(sql.len() + 256);
    for schema in &compilation.created_schemas {
        let _ = writeln!(out, "CREATE SCHEMA IF NOT EXISTS {};", quote_ident(schema));
    }
    for statement in &parsed.statements {
        let i = statement.index;
        for stub in compilation.stubs.iter().filter(|s| s.statement == i) {
            out.push_str(&stub.ddl());
            out.push_str(";\n");
        }
        if is_extensions(statement) || provided.iter().any(|(s, _, _)| *s == i) {
            continue;
        }
        let range = statement.span.range();
        let mut rewrites: Vec<(std::ops::Range<usize>, &str)> = Vec::new();
        if let Some(roles) = policy_roles(statement, &parsed).and_then(|r| r.rewrite) {
            rewrites.push((roles, "PUBLIC"));
        }
        if let Some(at) = schema_keyword_end(statement, &parsed) {
            rewrites.push((at..at, " IF NOT EXISTS"));
        }
        rewrites.sort_by_key(|(r, _)| r.start);
        let mut cursor = range.start;
        for (replaced, with) in rewrites {
            out.push_str(&sql[cursor..replaced.start]);
            out.push_str(with);
            cursor = replaced.end;
        }
        out.push_str(&sql[cursor..range.end]);
        out.push_str(";\n");
    }
    out
}

/// A migration's statement as a playground runs it (Test migration, Run
/// migration): its policy roles become `PUBLIC`, as [`script`] made them
/// (none but the playground's three roles exist there). Everything else
/// is left as written.
pub fn migration(sql: &str) -> String {
    let parsed = parse(sql);
    let mut rewrites: Vec<std::ops::Range<usize>> = parsed
        .statements
        .iter()
        .filter_map(|statement| policy_roles(statement, &parsed).and_then(|r| r.rewrite))
        .collect();
    if rewrites.is_empty() {
        return sql.to_owned();
    }
    rewrites.sort_by_key(|r| r.start);
    let mut out = String::with_capacity(sql.len());
    let mut cursor = 0;
    for range in rewrites {
        out.push_str(&sql[cursor..range.start]);
        out.push_str("PUBLIC");
        cursor = range.end;
    }
    out.push_str(&sql[cursor..]);
    out
}

/// Where `IF NOT EXISTS` goes in a `CREATE SCHEMA` without it (and without
/// schema elements, which Postgres refuses with it): after `SCHEMA`.
fn schema_keyword_end(statement: &Statement, parsed: &crate::parse::ParsedSql) -> Option<usize> {
    let NodeEnum::CreateSchemaStmt(s) = statement.node() else {
        return None;
    };
    if s.if_not_exists || !s.schema_elts.is_empty() {
        return None;
    }
    parsed
        .tokens_of(statement)
        .iter()
        .filter(|t| !t.is_comment())
        .nth(1)
        .map(|t| t.end)
}

/// A statement about an extension itself.
fn is_extensions(statement: &Statement) -> bool {
    match statement.node() {
        NodeEnum::CreateExtensionStmt(_)
        | NodeEnum::AlterExtensionStmt(_)
        | NodeEnum::AlterExtensionContentsStmt(_) => true,
        NodeEnum::CommentStmt(s) => s.objtype() == ObjectType::ObjectExtension,
        NodeEnum::DropStmt(s) => s.remove_type() == ObjectType::ObjectExtension,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn compilation() -> Compilation {
        Compilation::failed(String::new(), 0, Vec::new())
    }

    #[test]
    fn extensions_are_left_to_the_init_and_policies_go_to_public() {
        let sql = "CREATE EXTENSION IF NOT EXISTS vector WITH SCHEMA public;\n\
                   COMMENT ON EXTENSION vector IS 'vectors';\n\
                   CREATE TABLE docs (id int, owner text, embedding vector(3));\n\
                   CREATE POLICY mine ON docs FOR SELECT TO app_user, admin USING (owner = current_user);";
        let script = script(sql, &compilation(), &Catalog::default());
        assert_eq!(
            script,
            "CREATE TABLE docs (id int, owner text, embedding vector(3));\n\
             CREATE POLICY mine ON docs FOR SELECT TO PUBLIC USING (owner = current_user);\n"
        );
    }

    #[test]
    fn a_migration_s_policies_name_public_and_the_rest_is_kept() {
        assert_eq!(
            migration(
                "CREATE POLICY own ON docs FOR SELECT TO app_user, \"Admin\" USING (owner = current_user);"
            ),
            "CREATE POLICY own ON docs FOR SELECT TO PUBLIC USING (owner = current_user);"
        );
        assert_eq!(
            migration("ALTER POLICY own ON docs TO auditor;"),
            "ALTER POLICY own ON docs TO PUBLIC;"
        );
        let untouched = "ALTER TABLE docs ADD COLUMN body text; -- TO app_user";
        assert_eq!(migration(untouched), untouched);
    }

    #[test]
    fn schemas_may_exist_already() {
        let script = script(
            "CREATE SCHEMA extensions;\nCREATE SCHEMA IF NOT EXISTS app;\nCREATE SCHEMA s CREATE TABLE t (a int);",
            &compilation(),
            &Catalog::default(),
        );
        assert_eq!(
            script,
            "CREATE SCHEMA IF NOT EXISTS extensions;\nCREATE SCHEMA IF NOT EXISTS app;\nCREATE SCHEMA s CREATE TABLE t (a int);\n"
        );
    }

    #[test]
    fn created_schemas_come_first() {
        let mut compiled = compilation();
        compiled.created_schemas = vec!["auth".to_owned()];
        let script = script("CREATE TABLE t (id int)", &compiled, &Catalog::default());
        assert_eq!(
            script,
            "CREATE SCHEMA IF NOT EXISTS auth;\nCREATE TABLE t (id int);\n"
        );
    }
}
