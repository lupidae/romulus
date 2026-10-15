//! Errors point at the user's text, whatever the compiler added to it.

use romulus_sandbox::{Code, Compilation, Diagnostic, Position};

use crate::{compile_ok, explain, with_compiler};

fn only_error(sql: &str, compilation: &Compilation) -> Diagnostic {
    let errors: Vec<&Diagnostic> = compilation.errors().collect();
    assert_eq!(errors.len(), 1, "{}", explain(sql, compilation));
    errors[0].clone()
}

fn at<'a>(sql: &'a str, diagnostic: &Diagnostic) -> (&'a str, Position) {
    let span = diagnostic.span.expect("located");
    (&sql[span.range()], span.from)
}

#[test]
fn a_positioned_error_underlines_its_token_after_stubs_and_rewrites() {
    with_compiler(|compiler| async move {
        // A stub and a policy rewrite before the error shift the script;
        // the error must still land on `id2` in the user's text.
        let sql = "CREATE TABLE a (id int DEFAULT app.next_id());\n\
                   CREATE POLICY p ON a TO admin, auditor USING (id = 1);\n\
                   CREATE INDEX ON a (app.next_id(), id2);";
        let compilation = compiler.compile(sql).await.expect("the sandbox runs");
        let error = only_error(sql, &compilation);
        assert_eq!(error.code, Code::Sql);
        assert_eq!(error.sqlstate.as_deref(), Some("42703"));
        assert_eq!(error.message, "column \"id2\" does not exist");
        assert_eq!(
            at(sql, &error),
            (
                "id2",
                Position {
                    line: 3,
                    column: 35
                }
            )
        );
        assert_eq!(error.statement, Some(2));
    });
}

#[test]
fn an_error_without_a_position_underlines_the_name_it_quotes() {
    with_compiler(|compiler| async move {
        let sql = "CREATE TABLE a (id int);\n\
                   -- a comment\n\
                   CREATE TABLE b (a_id int REFERENCES nope (id));\n\
                   CREATE TABLE c (id int);";
        let compilation = compiler.compile(sql).await.expect("the sandbox runs");
        let error = only_error(sql, &compilation);
        assert_eq!(error.message, "relation \"nope\" does not exist");
        assert_eq!(
            at(sql, &error),
            (
                "nope",
                Position {
                    line: 3,
                    column: 37
                }
            )
        );
        assert_eq!(error.statement, Some(1));

        let sql = "CREATE TABLE twice (id int);\nCREATE TABLE twice (id int);";
        let compilation = compiler.compile(sql).await.expect("the sandbox runs");
        let error = only_error(sql, &compilation);
        assert_eq!(error.message, "relation \"twice\" already exists");
        assert_eq!(
            at(sql, &error),
            (
                "twice",
                Position {
                    line: 2,
                    column: 14
                }
            )
        );

        // A name the statement does not spell exactly once: the statement.
        let sql = "CREATE TABLE p (id int);\nCREATE TABLE q (id int, CONSTRAINT k CHECK (id > 0), CONSTRAINT k CHECK (id < 9));";
        let compilation = compiler.compile(sql).await.expect("the sandbox runs");
        let error = only_error(sql, &compilation);
        assert_eq!(at(sql, &error).1, Position { line: 2, column: 1 });
    });
}

#[test]
fn positions_count_characters_and_utf16_for_the_editor() {
    with_compiler(|compiler| async move {
        let sql = "-- café ☕ 🐘\nCREATE TABLE \"Été\" (x int DEFAULT nope_col + 1);";
        let compilation = compiler.compile(sql).await.expect("the sandbox runs");
        let error = only_error(sql, &compilation);
        let span = error.span.unwrap();
        assert_eq!(&sql[span.range()], "nope_col");
        assert_eq!(
            span.from,
            Position {
                line: 2,
                column: 35
            }
        );
        // "-- café ☕ 🐘\n" is 13 UTF-16 units; the column is 34 more.
        assert_eq!(span.utf16_start, 13 + 34);
    });
}

#[test]
fn syntax_errors_are_all_reported_without_running_anything() {
    with_compiler(|compiler| async move {
        let sql = "CREATE TABL a (id int);\nCREATE TABLE b (id int,);\nCREATE TABLE ok (id int);\nCREATE TABLE c (";
        let compilation = compiler.compile(sql).await.expect("the sandbox runs");
        let errors: Vec<(String, Position)> = compilation
            .errors()
            .map(|d| (d.message.clone(), d.span.unwrap().from))
            .collect();
        assert_eq!(
            errors,
            [
                (
                    "syntax error at or near \"TABL\"".to_owned(),
                    Position { line: 1, column: 8 }
                ),
                (
                    "syntax error at or near \")\"".to_owned(),
                    Position {
                        line: 2,
                        column: 24
                    }
                ),
                (
                    "syntax error at end of input".to_owned(),
                    Position {
                        line: 4,
                        column: 17
                    }
                ),
            ]
        );
        assert!(compilation.errors().all(|d| d.code == Code::SyntaxError));
        assert_eq!(compilation.statements, 1);
    });
}

#[test]
fn a_missing_extension_type_says_which_extension_to_create() {
    with_compiler(|compiler| async move {
        // The DEFAULT's stub would return geometry, which does not exist
        // either: the stub is dropped and the column's own error shows.
        let sql =
            "CREATE TABLE spots (\n  id int,\n  at geometry(Point, 4326) DEFAULT make_point()\n);";
        let compilation = compiler.compile(sql).await.expect("the sandbox runs");
        let error = only_error(sql, &compilation);
        assert_eq!(error.message, "type \"geometry\" does not exist");
        assert_eq!(
            at(sql, &error),
            ("geometry", Position { line: 3, column: 6 })
        );
        assert_eq!(
            error.hint.as_deref(),
            Some(
                "geometry comes with extension postgis: add CREATE EXTENSION IF NOT EXISTS postgis first."
            )
        );

        let sql = "CREATE TABLE ids (id uuid DEFAULT uuid_generate_v4())";
        let compilation = compile_ok(compiler, sql).await;
        // Without the extension it is just a function the text does not
        // define: stubbed, and the model is the same as with it.
        assert_eq!(
            compilation.stubs[0].signature(),
            "public.uuid_generate_v4() → uuid"
        );
    });
}

#[test]
fn stub_types_are_corrected_from_postgres_errors() {
    with_compiler(|compiler| async move {
        // A CASE hides the argument's type from the inference (text), and a
        // sub-query the comparison's (text): Postgres says uuid and numeric.
        let sql = "CREATE TABLE t (id uuid, n numeric, CHECK (app.valid(CASE WHEN true THEN id END)));\n\
                   CREATE POLICY p ON t USING (app.level() = (SELECT max(n) FROM t));";
        let compilation = compile_ok(compiler, sql).await;
        let signatures: Vec<String> = compilation
            .stubs
            .iter()
            .map(romulus_sandbox::Stub::signature)
            .collect();
        assert_eq!(
            signatures,
            ["app.valid(uuid) → boolean", "app.level() → numeric"]
        );
    });
}

#[test]
fn errors_inside_a_rewritten_policy_still_point_at_the_policy() {
    with_compiler(|compiler| async move {
        let sql = "CREATE TABLE t (id int);\nCREATE POLICY p ON t TO reader USING (missing = 1);";
        let compilation = compiler.compile(sql).await.expect("the sandbox runs");
        let error = only_error(sql, &compilation);
        assert_eq!(
            at(sql, &error),
            (
                "missing",
                Position {
                    line: 2,
                    column: 39
                }
            )
        );
    });
}
