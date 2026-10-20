//! A model as SQL, in an order that compiles back.

use remus_core::{Schema, model::EntityKind};

use crate::{
    key::{Action, ObjectKind},
    names::QualifiedName,
    parse::parse,
};

/// Where a statement goes: after or before the statement at this index.
enum Move {
    After(usize),
    Before(usize),
}

/// Render a model as SQL: what a draft starts from when the latest version
/// was pushed as remus JSON.
///
/// This is `remus_sql::render` with the fixes it needs to compile back to
/// the same model:
///
/// - policy expressions are parenthesized (`USING (true)`): remus-sql writes
///   them as Postgres prints them, and Postgres drops the parentheses around
///   a bare call, column or constant, which `CREATE POLICY` requires;
/// - an index on a materialized view goes right after the view (remus-sql
///   writes every index before every view);
/// - a unique index goes before the foreign keys (remus-sql writes them
///   after, and a foreign key may reference a unique index).
///
/// All three are remus#16 (fixed upstream for the next remus release):
/// remove these workarounds, and their tests, once the workspace depends on
/// it. And:
///
/// - the copies Postgres makes of a foreign key to a partitioned table, one
///   per partition (`…_fkey1` referencing the partition), are left out: the
///   key makes them again, and creating one fails ("already exists").
///
/// Everything else is remus-sql's output, byte for byte. Known gaps of
/// remus 0.2 this does not paper over: `NOT VALID` CHECKs are written
/// inline (so they come back validated), and stand-alone range types are
/// not modelled (a column of one cannot be rendered).
pub fn render_sql(schema: &Schema) -> String {
    let fixed = fixed(schema);
    let schema = fixed.as_ref().unwrap_or(schema);
    let sql = remus_sql::render(schema);
    let parsed = parse(&sql);
    if !parsed.errors.is_empty() {
        return sql;
    }
    let is_view = |schema_name: &str, name: &str| {
        schema.entities.iter().any(|e| {
            e.kind == EntityKind::MaterializedView && e.schema == schema_name && e.name == name
        })
    };

    let first_fk = parsed
        .statements
        .iter()
        .position(|s| s.key.action() == Action::Create && s.key.kind() == ObjectKind::ForeignKey);
    let mut moves: Vec<(usize, Move)> = Vec::new();
    for statement in &parsed.statements {
        let pg_query::NodeEnum::IndexStmt(index) = &statement.node else {
            continue;
        };
        let Some(relation) = &index.relation else {
            continue;
        };
        let (rel_schema, rel_name) =
            QualifiedName::from_range_var(relation).resolve(&statement.search_path);
        if is_view(&rel_schema, &rel_name) {
            let view = parsed.statements.iter().find(|s| {
                s.key.kind() == ObjectKind::MaterializedView
                    && s.key.action() == Action::Create
                    && s.key.path() == [rel_schema.clone(), rel_name.clone()]
            });
            if let Some(view) = view
                && view.index > statement.index
            {
                moves.push((statement.index, Move::After(view.index)));
            }
        } else if index.unique
            && let Some(fk) = first_fk
            && fk < statement.index
        {
            moves.push((statement.index, Move::Before(fk)));
        }
    }
    if moves.is_empty() {
        return sql;
    }

    // remus#16: index order. Rebuild the text: each statement's extent, in the new order, with the
    // original gaps (blank lines, comments) kept where they were.
    let moved_statements: Vec<usize> = moves.iter().map(|(i, _)| *i).collect();
    let text = |i: usize| {
        let s = &parsed.statements[i];
        sql[s.extent.range()].to_owned()
    };
    let mut out = String::with_capacity(sql.len());
    let mut cursor = 0;
    for statement in &parsed.statements {
        let range = statement.extent.range();
        out.push_str(&sql[cursor..range.start]);
        cursor = range.end;
        for (i, _) in moves
            .iter()
            .filter(|(_, m)| matches!(m, Move::Before(t) if *t == statement.index))
        {
            out.push_str(&text(*i));
            out.push('\n');
        }
        if !moved_statements.contains(&statement.index) {
            out.push_str(&sql[range]);
        } else if sql[cursor..].starts_with('\n') {
            // Drop the moved statement's line break too.
            cursor += 1;
        }
        for (i, _) in moves
            .iter()
            .filter(|(_, m)| matches!(m, Move::After(t) if *t == statement.index))
        {
            out.push('\n');
            out.push_str(&text(*i));
        }
    }
    out.push_str(&sql[cursor..]);
    out
}

/// The model with its policy expressions parenthesized, if any needs it.
/// Workaround for remus#16.
fn fixed(schema: &Schema) -> Option<Schema> {
    let needs_parens = schema
        .entities
        .iter()
        .flat_map(|e| &e.policies)
        .flat_map(|p| [&p.using, &p.check])
        .flatten()
        .any(|expr| !is_parenthesized(expr));
    let mut without_clones = schema.clone();
    romulus_migration::without_fk_clones(&mut without_clones);
    let clones = without_clones != *schema;
    if !needs_parens && !clones {
        return None;
    }
    let mut schema = without_clones;
    for policy in schema.entities.iter_mut().flat_map(|e| &mut e.policies) {
        for expr in [&mut policy.using, &mut policy.check].into_iter().flatten() {
            if !is_parenthesized(expr) {
                *expr = format!("({expr})");
            }
        }
    }
    Some(schema)
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

#[cfg(test)]
mod tests {
    use remus_core::Schema;

    use super::{is_parenthesized, render_sql};

    fn schema(json: &str) -> Schema {
        Schema::from_json(json).unwrap()
    }

    const BASE: &str = r#"{
        "remus_version": "0.1", "database": "db", "server_version": "17",
        "entities": [
            {"schema": "public", "name": "accounts", "kind": "r", "comment": null,
             "partition_key": null, "view_definition": null,
             "columns": [{"name": "code", "type": "text", "type_schema": "pg_catalog",
                          "type_name": "text", "type_kind": "b", "not_null": true,
                          "default": null, "identity": null, "generated": null, "comment": null}],
             "indexes": [{"name": "accounts_code_idx", "unique": true, "partial": false,
                          "definition": "CREATE UNIQUE INDEX accounts_code_idx ON public.accounts USING btree (code)"}]},
            {"schema": "public", "name": "totals", "kind": "m", "comment": null,
             "partition_key": null, "view_definition": " SELECT 1 AS one;",
             "indexes": [{"name": "totals_one_idx", "unique": true, "partial": false,
                          "definition": "CREATE UNIQUE INDEX totals_one_idx ON public.totals USING btree (one)"}]},
            {"schema": "public", "name": "payments", "kind": "r", "comment": null,
             "partition_key": null, "view_definition": null,
             "columns": [{"name": "account", "type": "text", "type_schema": "pg_catalog",
                          "type_name": "text", "type_kind": "b", "not_null": false,
                          "default": null, "identity": null, "generated": null, "comment": null}],
             "constraints": [{"name": "payments_account_fkey", "kind": "f", "columns": ["account"],
                              "ref_schema": "public", "ref_table": "accounts", "ref_columns": ["code"],
                              "on_update": "a", "on_delete": "a", "deferrable": false,
                              "definition": "FOREIGN KEY (account) REFERENCES public.accounts(code)",
                              "comment": null}]}
        ]
    }"#;

    #[test]
    fn indexes_follow_their_materialized_view_and_unique_ones_precede_foreign_keys() {
        let sql = render_sql(&schema(BASE));
        let at = |needle: &str| {
            sql.find(needle)
                .unwrap_or_else(|| panic!("{needle} in\n{sql}"))
        };
        assert!(
            at("CREATE UNIQUE INDEX accounts_code_idx")
                < at("ADD CONSTRAINT payments_account_fkey")
        );
        assert!(
            at("CREATE MATERIALIZED VIEW public.totals") < at("CREATE UNIQUE INDEX totals_one_idx")
        );
        // Nothing lost, nothing doubled.
        assert_eq!(sql.matches("CREATE UNIQUE INDEX").count(), 2);
        assert_eq!(
            crate::parse(&sql).statements.len(),
            crate::parse(&remus_sql::render(&schema(BASE)))
                .statements
                .len()
        );
    }

    #[test]
    fn a_schema_needing_no_moves_renders_exactly_as_remus_sql() {
        let plain = BASE.replace("\"unique\": true, \"partial\": false,\n                          \"definition\": \"CREATE UNIQUE INDEX accounts", "\"unique\": false, \"partial\": false,\n                          \"definition\": \"CREATE INDEX accounts");
        let mut model = schema(&plain);
        model.entities.retain(|e| e.name != "totals");
        assert_eq!(render_sql(&model), remus_sql::render(&model));
    }

    #[test]
    fn only_fully_wrapped_expressions_count_as_parenthesized() {
        assert!(is_parenthesized("(a = b)"));
        assert!(is_parenthesized("((a = 1) AND (b = ')'))"));
        assert!(!is_parenthesized("true"));
        assert!(!is_parenthesized("app.is_admin(id)"));
        assert!(!is_parenthesized("(a = 1) AND (b = 2)"));
        assert!(!is_parenthesized("(a) = (')')"));
    }

    #[test]
    fn policy_expressions_get_parentheses() {
        let model = schema(
            r#"{
            "remus_version": "0.1", "database": "db", "server_version": "17",
            "entities": [{"schema": "public", "name": "t", "kind": "r", "comment": null,
                          "partition_key": null, "view_definition": null, "rls": true,
                          "policies": [{"name": "p", "command": "*", "permissive": true,
                                        "roles": ["app"], "using": "true",
                                        "check": "app.ok(id)"},
                                       {"name": "q", "command": "r", "permissive": true,
                                        "roles": [], "using": "(id = 1)", "check": null}]}]
        }"#,
        );
        let sql = render_sql(&model);
        assert!(
            sql.contains(
                "CREATE POLICY p ON public.t FOR ALL TO app USING (true) WITH CHECK (app.ok(id));"
            ),
            "{sql}"
        );
        assert!(
            sql.contains("CREATE POLICY q ON public.t FOR SELECT USING (id = 1);"),
            "{sql}"
        );
    }
}
