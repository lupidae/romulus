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
//! condition is not evaluated).
//!
//! A guarded block keeps its guard, and only its own: the replay runs each
//! statement in a fixed wrapper that ignores exactly the errors the block
//! did. `IF NOT EXISTS (…catalogs…)` ignores "already exists" (the
//! `duplicate_*` conditions), `IF EXISTS (…catalogs…)` ignores "does not
//! exist" (`undefined_*`), and an `EXCEPTION WHEN …` handler ignores the
//! conditions it names (any condition or SQLSTATE, `others` included, as
//! the migration did on the real database: pg_dump-style guards ignore
//! `duplicate_object OR duplicate_table OR invalid_table_definition`), but
//! never `query_canceled` or `assert_failure` (the replay's own statement
//! timeout must stay an error). Any other block (loops, variables, data, a
//! `RETURN` before its end, a condition on anything but the catalogs, a
//! handler that does something) is skipped and reported.

use std::ops::Range;

use pg_query::{NodeEnum, protobuf::Token as TokenKind};
use serde_json::Value;

use super::statements::skip_reason;
use crate::walk;

/// The statements of a `DO` block that is only DDL, located in the text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Unwrapped {
    /// Each statement's range in the text, its `;` included when the body
    /// has one right after it.
    pub statements: Vec<Range<usize>>,
    /// The conditions the block ignored, as they go after `WHEN` (names, or
    /// `SQLSTATE '42P07'`); empty when it was not guarded.
    pub guard: Vec<String>,
}

/// What `IF NOT EXISTS (…)` guards against: the object is there already.
const DUPLICATES: &[&str] = &[
    "duplicate_object",
    "duplicate_table",
    "duplicate_column",
    "duplicate_schema",
    "duplicate_function",
    "duplicate_alias",
];

/// What `IF EXISTS (…)` guards against: the object is not there.
const UNDEFINED: &[&str] = &[
    "undefined_object",
    "undefined_table",
    "undefined_column",
    "undefined_function",
];

/// Conditions no guard may ignore: the replay's statement timeout
/// (`query_canceled`) and assertions, by name or SQLSTATE.
const NEVER_IGNORED: &[&str] = &["query_canceled", "assert_failure", "57014", "P0004"];

/// A handler's condition, as it goes after `WHEN` in the replay's wrapper
/// (a name, or `SQLSTATE '…'`); `None` for the conditions no guard may
/// ignore. The PL/pgSQL parser already refused names it does not know, and
/// a name or code here is plain characters: nothing in it can end the
/// wrapper's handler.
fn guard_condition(name: &str) -> Option<String> {
    if NEVER_IGNORED.contains(&name) {
        return None;
    }
    let identifier = name.as_bytes().first().is_some_and(u8::is_ascii_lowercase)
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
    if identifier {
        return Some(name.to_owned());
    }
    let sqlstate = name.len() == 5
        && name
            .bytes()
            .all(|b| b.is_ascii_digit() || b.is_ascii_uppercase());
    sqlstate.then(|| format!("SQLSTATE '{name}'"))
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
    // PL/pgSQL. A body using this very tag is not unwrapped.
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
    // The block's statements, without the RETURNs that end it (the
    // wrapper's own, and one the body may end with): a RETURN anywhere else
    // would stop what follows it.
    let action = &function["action"]["PLpgSQL_stmt_block"];
    let mut top: Vec<Value> = action["body"].as_array()?.clone();
    while top.last().is_some_and(|s| {
        s.get("PLpgSQL_stmt_return")
            .is_some_and(|r| r.get("expr").is_none())
    }) {
        top.pop();
    }
    if action.get("exceptions").is_some() {
        return None;
    }
    let mut queries = Vec::new();
    let mut guard = Vec::new();
    collect(&Value::Array(top), &mut queries, &mut guard)?;
    if queries.is_empty() {
        return None;
    }

    let statements = locate(body, &queries)?
        .into_iter()
        .map(|r| at + body_at + r.start..at + body_at + r.end)
        .collect();
    guard.sort();
    guard.dedup();
    Some(Unwrapped { statements, guard })
}

/// Where each query is in the body, in order, outside its comments and
/// strings (a statement's text may also appear in a comment above it).
fn locate(body: &str, queries: &[String]) -> Option<Vec<Range<usize>>> {
    let tokens = pg_query::scan(body).ok()?.tokens;
    let opaque: Vec<Range<usize>> = tokens
        .iter()
        .filter(|t| {
            matches!(
                t.token(),
                TokenKind::SqlComment
                    | TokenKind::CComment
                    | TokenKind::Sconst
                    | TokenKind::Usconst
            )
        })
        .filter_map(|t| Some(usize::try_from(t.start).ok()?..usize::try_from(t.end).ok()?))
        .collect();
    let inside = |at: usize| opaque.iter().any(|r| r.start <= at && at < r.end);
    let mut out = Vec::with_capacity(queries.len());
    let mut from = 0;
    for query in queries {
        let found = body[from..]
            .match_indices(query.as_str())
            .map(|(k, _)| from + k)
            .find(|&k| !inside(k))?;
        let mut end = found + query.len();
        let rest = &body[end..];
        if let Some(k) = rest.find(|c: char| !c.is_whitespace())
            && rest[k..].starts_with(';')
        {
            end += k + 1;
        }
        out.push(found..end);
        from = end;
    }
    Some(out)
}

/// Walk PL/pgSQL statements, collecting DDL and the conditions guarding
/// it; `None` as soon as anything else turns up.
fn collect(value: &Value, out: &mut Vec<String>, guard: &mut Vec<String>) -> Option<()> {
    if let Some(list) = value.as_array() {
        for item in list {
            collect(item, out, guard)?;
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
                    for condition in handler["conditions"].as_array()? {
                        let name = condition["PLpgSQL_condition"]["condname"].as_str()?;
                        guard.push(guard_condition(name)?);
                    }
                    // `THEN NULL` (no statements) or a notice.
                    for action in handler["action"].as_array().into_iter().flatten() {
                        quiet(action)?;
                    }
                }
            }
            collect(fields.get("body").unwrap_or(&Value::Null), out, guard)
        }
        "PLpgSQL_stmt_if" => {
            let condition = fields["cond"]["PLpgSQL_expr"]["query"].as_str()?;
            let has_else = fields.get("elsif_list").is_some()
                || fields
                    .get("else_body")
                    .and_then(Value::as_array)
                    .is_some_and(|e| !e.is_empty());
            if has_else {
                return None;
            }
            let guards = match catalog_check(condition)? {
                Exists::Absent => DUPLICATES,
                Exists::Present => UNDEFINED,
            };
            guard.extend(guards.iter().map(|g| (*g).to_owned()));
            collect(fields.get("then_body").unwrap_or(&Value::Null), out, guard)
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

/// What a catalog check tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Exists {
    /// `NOT EXISTS (…)`: run when the object is absent.
    Absent,
    /// `EXISTS (…)`: run when it is there.
    Present,
}

/// `[NOT] EXISTS (SELECT … FROM <catalogs>)`: whether an object is there,
/// read only from `pg_catalog` relations or `information_schema`. `None`
/// for any other condition.
fn catalog_check(condition: &str) -> Option<Exists> {
    let parsed = pg_query::parse(&format!("SELECT {condition}")).ok()?;
    let [raw] = parsed.protobuf.stmts.as_slice() else {
        return None;
    };
    let Some(NodeEnum::SelectStmt(select)) = raw.stmt.as_ref()?.node.as_ref() else {
        return None;
    };
    let [target] = select.target_list.as_slice() else {
        return None;
    };
    let Some(NodeEnum::ResTarget(target)) = target.node.as_ref() else {
        return None;
    };
    let value = serde_json::to_value(target.val.as_deref()?).ok()?;
    let (sublink, exists) = match value["node"].as_object()?.iter().next()? {
        (kind, fields) if kind == "BoolExpr" => {
            // NOT_EXPR around one EXISTS sublink.
            if fields["boolop"].as_i64() != Some(3) {
                return None;
            }
            let [arg] = fields["args"].as_array()?.as_slice() else {
                return None;
            };
            (arg["node"]["SubLink"].clone(), Exists::Absent)
        }
        (kind, fields) if kind == "SubLink" => (fields.clone(), Exists::Present),
        _ => return None,
    };
    // EXISTS_SUBLINK.
    if sublink["sub_link_type"].as_i64() != Some(1) {
        return None;
    }
    let mut catalogs_only = true;
    let mut relations = 0;
    walk::visit(&sublink["subselect"], &mut |kind, fields| match kind {
        "RangeVar" => {
            relations += 1;
            let schema = fields["schemaname"].as_str().unwrap_or_default();
            let name = fields["relname"].as_str().unwrap_or_default();
            let catalog = (schema.is_empty() || schema == "pg_catalog") && name.starts_with("pg_");
            if !catalog && schema != "information_schema" {
                catalogs_only = false;
            }
        }
        "RangeFunction" | "RangeTableFunc" => catalogs_only = false,
        _ => {}
    });
    (catalogs_only && relations > 0).then_some(exists)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::parse;

    fn unwrapped(sql: &str) -> Option<(Vec<&str>, Vec<String>)> {
        let parsed = parse(sql);
        let statement = &parsed.statements[0];
        let at = statement.span.start;
        unwrap(statement.node(), &sql[statement.span.range()], at).map(|u| {
            (
                u.statements.iter().map(|r| &sql[r.clone()]).collect(),
                u.guard,
            )
        })
    }

    #[test]
    fn rails_create_enum_is_unwrapped_and_guarded_against_duplicates() {
        let sql = "DO $$\nBEGIN\n    IF NOT EXISTS (\n      SELECT 1\n      FROM pg_type t\n      JOIN pg_namespace n ON t.typnamespace = n.oid\n      WHERE t.typname = 'user_role'\n        AND n.nspname = ANY (current_schemas(false))\n    ) THEN\n        CREATE TYPE \"user_role\" AS ENUM ('owner', 'admin', 'member');\n    END IF;\nEND\n$$;";
        let (statements, guard) = unwrapped(sql).unwrap();
        assert_eq!(
            statements,
            ["CREATE TYPE \"user_role\" AS ENUM ('owner', 'admin', 'member');"]
        );
        assert!(
            guard.iter().all(|g| g.starts_with("duplicate_")),
            "{guard:?}"
        );
    }

    #[test]
    fn exception_handlers_guard_with_exactly_their_conditions() {
        let drizzle = "DO $$ BEGIN\n ALTER TABLE \"posts\" ADD CONSTRAINT \"posts_author_fk\" FOREIGN KEY (\"author_id\") REFERENCES \"public\".\"users\"(\"id\") ON DELETE cascade ON UPDATE no action;\nEXCEPTION\n WHEN duplicate_object THEN null;\nEND $$;";
        let (statements, guard) = unwrapped(drizzle).unwrap();
        assert_eq!(guard, ["duplicate_object"]);
        assert_eq!(statements.len(), 1);
        assert!(statements[0].ends_with("no action;"));

        let codes = "DO $$ BEGIN CREATE TABLE a (); EXCEPTION WHEN SQLSTATE '42P07' OR others THEN NULL; END $$";
        assert_eq!(unwrapped(codes).unwrap().1, ["SQLSTATE '42P07'", "others"]);

        // pg_dump-made schemas made re-runnable: every object guarded, by
        // more than the duplicate conditions.
        let dumped = "DO $$ BEGIN\nALTER TABLE ONLY public.a\n    ADD CONSTRAINT a_b_fkey FOREIGN KEY (b) REFERENCES public.b(id);\nEXCEPTION WHEN duplicate_object OR duplicate_table OR invalid_table_definition THEN NULL; END $$;";
        let (statements, guard) = unwrapped(dumped).unwrap();
        assert_eq!(statements.len(), 1);
        assert_eq!(
            guard,
            [
                "duplicate_object",
                "duplicate_table",
                "invalid_table_definition"
            ]
        );
        let division = "DO $$ BEGIN CREATE TABLE a (); EXCEPTION WHEN division_by_zero OR SQLSTATE '22012' THEN NULL; END $$";
        assert_eq!(
            unwrapped(division).unwrap().1,
            ["SQLSTATE '22012'", "division_by_zero"]
        );

        let plain =
            "DO $body$ BEGIN CREATE TABLE a (id int); CREATE INDEX ON a (id); RETURN; END $body$";
        assert_eq!(
            unwrapped(plain),
            Some((
                vec!["CREATE TABLE a (id int);", "CREATE INDEX ON a (id);"],
                vec![]
            ))
        );
    }

    #[test]
    fn statements_are_found_outside_comments_and_strings() {
        let sql = "DO $$ BEGIN /* CREATE TABLE a (x int); */ RAISE NOTICE 'CREATE TABLE a (x int)'; CREATE TABLE a (x int); END $$";
        let (statements, _) = unwrapped(sql).unwrap();
        let start = sql.find("; CREATE TABLE a").unwrap() + 2;
        assert_eq!(statements, ["CREATE TABLE a (x int);"]);
        let parsed = parse(sql);
        let found = unwrap(parsed.statements[0].node(), sql, 0).unwrap();
        assert_eq!(found.statements[0].start, start);
    }

    #[test]
    fn code_data_and_conditions_on_anything_else_stay_skipped() {
        for sql in [
            "DO $$ BEGIN UPDATE users SET x = 1; END $$",
            "DO $$ BEGIN PERFORM pg_sleep(1); END $$",
            "DO $$ DECLARE r record; BEGIN FOR r IN SELECT * FROM t LOOP EXECUTE 'DROP TABLE ' || r.name; END LOOP; END $$",
            "DO $$ BEGIN IF current_setting('app.env') = 'prod' THEN CREATE INDEX ON t (x); END IF; END $$",
            "DO $$ BEGIN IF NOT EXISTS (SELECT 1 FROM my_pg_tables) THEN CREATE TABLE t (); END IF; END $$",
            "DO $$ BEGIN IF NOT EXISTS (SELECT 1 FROM public.pg_lookalike) THEN CREATE TABLE t (); END IF; END $$",
            "DO $$ BEGIN IF NOT EXISTS (SELECT 1 FROM pg_class, generate_series(1, 10)) THEN CREATE TABLE t (); END IF; END $$",
            "DO $$ BEGIN IF NOT EXISTS (SELECT 1 FROM pg_type WHERE typname = 'm') THEN CREATE TYPE m AS ENUM ('a'); ELSE DROP TYPE m; END IF; END $$",
            "DO $$ BEGIN CREATE TABLE t (id int); EXCEPTION WHEN query_canceled THEN NULL; END $$",
            "DO $$ BEGIN CREATE TABLE t (id int); EXCEPTION WHEN SQLSTATE '57014' THEN NULL; END $$",
            "DO $$ BEGIN CREATE TABLE t (id int); EXCEPTION WHEN duplicate_table OR assert_failure THEN NULL; END $$",
            "DO $$ BEGIN CREATE TABLE t (id int); RAISE EXCEPTION 'no'; END $$",
            "DO $$ BEGIN RETURN; CREATE TABLE t (id int); END $$",
            "DO 'BEGIN CREATE TABLE t (id int); END'",
            "DO LANGUAGE plpython3u $$ import os $$",
            "DO $$ BEGIN EXECUTE 'CREATE TABLE t (id int)'; END $$",
            "DO $$ BEGIN END $$",
        ] {
            assert_eq!(unwrapped(sql), None, "{sql}");
        }
    }

    /// Bodies the PL/pgSQL parser reads in many shapes: anything but plain
    /// DDL stays skipped, and nothing panics.
    #[test]
    fn odd_bodies_are_skipped_without_panicking() {
        for body in [
            "DECLARE x t.c%TYPE; BEGIN CREATE TABLE a(); END",
            "DECLARE r t%ROWTYPE; BEGIN CREATE TABLE a(); END",
            "BEGIN FOREACH x IN ARRAY y LOOP END LOOP; END",
            "BEGIN RETURN QUERY SELECT 1; END",
            "DECLARE c CURSOR FOR SELECT 1; BEGIN OPEN c; END",
            "BEGIN GET DIAGNOSTICS x = ROW_COUNT; END",
            "DECLARE x int := 1/0; BEGIN END",
            "BEGIN ASSERT false; END",
            "BEGIN COMMIT; END",
            "BEGIN CALL p(); END",
            "BEGIN IF EXISTS (SELECT pg_sleep(10) FROM pg_class) THEN CREATE TABLE a(); END IF; END",
            "DECLARE x record; BEGIN SELECT 1 INTO x; END",
            "BEGIN PERFORM 1; END",
            "BEGIN CASE WHEN true THEN NULL; END CASE; END",
            "BEGIN EXIT; END",
            "BEGIN RAISE EXCEPTION USING MESSAGE = 'x'; END",
            "BEGIN RAISE; END",
            "DECLARE x int[]; BEGIN x[1] := 1; END",
        ] {
            let sql = format!("DO $$ {body} $$;");
            let parsed = parse(&sql);
            let statement = &parsed.statements[0];
            let found = unwrap(statement.node(), &sql[statement.span.range()], 0);
            let ddl_only = body.contains("pg_sleep(10)");
            assert_eq!(found.is_some(), ddl_only, "{body}");
        }
        // Statements labelled or with compiler options still unwrap.
        for body in [
            "<<l>> BEGIN CREATE TABLE a(); END l",
            "#variable_conflict use_column\nBEGIN CREATE TABLE a(); END",
        ] {
            let sql = format!("DO $$ {body} $$;");
            let parsed = parse(&sql);
            let statement = &parsed.statements[0];
            assert!(
                unwrap(statement.node(), &sql[statement.span.range()], 0).is_some(),
                "{body}"
            );
        }
    }
}
