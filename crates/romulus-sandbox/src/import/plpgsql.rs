//! DDL wrapped in a `DO` block, unwrapped for the replay.
//!
//! Tools and people wrap schema changes in `DO` blocks to make them
//! idempotent: Active Record 7.1+ writes every `create_enum` as
//!
//! ```sql
//! DO $$ BEGIN IF NOT EXISTS (SELECT 1 FROM pg_type t … WHERE t.typname = 'mood' …) THEN
//!   CREATE TYPE "mood" AS ENUM ('happy', 'sad');
//! END IF; END $$;
//! ```
//!
//! and Drizzle (like many hand-written migrations) adds foreign keys as
//! `DO $$ BEGIN ALTER TABLE … ADD CONSTRAINT …; EXCEPTION WHEN
//! duplicate_object THEN null; END $$;`. Skipping those would lose the
//! schema they create, so a block made only of such DDL is unwrapped: its
//! statements are replayed as if written at the top level, each checked by
//! the allowlist like any other, and none of the block's own code runs (the
//! condition is not evaluated). When the block was guarded (an `IF [NOT]
//! EXISTS` on the catalogs, or an `EXCEPTION WHEN duplicate_…/undefined_…`
//! handler), the replay runs each statement in a fixed wrapper of its own
//! that ignores "already exists" and "does not exist" errors, which is what
//! the guard meant. Any other block (loops, variables, data, conditions on
//! anything but the catalogs) is skipped and reported.

use std::ops::Range;

use pg_query::NodeEnum;
use serde_json::Value;

use super::statements::skip_reason;

/// The statements of a `DO` block that is only DDL, located in the text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Unwrapped {
    /// Each statement's range in the text, its `;` included when the body
    /// has one right after it.
    pub statements: Vec<Range<usize>>,
    /// The block ran them only if they did not exist yet (or still did).
    pub guarded: bool,
}

/// Conditions an exception handler may catch for the block to count as a
/// guard: the object was already there, or was not.
fn guard_condition(name: &str) -> bool {
    name == "others" || name.starts_with("duplicate_") || name.starts_with("undefined_")
}

/// The block's DDL, if it is only DDL. `statement` is the `DO` statement's
/// text, `at` its offset in the whole text.
pub(crate) fn unwrap(node: &NodeEnum, statement: &str, at: usize) -> Option<Unwrapped> {
    let NodeEnum::DoStmt(s) = node else {
        return None;
    };
    let mut body = None;
    for arg in &s.args {
        let Some(NodeEnum::DefElem(d)) = arg.node.as_ref() else {
            return None;
        };
        let value = d.arg.as_deref().and_then(crate::names::string)?;
        match d.defname.as_str() {
            "as" => body = Some(value),
            "language" if value.eq_ignore_ascii_case("plpgsql") => {}
            _ => return None,
        }
    }
    let body = body?;
    // Dollar-quoted only: the body's text is then the statement's, byte
    // for byte, and its statements can be located in it.
    let body_at = statement.find(body).filter(|_| !body.is_empty())?;
    let quote = statement[..body_at].rfind('$')?;
    if !statement[..=quote].trim_end().ends_with('$') {
        return None;
    }

    // The body is parsed as a function's: the only way libpg_query parses
    // PL/pgSQL. The tag cannot occur in a dollar-quoted body that ended at
    // its own closing tag… unless the body used this very tag, which the
    // parse would then reject.
    let tag = "$romulus_do$";
    if body.contains(tag) {
        return None;
    }
    let wrapped =
        format!("CREATE FUNCTION romulus_do() RETURNS void LANGUAGE plpgsql AS {tag}{body}{tag}");
    let parsed = pg_query::parse_plpgsql(&wrapped).ok()?;
    let function = parsed.get(0)?.get("PLpgSQL_function")?;
    // Variables of its own (beyond FOUND, SQLSTATE, SQLERRM) mean code.
    let datums = function["datums"].as_array().map_or(0, Vec::len);
    let builtin = function["datums"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|d| {
            matches!(
                d["PLpgSQL_var"]["refname"].as_str(),
                Some("found" | "sqlstate" | "sqlerrm")
            )
        })
        .count();
    if datums != builtin {
        return None;
    }
    let mut queries = Vec::new();
    let mut guarded = false;
    collect(&function["action"], &mut queries, &mut guarded)?;
    if queries.is_empty() {
        return None;
    }

    // Locate each statement in the body, in order.
    let mut statements = Vec::with_capacity(queries.len());
    let mut from = 0;
    for query in &queries {
        let found = body[from..].find(query.as_str())? + from;
        let mut end = found + query.len();
        let rest = &body[end..];
        if let Some(k) = rest.find(|c: char| !c.is_whitespace())
            && rest[k..].starts_with(';')
        {
            end += k + 1;
        }
        statements.push(at + body_at + found..at + body_at + end);
        from = end;
    }
    Some(Unwrapped {
        statements,
        guarded,
    })
}

/// Walk a PL/pgSQL statement (or a list of them), collecting DDL; `None`
/// as soon as anything else turns up.
fn collect(value: &Value, out: &mut Vec<String>, guarded: &mut bool) -> Option<()> {
    if let Some(list) = value.as_array() {
        for item in list {
            collect(item, out, guarded)?;
        }
        return Some(());
    }
    let (kind, fields) = value.as_object()?.iter().next()?;
    match kind.as_str() {
        "PLpgSQL_stmt_block" => {
            if let Some(exceptions) = fields.get("exceptions") {
                let handlers = exceptions["PLpgSQL_exception_block"]["exc_list"].as_array()?;
                for handler in handlers {
                    let handler = &handler["PLpgSQL_exception"];
                    let conditions = handler["conditions"].as_array()?;
                    if !conditions.iter().all(|c| {
                        c["PLpgSQL_condition"]["condname"]
                            .as_str()
                            .is_some_and(guard_condition)
                    }) {
                        return None;
                    }
                    // `THEN NULL` (no statements) or a notice.
                    for action in handler["action"].as_array().into_iter().flatten() {
                        quiet(action)?;
                    }
                }
                *guarded = true;
            }
            collect(fields.get("body").unwrap_or(&Value::Null), out, guarded)
        }
        "PLpgSQL_stmt_if" => {
            let condition = fields["cond"]["PLpgSQL_expr"]["query"].as_str()?;
            let has_else = fields.get("elsif_list").is_some()
                || fields
                    .get("else_body")
                    .and_then(Value::as_array)
                    .is_some_and(|e| !e.is_empty());
            if has_else || !catalog_check(condition) {
                return None;
            }
            *guarded = true;
            collect(
                fields.get("then_body").unwrap_or(&Value::Null),
                out,
                guarded,
            )
        }
        "PLpgSQL_stmt_execsql" => {
            if fields.get("into").is_some() || fields.get("target").is_some() {
                return None;
            }
            let query = fields["sqlstmt"]["PLpgSQL_expr"]["query"].as_str()?;
            // One statement, and one the replay keeps (DDL).
            let parsed = pg_query::parse(query).ok()?;
            let [raw] = parsed.protobuf.stmts.as_slice() else {
                return None;
            };
            let node = raw.stmt.as_ref()?.node.as_ref()?;
            if skip_reason(node).is_some() || matches!(node, NodeEnum::DoStmt(_)) {
                return None;
            }
            out.push(query.to_owned());
            Some(())
        }
        // The `RETURN` the function wrapper ends with.
        "PLpgSQL_stmt_return" if fields.get("expr").is_none() => Some(()),
        "PLpgSQL_stmt_raise" | "PLpgSQL_stmt_null" => quiet(value),
        _ => None,
    }
}

/// A statement that does nothing to the schema: `NULL`, or a `RAISE` below
/// `EXCEPTION`.
fn quiet(value: &Value) -> Option<()> {
    let (kind, fields) = value.as_object()?.iter().next()?;
    match kind.as_str() {
        "PLpgSQL_stmt_null" => Some(()),
        // elog levels: an exception is 21 (ERROR); notices and the like
        // are lower.
        "PLpgSQL_stmt_raise" => (fields["elog_level"].as_i64().unwrap_or(21) < 21).then_some(()),
        _ => None,
    }
}

/// `[NOT] EXISTS (SELECT … FROM pg_… / information_schema…)`: whether an
/// object is there, nothing else.
fn catalog_check(condition: &str) -> bool {
    let lower = condition.trim().to_ascii_lowercase();
    let exists = lower.starts_with("not exists") || lower.starts_with("exists");
    exists && (lower.contains("pg_") || lower.contains("information_schema"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::parse;

    fn unwrapped(sql: &str) -> Option<(Vec<&str>, bool)> {
        let parsed = parse(sql);
        let statement = &parsed.statements[0];
        let at = statement.span.start;
        unwrap(statement.node(), &sql[statement.span.range()], at).map(|u| {
            (
                u.statements.iter().map(|r| &sql[r.clone()]).collect(),
                u.guarded,
            )
        })
    }

    #[test]
    fn rails_create_enum_is_unwrapped_and_guarded() {
        let sql = "DO $$\nBEGIN\n    IF NOT EXISTS (\n      SELECT 1\n      FROM pg_type t\n      JOIN pg_namespace n ON t.typnamespace = n.oid\n      WHERE t.typname = 'user_role'\n        AND n.nspname = ANY (current_schemas(false))\n    ) THEN\n        CREATE TYPE \"user_role\" AS ENUM ('owner', 'admin', 'member');\n    END IF;\nEND\n$$;";
        assert_eq!(
            unwrapped(sql),
            Some((
                vec!["CREATE TYPE \"user_role\" AS ENUM ('owner', 'admin', 'member');"],
                true
            ))
        );
    }

    #[test]
    fn exception_guards_and_plain_blocks_are_unwrapped() {
        let drizzle = "DO $$ BEGIN\n ALTER TABLE \"posts\" ADD CONSTRAINT \"posts_author_fk\" FOREIGN KEY (\"author_id\") REFERENCES \"public\".\"users\"(\"id\") ON DELETE cascade ON UPDATE no action;\nEXCEPTION\n WHEN duplicate_object THEN null;\nEND $$;";
        let (statements, guarded) = unwrapped(drizzle).unwrap();
        assert!(guarded);
        assert_eq!(statements.len(), 1);
        assert!(statements[0].starts_with("ALTER TABLE \"posts\""));
        assert!(statements[0].ends_with("no action;"));

        let plain = "DO $body$ BEGIN CREATE TABLE a (id int); CREATE INDEX ON a (id); END $body$";
        assert_eq!(
            unwrapped(plain),
            Some((
                vec!["CREATE TABLE a (id int);", "CREATE INDEX ON a (id);"],
                false
            ))
        );
    }

    #[test]
    fn code_data_and_conditions_on_anything_else_stay_skipped() {
        for sql in [
            "DO $$ BEGIN UPDATE users SET x = 1; END $$",
            "DO $$ BEGIN PERFORM pg_sleep(1); END $$",
            "DO $$ DECLARE r record; BEGIN FOR r IN SELECT * FROM t LOOP EXECUTE 'DROP TABLE ' || r.name; END LOOP; END $$",
            "DO $$ BEGIN IF current_setting('app.env') = 'prod' THEN CREATE INDEX ON t (x); END IF; END $$",
            "DO $$ BEGIN IF NOT EXISTS (SELECT 1 FROM pg_type WHERE typname = 'm') THEN CREATE TYPE m AS ENUM ('a'); ELSE DROP TYPE m; END IF; END $$",
            "DO $$ BEGIN CREATE TABLE t (id int); EXCEPTION WHEN division_by_zero THEN NULL; END $$",
            "DO $$ BEGIN CREATE TABLE t (id int); RAISE EXCEPTION 'no'; END $$",
            "DO 'BEGIN CREATE TABLE t (id int); END'",
            "DO LANGUAGE plpython3u $$ import os $$",
            "DO $$ BEGIN EXECUTE 'CREATE TABLE t (id int)'; END $$",
            "DO $$ BEGIN END $$",
        ] {
            assert_eq!(unwrapped(sql), None, "{sql}");
        }
    }
}
