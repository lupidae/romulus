//! A whole schema as a tool dumps it (`pg_dump --schema-only`), reduced to
//! what a schema version holds, for an upload: what a migration import
//! skips (psql meta-commands, settings, ownership and privileges, data…: see
//! [`super::statements`]), and the functions, procedures, triggers and rules
//! an import would replay as stubs, which the schema model does not hold
//! (calls to them are stubbed when the rest compiles). Every statement left
//! out is blanked, so the lines and columns of the rest are the dump's own,
//! and listed.

use pg_query::{NodeEnum, protobuf::ObjectType};

use super::{
    lexer,
    statements::{SkipReason, Skipped, analyze, first_line, semicolon_after},
};
use crate::{allowlist::label, parse::parse};

/// A dump reduced to its schema.
#[derive(Debug, Clone)]
pub struct SchemaOnly {
    /// The text with what was left out blanked (lines kept), each line's
    /// trailing spaces trimmed.
    pub sql: String,
    /// What was left out, by line.
    pub skipped: Vec<Skipped>,
}

/// `sql` reduced to what a schema version holds (see the module docs).
pub fn schema_only(sql: &str) -> SchemaOnly {
    let analysis = analyze(sql);
    let parsed = parse(&analysis.sql);
    let mut skipped = analysis.skipped;
    let mut blanks = Vec::new();
    for statement in &parsed.statements {
        if !is_code(&statement.node) {
            continue;
        }
        let span = statement.span;
        skipped.push(Skipped {
            line: span.from.line,
            kind: label(&analysis.sql, parsed.tokens_of(statement)),
            reason: SkipReason::Code,
            text: first_line(&analysis.sql[span.range()]),
        });
        blanks.push(span.start..semicolon_after(&analysis.sql, span.end));
    }
    skipped.sort_by_key(|s| s.line);
    let blanked = lexer::blank(&analysis.sql, blanks);
    let mut sql = blanked
        .lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n");
    sql.push('\n');
    SchemaOnly { sql, skipped }
}

/// Functions, procedures, triggers and rules, and what is about them.
fn is_code(node: &NodeEnum) -> bool {
    use NodeEnum as N;
    let code = |object: ObjectType| {
        matches!(
            object,
            ObjectType::ObjectFunction
                | ObjectType::ObjectProcedure
                | ObjectType::ObjectRoutine
                | ObjectType::ObjectTrigger
                | ObjectType::ObjectRule
        )
    };
    match node {
        N::CreateFunctionStmt(_)
        | N::AlterFunctionStmt(_)
        | N::CreateTrigStmt(_)
        | N::RuleStmt(_) => true,
        N::CommentStmt(s) => code(s.objtype()),
        N::DropStmt(s) => code(s.remove_type()),
        N::RenameStmt(s) => code(s.rename_type()),
        N::AlterObjectSchemaStmt(s) => code(s.object_type()),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DUMP: &str = "\\restrict abc123\n\
SET statement_timeout = 0;\n\
SELECT pg_catalog.set_config('search_path', '', false);\n\
CREATE TABLE public.orders (id bigint NOT NULL);\n\
ALTER TABLE public.orders OWNER TO app;\n\
CREATE FUNCTION public.touch() RETURNS trigger\n    LANGUAGE plpgsql\n    AS $$ BEGIN RETURN NEW; END $$;\n\
CREATE TRIGGER orders_touch BEFORE UPDATE ON public.orders FOR EACH ROW EXECUTE FUNCTION public.touch();\n\
COMMENT ON FUNCTION public.touch() IS 'touches';\n\
GRANT SELECT ON public.orders TO reader;\n\
\\unrestrict abc123\n";

    #[test]
    fn a_dump_keeps_its_schema_and_its_lines() {
        let reduced = schema_only(DUMP);
        let kept: Vec<&str> = reduced.sql.lines().filter(|l| !l.is_empty()).collect();
        assert_eq!(kept, ["CREATE TABLE public.orders (id bigint NOT NULL);"]);
        // The table stays on its own line: errors point into the dump.
        assert_eq!(reduced.sql.lines().nth(3), Some(kept[0]));
        assert_eq!(reduced.sql.lines().count(), DUMP.lines().count());
        let reasons: Vec<(u32, SkipReason)> =
            reduced.skipped.iter().map(|s| (s.line, s.reason)).collect();
        assert_eq!(
            reasons,
            [
                (1, SkipReason::Psql),
                (2, SkipReason::Settings),
                (3, SkipReason::Code),
                (5, SkipReason::Privileges),
                (6, SkipReason::Code),
                (9, SkipReason::Code),
                (10, SkipReason::Code),
                (11, SkipReason::Privileges),
                (12, SkipReason::Psql),
            ]
        );
    }
}
