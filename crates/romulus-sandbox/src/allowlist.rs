//! The DDL allowlist, checked on the server before the sandbox sees the text.
//!
//! The sandbox runs schema DDL only: `CREATE`/`ALTER`/`DROP` of schemas,
//! tables, views, materialized views, indexes, sequences, types, domains,
//! policies and allowlisted extensions, `COMMENT ON`, renames, and `SET
//! search_path`. Everything else is refused with a located error: DML, `DO`,
//! `COPY`, transaction control, functions, triggers, privileges, ownership,
//! roles and anything cluster-wide.
//!
//! Allowed statements can still make Postgres evaluate expressions while it
//! compiles: partition bounds are evaluated, and immutable calls with
//! constant arguments are folded in defaults added by `ALTER TABLE`, CHECKs
//! added by `ALTER TABLE`, index and partition-key expressions, generated
//! columns and `ALTER … TYPE … USING`. Users cannot define functions, so
//! only built-ins run, and three rules keep that harmless:
//!
//! - functions with side effects or that run SQL given as text (`pg_sleep`,
//!   `pg_terminate_backend`, advisory locks, `set_config`, large objects,
//!   server files, `query_to_xml`, `ts_stat`…) are refused anywhere;
//! - functions whose output can dwarf their input (`repeat`, `lpad`,
//!   `replace`, `array_fill`, `format`…) are refused when all their
//!   arguments are constants, the case Postgres folds;
//! - partition bounds must be constants.
//!
//! Defence in depth behind it: no superuser, one role per sandbox database
//! (a backend cannot signal another compile's), `statement_timeout`,
//! `temp_file_limit`, materialized views always created `WITH NO DATA`, and
//! every compile rolled back.

use pg_query::{
    NodeEnum,
    protobuf::{AlterTableType, ObjectType, Token as TokenKind, VariableSetKind},
};
use serde_json::Value;

use crate::{
    catalog::ALLOWED_EXTENSIONS,
    diagnostic::{Code, Diagnostic},
    parse::{ParsedSql, Token},
    source::location,
    walk,
};

/// Functions refused anywhere: side effects outside the transaction, waits,
/// signals, locks, server files, or SQL run from a string.
const DENIED_FUNCTIONS: &[&str] = &[
    "pg_sleep",
    "pg_sleep_for",
    "pg_sleep_until",
    "pg_terminate_backend",
    "pg_cancel_backend",
    "pg_reload_conf",
    "pg_rotate_logfile",
    "pg_log_backend_memory_contexts",
    "pg_promote",
    "pg_switch_wal",
    "pg_create_restore_point",
    "pg_backup_start",
    "pg_backup_stop",
    "pg_wal_replay_pause",
    "pg_wal_replay_resume",
    "pg_advisory_lock",
    "pg_advisory_lock_shared",
    "pg_advisory_unlock",
    "pg_advisory_unlock_shared",
    "pg_advisory_unlock_all",
    "pg_advisory_xact_lock",
    "pg_advisory_xact_lock_shared",
    "pg_try_advisory_lock",
    "pg_try_advisory_lock_shared",
    "pg_try_advisory_xact_lock",
    "pg_try_advisory_xact_lock_shared",
    "set_config",
    "pg_notify",
    "lo_import",
    "lo_export",
    "lo_unlink",
    "lo_create",
    "lo_creat",
    "lo_from_bytea",
    "lo_put",
    "lo_open",
    "lo_close",
    "lo_write",
    "lowrite",
    "loread",
    "lo_get",
    "lo_truncate",
    "lo_truncate64",
    "lo_lseek",
    "lo_lseek64",
    "pg_read_file",
    "pg_read_binary_file",
    "pg_ls_dir",
    "pg_stat_file",
    "pg_ls_logdir",
    "pg_ls_waldir",
    "pg_ls_tmpdir",
    "pg_ls_archive_statusdir",
    "pg_ls_logicalsnapdir",
    "pg_ls_logicalmapdir",
    "pg_ls_replslotdir",
    "pg_current_logfile",
    "pg_create_physical_replication_slot",
    "pg_create_logical_replication_slot",
    "pg_drop_replication_slot",
    "pg_copy_physical_replication_slot",
    "pg_copy_logical_replication_slot",
    "pg_logical_slot_get_changes",
    "pg_logical_slot_peek_changes",
    "pg_logical_slot_get_binary_changes",
    "pg_logical_slot_peek_binary_changes",
    "pg_logical_emit_message",
    "pg_replication_slot_advance",
    "pg_replication_origin_create",
    "pg_replication_origin_drop",
    "pg_replication_origin_advance",
    "pg_replication_origin_session_setup",
    "pg_import_system_collations",
    "pg_stat_reset",
    "pg_stat_reset_shared",
    "pg_stat_reset_single_table_counters",
    "pg_stat_reset_single_function_counters",
    "pg_stat_reset_slru",
    "pg_stat_reset_replication_slot",
    "pg_stat_reset_subscription_stats",
    "pg_export_snapshot",
    "query_to_xml",
    "query_to_xmlschema",
    "query_to_xml_and_xmlschema",
    "cursor_to_xml",
    "cursor_to_xmlschema",
    "table_to_xml",
    "table_to_xmlschema",
    "table_to_xml_and_xmlschema",
    "schema_to_xml",
    "schema_to_xmlschema",
    "schema_to_xml_and_xmlschema",
    "database_to_xml",
    "database_to_xmlschema",
    "database_to_xml_and_xmlschema",
    "ts_stat",
    "crosstab",
    "connectby",
    "dblink",
    "dblink_exec",
];

/// Functions whose result can be far larger than their input, refused when
/// every argument is a constant (the calls Postgres folds while compiling).
const AMPLIFYING_FUNCTIONS: &[&str] = &[
    "repeat",
    "lpad",
    "rpad",
    "array_fill",
    "replace",
    "regexp_replace",
    "overlay",
    "format",
];

/// Identifier prefixes the text may not use: temporary schemas, and the
/// sandbox's own session objects.
const RESERVED_PREFIXES: &[&str] = &["pg_temp", "pg_toast", "romulus_sandbox_"];

const HINT_DATA: &str = "Data does not belong in a schema version: remove the statement (keep it in a migration or a seed file).";
const HINT_CODE: &str = "The sandbox only runs schema DDL; procedural code cannot run in it.";
const HINT_TRANSACTION: &str =
    "romulus runs the whole text in one transaction: remove BEGIN, COMMIT and SAVEPOINT.";
const HINT_PRIVILEGES: &str =
    "Privileges and ownership are not part of the schema model: remove the statement or clause.";
const HINT_FUNCTIONS: &str = "Functions and triggers are not part of the schema model: remove the definition. Calls to functions the text does not define are stubbed automatically.";
const HINT_CLUSTER: &str =
    "Roles, databases, tablespaces and server settings are outside a schema: remove the statement.";

/// Check every parsed statement against the allowlist.
pub fn check(parsed: &ParsedSql, sql: &str) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    for statement in &parsed.statements {
        let mut problems = Vec::new();
        check_statement(&statement.node, &mut problems);
        check_expressions(&statement.node, &mut problems);
        check_identifiers(sql, parsed.tokens_of(statement), &mut problems);
        let label = label(sql, parsed.tokens_of(statement));
        for problem in problems {
            let range = problem
                .at
                .and_then(|offset| parsed.token_at(offset))
                .unwrap_or_else(|| statement.span.range());
            let message = problem.message.replace("{statement}", &label);
            let mut diagnostic = Diagnostic::error(problem.code, message)
                .with_span(parsed.span(sql, range))
                .with_statement(statement.index);
            if let Some(hint) = problem.hint {
                diagnostic = diagnostic.with_hint(hint);
            }
            out.push(diagnostic);
        }
    }
    out
}

struct Problem {
    code: Code,
    message: String,
    hint: Option<String>,
    /// A byte offset to underline the token at; the whole statement if none.
    at: Option<usize>,
}

impl Problem {
    fn not_allowed(message: impl Into<String>, hint: &str) -> Self {
        Self {
            code: Code::NotAllowed,
            message: message.into(),
            hint: Some(hint.to_owned()),
            at: None,
        }
    }

    fn statement(hint: &str) -> Self {
        Self::not_allowed("{statement} is not allowed", hint)
    }
}

#[allow(clippy::too_many_lines)]
fn check_statement(node: &NodeEnum, out: &mut Vec<Problem>) {
    use NodeEnum as N;
    match node {
        N::CreateSchemaStmt(s) => {
            if s.authrole.is_some() {
                out.push(Problem::not_allowed(
                    "CREATE SCHEMA … AUTHORIZATION is not allowed",
                    HINT_PRIVILEGES,
                ));
            }
            for element in &s.schema_elts {
                if let Some(element) = element.node.as_ref() {
                    check_statement(element, out);
                }
            }
        }
        N::CreateStmt(s) => {
            if !s.tablespacename.is_empty() {
                out.push(Problem::not_allowed(
                    "TABLESPACE is not allowed",
                    "Tablespaces are not part of the schema model: remove the clause.",
                ));
            }
            if let Some(bound) = &s.partbound {
                check_partition_bound(bound, out);
            }
        }
        N::AlterTableStmt(s) => {
            if s.objtype() == ObjectType::ObjectForeignTable {
                out.push(Problem::statement(HINT_FOREIGN));
            }
            for cmd in &s.cmds {
                let Some(N::AlterTableCmd(cmd)) = cmd.node.as_ref() else {
                    continue;
                };
                check_alter_table_cmd(cmd, out);
            }
        }
        N::CreateTableAsStmt(s) => {
            if s.is_select_into {
                out.push(Problem::not_allowed(
                    "SELECT … INTO is not allowed",
                    "Create the table with its columns instead: a query cannot run in the sandbox.",
                ));
            } else if s.objtype() != ObjectType::ObjectMatview {
                out.push(Problem::not_allowed(
                    "CREATE TABLE … AS is not allowed",
                    "Create the table with its columns instead: a query cannot run in the sandbox.",
                ));
            }
        }
        N::IndexStmt(s) => {
            if s.concurrent {
                out.push(Problem::not_allowed(
                    "CREATE INDEX CONCURRENTLY is not allowed",
                    "Drop CONCURRENTLY: it cannot run inside the compile's transaction, and the schema is the same without it.",
                ));
            }
            if !s.table_space.is_empty() {
                out.push(Problem::not_allowed(
                    "TABLESPACE is not allowed",
                    "Tablespaces are not part of the schema model: remove the clause.",
                ));
            }
        }
        N::CreateExtensionStmt(s) => {
            if !ALLOWED_EXTENSIONS.contains(&s.extname.as_str()) {
                out.push(Problem {
                    code: Code::ExtensionNotAvailable,
                    message: format!("extension \"{}\" is not available", s.extname),
                    hint: Some(format!(
                        "The sandbox offers {}.",
                        ALLOWED_EXTENSIONS.join(", ")
                    )),
                    at: None,
                });
            }
        }
        N::CommentStmt(s) => {
            if !commentable(s.objtype()) {
                out.push(Problem::statement(hint_for_object(s.objtype())));
            }
        }
        N::RenameStmt(s) => {
            if !renamable(s.rename_type()) {
                out.push(Problem::statement(hint_for_object(s.rename_type())));
            }
        }
        N::AlterObjectSchemaStmt(s) => {
            if !movable(s.object_type()) {
                out.push(Problem::statement(hint_for_object(s.object_type())));
            }
        }
        N::DropStmt(s) => {
            if s.concurrent {
                out.push(Problem::not_allowed(
                    "DROP INDEX CONCURRENTLY is not allowed",
                    "Drop CONCURRENTLY: it cannot run inside the compile's transaction.",
                ));
            }
            if !droppable(s.remove_type()) {
                out.push(Problem::statement(hint_for_object(s.remove_type())));
            }
        }
        N::VariableSetStmt(s) => {
            let search_path = s.name.eq_ignore_ascii_case("search_path")
                && matches!(
                    s.kind(),
                    VariableSetKind::VarSetValue
                        | VariableSetKind::VarSetDefault
                        | VariableSetKind::VarReset
                );
            if !search_path {
                out.push(Problem::not_allowed(
                    "only SET search_path is allowed",
                    "Other settings do not change the schema, and the sandbox fixes its own.",
                ));
            }
        }
        N::DefineStmt(s) => {
            if s.kind() != ObjectType::ObjectCollation {
                out.push(Problem::statement(HINT_FUNCTIONS));
            }
        }
        // Allowed as they are.
        N::ViewStmt(_)
        | N::CreatePolicyStmt(_)
        | N::AlterPolicyStmt(_)
        | N::CreateSeqStmt(_)
        | N::AlterSeqStmt(_)
        | N::CreateEnumStmt(_)
        | N::AlterEnumStmt(_)
        | N::CreateDomainStmt(_)
        | N::AlterDomainStmt(_)
        | N::CompositeTypeStmt(_)
        | N::CreateRangeStmt(_)
        | N::CreateStatsStmt(_) => {}
        N::InsertStmt(_)
        | N::UpdateStmt(_)
        | N::DeleteStmt(_)
        | N::MergeStmt(_)
        | N::SelectStmt(_)
        | N::CopyStmt(_)
        | N::TruncateStmt(_)
        | N::RefreshMatViewStmt(_) => out.push(Problem::statement(HINT_DATA)),
        N::DoStmt(_)
        | N::CallStmt(_)
        | N::ExecuteStmt(_)
        | N::PrepareStmt(_)
        | N::DeallocateStmt(_)
        | N::DeclareCursorStmt(_)
        | N::FetchStmt(_)
        | N::ClosePortalStmt(_)
        | N::ExplainStmt(_)
        | N::ListenStmt(_)
        | N::UnlistenStmt(_)
        | N::NotifyStmt(_)
        | N::LockStmt(_)
        | N::LoadStmt(_)
        | N::DiscardStmt(_)
        | N::VariableShowStmt(_)
        | N::ConstraintsSetStmt(_) => out.push(Problem::statement(HINT_CODE)),
        N::TransactionStmt(_) => out.push(Problem::statement(HINT_TRANSACTION)),
        N::GrantStmt(_)
        | N::GrantRoleStmt(_)
        | N::AlterDefaultPrivilegesStmt(_)
        | N::AlterOwnerStmt(_)
        | N::ReassignOwnedStmt(_)
        | N::DropOwnedStmt(_)
        | N::SecLabelStmt(_) => out.push(Problem::statement(HINT_PRIVILEGES)),
        N::CreateFunctionStmt(_)
        | N::AlterFunctionStmt(_)
        | N::CreateTrigStmt(_)
        | N::CreateEventTrigStmt(_)
        | N::AlterEventTrigStmt(_)
        | N::RuleStmt(_)
        | N::CreateCastStmt(_)
        | N::CreateOpClassStmt(_)
        | N::CreateOpFamilyStmt(_)
        | N::AlterOpFamilyStmt(_)
        | N::AlterOperatorStmt(_)
        | N::CreateConversionStmt(_)
        | N::CreateTransformStmt(_)
        | N::CreatePlangStmt(_)
        | N::CreateAmStmt(_) => out.push(Problem::statement(HINT_FUNCTIONS)),
        N::CreateForeignTableStmt(_)
        | N::CreateFdwStmt(_)
        | N::AlterFdwStmt(_)
        | N::CreateForeignServerStmt(_)
        | N::AlterForeignServerStmt(_)
        | N::CreateUserMappingStmt(_)
        | N::AlterUserMappingStmt(_)
        | N::DropUserMappingStmt(_)
        | N::ImportForeignSchemaStmt(_) => out.push(Problem::statement(HINT_FOREIGN)),
        _ => out.push(Problem::statement(HINT_CLUSTER)),
    }
}

const HINT_FOREIGN: &str =
    "Foreign tables need a foreign server, which the sandbox does not have: remove the statement.";

fn check_alter_table_cmd(cmd: &pg_query::protobuf::AlterTableCmd, out: &mut Vec<Problem>) {
    use AlterTableType as T;
    match cmd.subtype() {
        T::AtChangeOwner => out.push(Problem::not_allowed(
            "ALTER … OWNER TO is not allowed",
            HINT_PRIVILEGES,
        )),
        T::AtSetTableSpace => out.push(Problem::not_allowed(
            "SET TABLESPACE is not allowed",
            "Tablespaces are not part of the schema model: remove the clause.",
        )),
        T::AtEnableTrig
        | T::AtEnableAlwaysTrig
        | T::AtEnableReplicaTrig
        | T::AtDisableTrig
        | T::AtEnableTrigAll
        | T::AtDisableTrigAll
        | T::AtEnableTrigUser
        | T::AtDisableTrigUser
        | T::AtEnableRule
        | T::AtEnableAlwaysRule
        | T::AtEnableReplicaRule
        | T::AtDisableRule => out.push(Problem::not_allowed(
            "enabling or disabling triggers and rules is not allowed",
            HINT_FUNCTIONS,
        )),
        T::AtGenericOptions | T::AtAlterColumnGenericOptions => {
            out.push(Problem::not_allowed("OPTIONS is not allowed", HINT_FOREIGN));
        }
        T::AtDetachPartitionFinalize => out.push(Problem::not_allowed(
            "DETACH PARTITION … FINALIZE is not allowed",
            "It cannot run inside the compile's transaction: detach without CONCURRENTLY.",
        )),
        T::AtAttachPartition | T::AtDetachPartition => {
            if let Some(NodeEnum::PartitionCmd(partition)) =
                cmd.def.as_ref().and_then(|d| d.node.as_ref())
            {
                if partition.concurrent {
                    out.push(Problem::not_allowed(
                        "DETACH PARTITION CONCURRENTLY is not allowed",
                        "It cannot run inside the compile's transaction: drop CONCURRENTLY.",
                    ));
                }
                if let Some(bound) = &partition.bound {
                    check_partition_bound(bound, out);
                }
            }
        }
        _ => {}
    }
}

/// Partition bounds are evaluated when the partition is created: constants
/// only (`MINVALUE`, `MAXVALUE` and casts of literals are fine).
fn check_partition_bound(bound: &pg_query::protobuf::PartitionBoundSpec, out: &mut Vec<Problem>) {
    let Ok(value) = serde_json::to_value(bound) else {
        return;
    };
    let mut first = None;
    walk::visit(&value, &mut |kind, fields| {
        if first.is_none() && matches!(kind, "FuncCall" | "SubLink") {
            first = Some(walk::location(fields));
        }
    });
    if let Some(at) = first {
        out.push(Problem {
            code: Code::NotAllowed,
            message: "partition bounds must be constants".to_owned(),
            hint: Some(
                "Postgres evaluates a bound when it creates the partition; write the value itself."
                    .to_owned(),
            ),
            at,
        });
    }
}

/// Denied and constant amplifying function calls, and temporary relations,
/// anywhere in the statement. Views, materialized views (created without
/// data) and policies are never evaluated while compiling, so amplifying
/// calls are fine there.
fn check_expressions(node: &NodeEnum, out: &mut Vec<Problem>) {
    let Ok(value) = serde_json::to_value(node) else {
        return;
    };
    let evaluated = !matches!(
        node,
        NodeEnum::ViewStmt(_)
            | NodeEnum::CreateTableAsStmt(_)
            | NodeEnum::CreatePolicyStmt(_)
            | NodeEnum::AlterPolicyStmt(_)
    );
    walk::visit(&value, &mut |kind, fields| {
        if kind != "FuncCall" {
            return;
        }
        let name = walk::func_name(fields);
        let builtin = match name.as_slice() {
            [name] => Some(name.as_str()),
            [schema, name] if schema == "pg_catalog" => Some(name.as_str()),
            _ => None,
        };
        let Some(name) = builtin else {
            return;
        };
        let at = walk::location(fields);
        if DENIED_FUNCTIONS.contains(&name) {
            out.push(Problem {
                code: Code::FunctionNotAllowed,
                message: format!("{name}() is not allowed"),
                hint: Some(
                    "It has effects outside the schema (waits, signals, locks, files, or SQL run from a string), so the sandbox never calls it."
                        .to_owned(),
                ),
                at,
            });
        } else if evaluated
            && AMPLIFYING_FUNCTIONS.contains(&name)
            && only_constants(&fields["args"])
        {
            out.push(Problem {
                code: Code::FunctionNotAllowed,
                message: format!("{name}() with only constant arguments is not allowed"),
                hint: Some(
                    "Postgres may evaluate it while compiling the schema, and its result can be huge: write the value itself, or pass a column."
                        .to_owned(),
                ),
                at,
            });
        }
    });
    walk::visit_objects(&value, &mut |object| {
        let temporary = object.contains_key("relname")
            && object.get("relpersistence").and_then(Value::as_str) == Some("t");
        if temporary {
            out.push(Problem {
                code: Code::NotAllowed,
                message: "temporary objects are not allowed".to_owned(),
                hint: Some(
                    "A temporary object is not part of a schema: drop TEMPORARY.".to_owned(),
                ),
                at: object
                    .get("location")
                    .and_then(Value::as_i64)
                    .and_then(|l| i32::try_from(l).ok())
                    .and_then(location),
            });
        }
    });
}

/// No column (or parameter) anywhere in the arguments.
fn only_constants(args: &Value) -> bool {
    let mut constant = true;
    walk::visit(args, &mut |kind, _| {
        if matches!(kind, "ColumnRef" | "ParamRef") {
            constant = false;
        }
    });
    constant
}

/// Identifiers naming temporary schemas or the sandbox's own objects.
fn check_identifiers(sql: &str, tokens: &[Token], out: &mut Vec<Problem>) {
    for token in tokens.iter().filter(|t| t.kind == TokenKind::Ident) {
        let raw = &sql[token.start..token.end];
        let name = raw
            .strip_prefix('"')
            .and_then(|r| r.strip_suffix('"'))
            .map_or_else(|| raw.to_lowercase(), |quoted| quoted.replace("\"\"", "\""));
        if let Some(prefix) = RESERVED_PREFIXES.iter().find(|p| name.starts_with(**p)) {
            let hint = if prefix.starts_with("pg_") {
                "Temporary and internal schemas are not part of a schema."
            } else {
                "Names starting with romulus_sandbox_ are reserved for the sandbox: rename it."
            };
            out.push(Problem {
                code: Code::NotAllowed,
                message: format!("{raw} is reserved"),
                hint: Some(hint.to_owned()),
                at: Some(token.start),
            });
        }
    }
}

/// `INSERT`, `CREATE FUNCTION`, `COMMENT ON DATABASE`…: the leading
/// keywords of a statement, upper-cased, for messages.
fn label(sql: &str, tokens: &[Token]) -> String {
    const MODIFIERS: &[&str] = &[
        "OR",
        "REPLACE",
        "TEMP",
        "TEMPORARY",
        "UNLOGGED",
        "GLOBAL",
        "LOCAL",
        "UNIQUE",
        "IF",
        "NOT",
        "EXISTS",
        "ONLY",
        "CONCURRENTLY",
        "TRUSTED",
        "PROCEDURAL",
        "RECURSIVE",
    ];
    // Object kinds spelled with two words.
    const COMPOUND: &[&str] = &[
        "MATERIALIZED",
        "FOREIGN",
        "EVENT",
        "DEFAULT",
        "TEXT",
        "USER",
        "OPERATOR",
        "ACCESS",
    ];
    let mut code = tokens.iter().filter(|t| !t.is_comment());
    let Some(first) = code.next() else {
        return "this statement".to_owned();
    };
    let word = |t: &Token| sql[t.start..t.end].to_uppercase();
    let mut words = vec![word(first)];
    let wanted = match words[0].as_str() {
        "CREATE" | "ALTER" | "DROP" => 1,
        "COMMENT" | "SECURITY" | "REFRESH" | "IMPORT" => 2,
        _ => 0,
    };
    let mut taken = 0;
    for token in code.take(6) {
        if taken >= wanted || !token.keyword {
            break;
        }
        let w = word(token);
        if MODIFIERS.contains(&w.as_str()) {
            continue;
        }
        let compound = COMPOUND.contains(&w.as_str());
        words.push(w);
        if !compound {
            taken += 1;
        }
    }
    words.join(" ")
}

fn commentable(objtype: ObjectType) -> bool {
    use ObjectType as O;
    matches!(
        objtype,
        O::ObjectTable
            | O::ObjectColumn
            | O::ObjectView
            | O::ObjectMatview
            | O::ObjectIndex
            | O::ObjectSequence
            | O::ObjectType
            | O::ObjectDomain
            | O::ObjectDomconstraint
            | O::ObjectTabconstraint
            | O::ObjectSchema
            | O::ObjectPolicy
            | O::ObjectExtension
            | O::ObjectCollation
            | O::ObjectStatisticExt
    )
}

fn renamable(objtype: ObjectType) -> bool {
    use ObjectType as O;
    matches!(
        objtype,
        O::ObjectTable
            | O::ObjectView
            | O::ObjectMatview
            | O::ObjectIndex
            | O::ObjectSequence
            | O::ObjectColumn
            | O::ObjectAttribute
            | O::ObjectTabconstraint
            | O::ObjectDomconstraint
            | O::ObjectType
            | O::ObjectDomain
            | O::ObjectSchema
            | O::ObjectPolicy
            | O::ObjectCollation
            | O::ObjectStatisticExt
    )
}

fn movable(objtype: ObjectType) -> bool {
    use ObjectType as O;
    matches!(
        objtype,
        O::ObjectTable
            | O::ObjectView
            | O::ObjectMatview
            | O::ObjectSequence
            | O::ObjectType
            | O::ObjectDomain
            | O::ObjectExtension
            | O::ObjectCollation
            | O::ObjectStatisticExt
    )
}

fn droppable(objtype: ObjectType) -> bool {
    use ObjectType as O;
    matches!(
        objtype,
        O::ObjectTable
            | O::ObjectView
            | O::ObjectMatview
            | O::ObjectIndex
            | O::ObjectSequence
            | O::ObjectType
            | O::ObjectDomain
            | O::ObjectSchema
            | O::ObjectPolicy
            | O::ObjectExtension
            | O::ObjectCollation
            | O::ObjectStatisticExt
    )
}

/// The hint for an object kind outside the allowlist.
fn hint_for_object(objtype: ObjectType) -> &'static str {
    use ObjectType as O;
    match objtype {
        O::ObjectFunction
        | O::ObjectProcedure
        | O::ObjectRoutine
        | O::ObjectAggregate
        | O::ObjectTrigger
        | O::ObjectRule
        | O::ObjectEventTrigger
        | O::ObjectOperator
        | O::ObjectCast
        | O::ObjectLanguage
        | O::ObjectTransform => HINT_FUNCTIONS,
        O::ObjectForeignTable | O::ObjectForeignServer | O::ObjectFdw | O::ObjectUserMapping => {
            HINT_FOREIGN
        }
        O::ObjectLargeobject => HINT_DATA,
        _ => HINT_CLUSTER,
    }
}

#[cfg(test)]
mod tests {
    use super::{check, check_statement};
    use crate::{diagnostic::Code, parse, parse::Statement};

    fn allowed(statement: &Statement) -> bool {
        let mut problems = Vec::new();
        check_statement(&statement.node, &mut problems);
        problems.is_empty()
    }

    fn problems(sql: &str) -> Vec<(Code, String, String)> {
        let parsed = parse(sql);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        check(&parsed, sql)
            .into_iter()
            .map(|d| {
                let span = d.span.unwrap();
                (d.code, d.message, sql[span.range()].to_owned())
            })
            .collect()
    }

    #[test]
    fn schema_ddl_is_allowed() {
        let sql = r"
            CREATE SCHEMA app;
            CREATE SCHEMA app2 CREATE TABLE t (id int) CREATE VIEW v AS SELECT 1;
            CREATE EXTENSION IF NOT EXISTS pgcrypto WITH SCHEMA public;
            CREATE EXTENSION postgis_topology CASCADE;
            CREATE TYPE app.mood AS ENUM ('ok', 'sad');
            ALTER TYPE app.mood ADD VALUE 'meh' AFTER 'ok';
            CREATE TYPE app.address AS (street text);
            CREATE TYPE app.span AS RANGE (subtype = int4);
            CREATE DOMAIN app.email AS text CHECK (VALUE ~ '@');
            ALTER DOMAIN app.email SET NOT NULL;
            CREATE SEQUENCE IF NOT EXISTS app.seq;
            CREATE UNLOGGED TABLE app.users (id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY, email app.email);
            CREATE TABLE app.events (at timestamptz) PARTITION BY RANGE (at);
            CREATE TABLE app.events_2026 PARTITION OF app.events FOR VALUES FROM ('2026-01-01') TO (MAXVALUE);
            ALTER SEQUENCE app.seq OWNED BY app.users.id;
            ALTER TABLE app.users ADD COLUMN bio text DEFAULT '', ALTER COLUMN email SET NOT NULL;
            ALTER TABLE app.users ADD CONSTRAINT users_email_key UNIQUE (email);
            ALTER TABLE app.users ENABLE ROW LEVEL SECURITY, FORCE ROW LEVEL SECURITY;
            ALTER TABLE app.events DETACH PARTITION app.events_2026;
            ALTER TABLE app.events ATTACH PARTITION app.events_2026 FOR VALUES FROM (MINVALUE) TO ('2027-01-01');
            CREATE INDEX ON app.users (lower(email));
            CREATE VIEW app.v AS SELECT id FROM app.users;
            CREATE MATERIALIZED VIEW app.mv AS SELECT count(*) FROM app.users;
            CREATE POLICY own ON app.users TO app_user USING (id = current_setting('app.id')::bigint);
            ALTER POLICY own ON app.users TO PUBLIC;
            COMMENT ON TABLE app.users IS 'People';
            COMMENT ON COLUMN app.users.email IS 'Login';
            COMMENT ON SCHEMA app IS 'App';
            CREATE STATISTICS app.st ON id, email FROM app.users;
            CREATE COLLATION app.ci (provider = icu, locale = 'und-u-ks-level2', deterministic = false);
            ALTER TABLE app.users RENAME COLUMN bio TO about;
            ALTER TABLE app.users SET SCHEMA public;
            DROP VIEW app.v;
            DROP INDEX app.users_lower_idx;
            SET search_path TO app, public;
            RESET search_path;
        ";
        assert_eq!(problems(sql), []);
        assert!(parse(sql).statements.iter().all(allowed));
    }

    #[test]
    fn dml_and_procedural_statements_are_refused_with_their_name() {
        let found = problems(
            "INSERT INTO t VALUES (1);
             UPDATE t SET a = 1;
             DELETE FROM t;
             SELECT 1;
             COPY t FROM STDIN;
             TRUNCATE t;
             DO $$ BEGIN PERFORM 1; END $$;
             CALL p();
             BEGIN;
             COMMIT;
             LOCK TABLE t;
             VACUUM t;
             GRANT SELECT ON t TO PUBLIC;
             REVOKE ALL ON t FROM PUBLIC;
             CREATE OR REPLACE FUNCTION f() RETURNS int LANGUAGE sql AS 'SELECT 1';
             CREATE TRIGGER trg BEFORE INSERT ON t FOR EACH ROW EXECUTE FUNCTION f();
             CREATE ROLE r;
             CREATE DATABASE d;
             ALTER SYSTEM SET work_mem = '1GB';
             CREATE FOREIGN TABLE ft (a int) SERVER s;
             REFRESH MATERIALIZED VIEW mv;
             CREATE TABLE t2 AS SELECT 1;
             SELECT 1 INTO t3;",
        );
        let messages: Vec<&str> = found.iter().map(|(_, m, _)| m.as_str()).collect();
        assert_eq!(
            messages,
            [
                "INSERT is not allowed",
                "UPDATE is not allowed",
                "DELETE is not allowed",
                "SELECT is not allowed",
                "COPY is not allowed",
                "TRUNCATE is not allowed",
                "DO is not allowed",
                "CALL is not allowed",
                "BEGIN is not allowed",
                "COMMIT is not allowed",
                "LOCK is not allowed",
                "VACUUM is not allowed",
                "GRANT is not allowed",
                "REVOKE is not allowed",
                "CREATE FUNCTION is not allowed",
                "CREATE TRIGGER is not allowed",
                "CREATE ROLE is not allowed",
                "CREATE DATABASE is not allowed",
                "ALTER SYSTEM is not allowed",
                "CREATE FOREIGN TABLE is not allowed",
                "REFRESH MATERIALIZED VIEW is not allowed",
                "CREATE TABLE … AS is not allowed",
                // SELECT … INTO parses as a SELECT.
                "SELECT is not allowed",
            ]
        );
        assert!(found.iter().all(|(code, _, _)| *code == Code::NotAllowed));
        // Each error covers its own statement.
        assert_eq!(found[0].2, "INSERT INTO t VALUES (1)");
        assert_eq!(found[6].2, "DO $$ BEGIN PERFORM 1; END $$");
    }

    #[test]
    fn clauses_outside_the_model_are_refused() {
        let found = problems(
            "CREATE SCHEMA s AUTHORIZATION joe;
             ALTER TABLE t OWNER TO joe;
             ALTER TABLE t SET TABLESPACE fast;
             ALTER TABLE t DISABLE TRIGGER ALL;
             CREATE TABLE t (a int) TABLESPACE fast;
             CREATE INDEX CONCURRENTLY i ON t (a);
             DROP INDEX CONCURRENTLY i;
             CREATE TEMP TABLE tmp (a int);
             CREATE TABLE pg_temp.tmp (a int);
             SET statement_timeout = 0;
             SET ROLE postgres;
             COMMENT ON FUNCTION f() IS 'x';
             COMMENT ON DATABASE d IS 'x';
             DROP FUNCTION f();
             ALTER FUNCTION f() RENAME TO g;
             CREATE EXTENSION dblink;
             CREATE EXTENSION tablefunc;",
        );
        let messages: Vec<&str> = found.iter().map(|(_, m, _)| m.as_str()).collect();
        assert_eq!(
            messages,
            [
                "CREATE SCHEMA … AUTHORIZATION is not allowed",
                "ALTER … OWNER TO is not allowed",
                "SET TABLESPACE is not allowed",
                "enabling or disabling triggers and rules is not allowed",
                "TABLESPACE is not allowed",
                "CREATE INDEX CONCURRENTLY is not allowed",
                "DROP INDEX CONCURRENTLY is not allowed",
                "temporary objects are not allowed",
                "pg_temp is reserved",
                "only SET search_path is allowed",
                "only SET search_path is allowed",
                "COMMENT ON FUNCTION is not allowed",
                "COMMENT ON DATABASE is not allowed",
                "DROP FUNCTION is not allowed",
                "ALTER FUNCTION is not allowed",
                "extension \"dblink\" is not available",
                "extension \"tablefunc\" is not available",
            ]
        );
        let reserved = &found[8];
        assert_eq!(reserved.2, "pg_temp");
        assert_eq!(found[15].0, Code::ExtensionNotAvailable);
    }

    #[test]
    fn dangerous_functions_are_refused_where_they_are_called() {
        let found = problems(
            "CREATE TABLE t (a int DEFAULT pg_sleep(10)::int);
             CREATE TABLE u (a int CHECK (pg_catalog.pg_terminate_backend(a)));
             CREATE VIEW v AS SELECT query_to_xml('DELETE FROM t', true, false, '');
             ALTER TABLE t ADD COLUMN b text DEFAULT repeat('x', 1000000000);
             CREATE INDEX ON t ((lpad('', 100000000, 'x') || a));
             CREATE TABLE w (a text DEFAULT replace(replace('x', 'x', 'xxxxxxxxxx'), 'x', 'xxxxxxxxxx'));",
        );
        let messages: Vec<(&str, &str)> = found
            .iter()
            .map(|(_, m, at)| (m.as_str(), at.as_str()))
            .collect();
        assert_eq!(
            messages,
            [
                ("pg_sleep() is not allowed", "pg_sleep"),
                ("pg_terminate_backend() is not allowed", "pg_catalog"),
                ("query_to_xml() is not allowed", "query_to_xml"),
                (
                    "repeat() with only constant arguments is not allowed",
                    "repeat"
                ),
                ("lpad() with only constant arguments is not allowed", "lpad"),
                (
                    "replace() with only constant arguments is not allowed",
                    "replace"
                ),
                (
                    "replace() with only constant arguments is not allowed",
                    "replace"
                ),
            ]
        );
        assert!(found.iter().all(|(c, _, _)| *c == Code::FunctionNotAllowed));
    }

    #[test]
    fn amplifying_functions_over_columns_and_schema_qualified_lookalikes_are_fine() {
        assert_eq!(
            problems(
                "CREATE TABLE t (code text, padded text GENERATED ALWAYS AS (lpad(code, 8, '0')) STORED);
                 CREATE TABLE u (a int DEFAULT app.pg_sleep(1));
                 CREATE INDEX ON t (replace(code, '-', ''));
                 CREATE VIEW calendar AS SELECT d::date, repeat('-', 10) FROM generate_series('2026-01-01'::date, '2026-12-31', '1 day') d;
                 CREATE POLICY p ON t USING (code <> repeat('x', 3));"
            ),
            []
        );
    }

    #[test]
    fn partition_bounds_must_be_constants() {
        let found = problems(
            "CREATE TABLE p1 PARTITION OF p FOR VALUES IN (now()::date);
             ALTER TABLE p ATTACH PARTITION p2 FOR VALUES FROM ((SELECT 1)) TO (10);",
        );
        let messages: Vec<(&str, &str)> = found
            .iter()
            .map(|(_, m, at)| (m.as_str(), at.as_str()))
            .collect();
        assert_eq!(
            messages,
            [
                ("partition bounds must be constants", "now"),
                ("partition bounds must be constants", "("),
            ]
        );
    }

    #[test]
    fn the_sandbox_session_objects_are_reserved() {
        let found = problems(
            "DROP SEQUENCE romulus_sandbox_progress; CREATE TABLE \"pg_toast_x\" (a int);",
        );
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].1, "romulus_sandbox_progress is reserved");
        assert_eq!(found[1].2, "\"pg_toast_x\"");
    }
}
